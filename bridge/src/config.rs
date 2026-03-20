//! Configuration and credential loading for the bridge API.

use serde::Deserialize;
use std::path::{Path, PathBuf};

const DEFAULT_API_URL: &str = "https://api.anthropic.com";

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub base_url: String,
    pub access_token: String,
    pub org_uuid: String,
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
        let access_token = creds
            .claude_ai_oauth
            .ok_or(ConfigError::MissingField("claudeAiOauth"))?
            .access_token;

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
        })
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
