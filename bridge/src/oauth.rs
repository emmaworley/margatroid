//! Direct OAuth refresh against Claude's token endpoint.
//!
//! Shelling out to `claude -p hi --max-turns 1` does NOT refresh the access
//! token when it's already expired — the CLI just exits with status 1 and the
//! credentials file is left untouched. Observed in production: with an
//! expired token, `claude -p hi` returned code=1, the daemon re-read the same
//! stale credentials, and main() exited 401, causing systemd to crash-loop
//! the service overnight.
//!
//! This module talks to Claude's OAuth token endpoint directly, exchanging
//! the stored refresh_token for a fresh access_token (and a rotated refresh
//! token), then atomically rewrites `~/.claude/.credentials.json`.
//!
//! The endpoint URL, client ID, and expected response shape were extracted
//! from the Claude Code CLI binary (v2.1.105):
//!
//!   - Token URL: `https://platform.claude.com/v1/oauth/token`
//!   - Client ID: `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
//!   - Response fields: `access_token`, `refresh_token`, `expires_in`
//!
//! If the Claude team ever rotates the client ID or moves the endpoint,
//! refresh will fail with a 4xx response and the daemon will surface the
//! error in its logs; we'll want to re-extract from the new binary.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const DEFAULT_SCOPES: &[&str] = &[
    "user:file_upload",
    "user:inference",
    "user:mcp_servers",
    "user:profile",
    "user:sessions:claude_code",
];

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("cannot determine home directory")]
    NoHome,

    #[error("reading credentials {0}: {1}")]
    Read(PathBuf, std::io::Error),

    #[error("writing credentials {0}: {1}")]
    Write(PathBuf, std::io::Error),

    #[error("parsing credentials JSON: {0}")]
    Parse(serde_json::Error),

    #[error("credentials missing claudeAiOauth section")]
    MissingOauthSection,

    #[error("credentials missing refreshToken")]
    MissingRefreshToken,

    #[error("refresh HTTP error: {0}")]
    Http(reqwest::Error),

    #[error("refresh endpoint returned {status}: {body}")]
    Api { status: u16, body: String },
}

/// Refresh the OAuth access token stored in `~/.claude/.credentials.json`.
///
/// Reads the current `refreshToken`, POSTs it to Claude's token endpoint,
/// and writes the response (new `accessToken`, new `refreshToken`, new
/// `expiresAt`) back to the credentials file atomically.
///
/// Returns the new expiry as milliseconds since epoch on success.
pub async fn refresh_credentials() -> Result<u64, OAuthError> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| OAuthError::NoHome)?;
    let creds_path = home.join(".claude/.credentials.json");

    // Load current credentials (preserving any fields we don't recognize).
    let raw = std::fs::read(&creds_path)
        .map_err(|e| OAuthError::Read(creds_path.clone(), e))?;
    let mut creds: serde_json::Value =
        serde_json::from_slice(&raw).map_err(OAuthError::Parse)?;

    let oauth_section = creds
        .get("claudeAiOauth")
        .ok_or(OAuthError::MissingOauthSection)?;
    let refresh_token = oauth_section
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .ok_or(OAuthError::MissingRefreshToken)?
        .to_string();

    // Preserve the scopes the existing credentials already had (falling back
    // to the default set if absent), so we don't accidentally narrow them.
    let scopes: Vec<String> = oauth_section
        .get("scopes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect());
    let scope_string = scopes.join(" ");

    // POST to the token endpoint.
    let http = reqwest::Client::new();
    let req_body = RefreshRequest {
        grant_type: "refresh_token",
        refresh_token: &refresh_token,
        client_id: CLIENT_ID,
        scope: &scope_string,
    };
    let resp = http
        .post(TOKEN_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&req_body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(OAuthError::Http)?;

    let status = resp.status().as_u16();
    if status >= 400 {
        let body = resp.text().await.unwrap_or_default();
        return Err(OAuthError::Api { status, body });
    }

    let token: TokenResponse = resp.json().await.map_err(OAuthError::Http)?;

    // Compute new expiresAt (milliseconds since epoch).
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let expires_at_ms = now_ms + token.expires_in.saturating_mul(1000);

    // Mutate the credentials JSON in place, preserving every other field.
    if let Some(section) = creds.get_mut("claudeAiOauth").and_then(|v| v.as_object_mut()) {
        section.insert(
            "accessToken".to_string(),
            serde_json::Value::String(token.access_token),
        );
        if let Some(new_refresh) = token.refresh_token {
            section.insert(
                "refreshToken".to_string(),
                serde_json::Value::String(new_refresh),
            );
        }
        section.insert(
            "expiresAt".to_string(),
            serde_json::Value::Number(expires_at_ms.into()),
        );
    } else {
        // Should be impossible — we already got the section above — but guard
        // anyway so we never clobber the file with half-updated state.
        return Err(OAuthError::MissingOauthSection);
    }

    // Write atomically (tmp+rename) to avoid partial writes if anything else
    // reads this file while we're mid-write.
    atomic_write_json(&creds_path, &creds)?;

    Ok(expires_at_ms)
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    grant_type: &'static str,
    refresh_token: &'a str,
    client_id: &'static str,
    scope: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
}

fn atomic_write_json(path: &Path, value: &serde_json::Value) -> Result<(), OAuthError> {
    let parent = path.parent().ok_or_else(|| {
        OAuthError::Write(
            path.to_path_buf(),
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"),
        )
    })?;
    let tmp_path = parent.join(format!(
        ".credentials.json.tmp.{}",
        std::process::id()
    ));
    let serialized = serde_json::to_vec(value).map_err(|e| {
        OAuthError::Write(
            tmp_path.clone(),
            std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        )
    })?;
    // Write, set perms, rename.
    std::fs::write(&tmp_path, &serialized)
        .map_err(|e| OAuthError::Write(tmp_path.clone(), e))?;
    // Preserve 0600 permissions (match what Claude Code itself uses).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp_path, path)
        .map_err(|e| OAuthError::Write(path.to_path_buf(), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_roundtrip() {
        let tmp = tempdir_like();
        let path = tmp.join(".credentials.json");
        let v = serde_json::json!({"claudeAiOauth": {"accessToken": "a", "refreshToken": "b", "expiresAt": 1_u64}});
        atomic_write_json(&path, &v).unwrap();
        let back: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, v);
        // Make sure no leftover temp file.
        let entries: Vec<_> = std::fs::read_dir(&tmp)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(entries, vec![".credentials.json".to_string()]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn tempdir_like() -> PathBuf {
        let p = std::env::temp_dir().join(format!("bridge-oauth-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
