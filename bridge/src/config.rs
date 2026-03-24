//! Configuration and credential loading for the bridge API.

use serde::Deserialize;
use std::path::{Path, PathBuf};

const DEFAULT_API_URL: &str = "https://api.anthropic.com";

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub base_url: String,
    pub access_token: String,
    pub org_uuid: String,
    /// Token expiry (milliseconds since epoch), if known.
    pub expires_at_ms: Option<u64>,
}

impl BridgeConfig {
    /// Load configuration from the standard Claude Code credential files.
    ///
    /// Reads:
    /// - `~/.claude/.credentials.json` for the OAuth access token
    /// - `~/.claude.json` for the organization UUID
    ///
    /// The base API URL defaults to `https://api.anthropic.com` but can be
    /// overridden via the `CLAUDE_API_BASE_URL` environment variable.
    pub fn from_default_files() -> Result<Self, ConfigError> {
        let home = home_dir()?;

        let creds_path = home.join(".claude/.credentials.json");
        let creds: CredentialsFile =
            read_json(&creds_path).map_err(|e| ConfigError::Read(creds_path.clone(), e))?;
        let oauth = creds
            .claude_ai_oauth
            .ok_or(ConfigError::MissingField("claudeAiOauth"))?;
        let access_token = oauth.access_token;
        let expires_at_ms = oauth.expires_at;

        let config_path = home.join(".claude.json");
        let config: ConfigFile =
            read_json(&config_path).map_err(|e| ConfigError::Read(config_path.clone(), e))?;
        let org_uuid = config
            .oauth_account
            .and_then(|a| a.organization_uuid)
            .or_else(|| std::env::var("CLAUDE_CODE_ORGANIZATION_UUID").ok())
            .ok_or(ConfigError::MissingField("oauthAccount.organizationUuid"))?;

        let base_url = std::env::var("CLAUDE_API_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_API_URL.into());

        Ok(Self {
            base_url,
            access_token,
            org_uuid,
            expires_at_ms,
        })
    }

    /// Re-read the access token (and expiry) from disk.
    ///
    /// Returns the new token and expiry, or an error if the file can't be read.
    pub fn reload_access_token(&mut self) -> Result<(), ConfigError> {
        let home = home_dir()?;
        let creds_path = home.join(".claude/.credentials.json");
        let creds: CredentialsFile =
            read_json(&creds_path).map_err(|e| ConfigError::Read(creds_path.clone(), e))?;
        let oauth = creds
            .claude_ai_oauth
            .ok_or(ConfigError::MissingField("claudeAiOauth"))?;
        self.access_token = oauth.access_token;
        self.expires_at_ms = oauth.expires_at;
        Ok(())
    }

    /// Milliseconds until the access token expires, or 0 if unknown/expired.
    pub fn token_ttl_ms(&self) -> u64 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.expires_at_ms
            .unwrap_or(0)
            .saturating_sub(now_ms)
    }
}

// ---------------------------------------------------------------------------
// File structures (serde)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OAuthCredentials>,
}

#[derive(Deserialize)]
struct OAuthCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "expiresAt")]
    expires_at: Option<u64>,
}

#[derive(Deserialize)]
struct ConfigFile {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OAuthAccount>,
}

#[derive(Deserialize)]
struct OAuthAccount {
    #[serde(rename = "organizationUuid")]
    organization_uuid: Option<String>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot determine home directory")]
    NoHome,

    #[error("reading {0}: {1}")]
    Read(PathBuf, std::io::Error),

    #[error("missing config field: {0}")]
    MissingField(&'static str),
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn home_dir() -> Result<PathBuf, ConfigError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| ConfigError::NoHome)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, std::io::Error> {
    let data = std::fs::read(path)?;
    serde_json::from_slice(&data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_ttl_ms_future_expiry() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let config = BridgeConfig {
            base_url: String::new(),
            access_token: String::new(),
            org_uuid: String::new(),
            expires_at_ms: Some(now_ms + 3_600_000), // 1 hour from now
        };
        let ttl = config.token_ttl_ms();
        // Should be close to 1 hour (allow 5s slack for test execution)
        assert!(ttl > 3_595_000, "ttl={ttl} should be ~3600000");
        assert!(ttl <= 3_600_000);
    }

    #[test]
    fn token_ttl_ms_expired() {
        let config = BridgeConfig {
            base_url: String::new(),
            access_token: String::new(),
            org_uuid: String::new(),
            expires_at_ms: Some(1_000), // epoch + 1s, long expired
        };
        assert_eq!(config.token_ttl_ms(), 0);
    }

    #[test]
    fn token_ttl_ms_unknown() {
        let config = BridgeConfig {
            base_url: String::new(),
            access_token: String::new(),
            org_uuid: String::new(),
            expires_at_ms: None,
        };
        assert_eq!(config.token_ttl_ms(), 0);
    }

    #[test]
    fn parse_credentials_with_expires_at() {
        let json = r#"{"claudeAiOauth":{"accessToken":"tok-123","expiresAt":1774417855356}}"#;
        let creds: CredentialsFile = serde_json::from_str(json).unwrap();
        let oauth = creds.claude_ai_oauth.unwrap();
        assert_eq!(oauth.access_token, "tok-123");
        assert_eq!(oauth.expires_at, Some(1774417855356));
    }

    #[test]
    fn parse_credentials_without_expires_at() {
        let json = r#"{"claudeAiOauth":{"accessToken":"tok-456"}}"#;
        let creds: CredentialsFile = serde_json::from_str(json).unwrap();
        let oauth = creds.claude_ai_oauth.unwrap();
        assert_eq!(oauth.access_token, "tok-456");
        assert_eq!(oauth.expires_at, None);
    }
}
