//! Manage Claude Code config files for margatroid sessions.
//!
//! - Host ~/.claude.json: set remoteDialogSeen so /remote-control doesn't prompt
//! - Per-session .claude.json: seeded with trust entry, remoteDialogSeen, and org UUID
//! - Per-session .claude/ directory: created for credentials mount target

use crate::home_dir;
use std::fs;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("~/.claude.json is not a JSON object")]
    NotAnObject,
    #[error("missing config field: {0}")]
    MissingField(&'static str),
}

type Result<T> = std::result::Result<T, ConfigError>;

fn host_claude_json_path() -> std::path::PathBuf {
    home_dir().join(".claude.json")
}

/// Set up all Claude Code config for a session.
///
/// - Creates the session directory
/// - Ensures `remoteDialogSeen` is set in the host ~/.claude.json
/// - Creates a per-session .claude.json with trust, remoteDialogSeen, and org UUID
/// - Creates .claude/ directory inside the session dir (mount target for credentials)
/// - Writes a default CLAUDE.md if one doesn't exist
pub fn setup_session(session_dir: &Path, name: &str, container_home: &str, host_mode: bool) -> Result<()> {
    fs::create_dir_all(session_dir)?;

    let session_dir_str = session_dir.to_string_lossy().to_string();

    if host_mode {
        // Host mode: Claude Code runs with the real home dir, so trust
        // the session dir in the host's ~/.claude.json directly.
        ensure_host_config(Some(&session_dir_str))?;
    } else {
        // Container mode: trust goes in per-session config (container sees
        // /home/<name> as its home), host config just needs remoteDialogSeen.
        ensure_host_config(None)?;

        let org_uuid = read_org_uuid()?;
        write_session_config(session_dir, container_home, &org_uuid)?;

        // Create .claude/ directory for credentials mount
        fs::create_dir_all(session_dir.join(".claude"))?;
    }

    // Write CLAUDE.md
    write_claude_md(session_dir, name)?;

    Ok(())
}

/// Ensure `remoteDialogSeen: true` and optionally trust a directory in the host's ~/.claude.json.
fn ensure_host_config(trust_dir: Option<&str>) -> Result<()> {
    let path = host_claude_json_path();
    let mut data: serde_json::Value = match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };

    let obj = data.as_object_mut().ok_or(ConfigError::NotAnObject)?;
    let mut changed = false;

    if !obj.get("remoteDialogSeen").and_then(|v| v.as_bool()).unwrap_or(false) {
        obj.insert("remoteDialogSeen".into(), serde_json::Value::Bool(true));
        changed = true;
    }

    if let Some(dir) = trust_dir {
        let projects = obj
            .entry("projects")
            .or_insert_with(|| serde_json::json!({}));
        let needs_trust = !projects
            .get(dir)
            .and_then(|p| p.get("hasTrustDialogAccepted"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if needs_trust {
            projects[dir] = serde_json::json!({
                "hasTrustDialogAccepted": true,
                "allowedTools": [],
                "mcpContextUris": [],
                "mcpServers": {},
                "enabledMcpjsonServers": [],
                "disabledMcpjsonServers": [],
            });
            changed = true;
        }
    }

    if changed {
        let content = serde_json::to_string_pretty(&data)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &content)?;
        fs::rename(&tmp, &path)?;
    }

    Ok(())
}

/// Read the organization UUID from the host's ~/.claude.json.
fn read_org_uuid() -> Result<String> {
    let path = host_claude_json_path();
    let data: serde_json::Value = match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };

    data.get("oauthAccount")
        .and_then(|a| a.get("organizationUuid"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| std::env::var("CLAUDE_CODE_ORGANIZATION_UUID").ok())
        .ok_or(ConfigError::MissingField("oauthAccount.organizationUuid in ~/.claude.json"))
}

/// Write a per-session .claude.json with minimal config for trust and remote control.
fn write_session_config(session_dir: &Path, container_home: &str, org_uuid: &str) -> Result<()> {
    let session_config_path = session_dir.join(".claude.json");

    // The trust path is what Claude Code sees as its working directory inside the container
    let trust_path = container_home.to_string();

    let config = serde_json::json!({
        "remoteDialogSeen": true,
        "hasCompletedOnboarding": true,
        "oauthAccount": {
            "organizationUuid": org_uuid,
        },
        "projects": {
            trust_path: {
                "hasTrustDialogAccepted": true,
                "allowedTools": [],
                "mcpContextUris": [],
                "mcpServers": {},
                "enabledMcpjsonServers": [],
                "disabledMcpjsonServers": [],
            }
        }
    });

    let content = serde_json::to_string_pretty(&config)?;
    let tmp = session_config_path.with_extension("json.tmp");
    fs::write(&tmp, &content)?;
    fs::rename(&tmp, &session_config_path)?;

    Ok(())
}

/// Write a default CLAUDE.md if one doesn't exist.
fn write_claude_md(session_dir: &Path, name: &str) -> Result<()> {
    let claude_md = session_dir.join("CLAUDE.md");
    if !claude_md.exists() {
        let content = format!(
            "# Worker Session: {name}\n\n\
             This is a scoped worker session for the `{name}` project.\n\
             All file operations should be relative to this directory \
             unless otherwise specified.\n",
        );
        fs::write(&claude_md, content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_claude_md_creates_file() {
        let dir =
            std::env::temp_dir().join(format!("orch-test-claude-md-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        write_claude_md(&dir, "test-session").unwrap();

        let path = dir.join("CLAUDE.md");
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("test-session"));

        // Second call should not overwrite
        std::fs::write(&path, "custom content").unwrap();
        write_claude_md(&dir, "test-session").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "custom content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_session_config_creates_file() {
        let dir =
            std::env::temp_dir().join(format!("orch-test-session-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        write_session_config(&dir, "/home/testbox", "org-123").unwrap();

        let path = dir.join(".claude.json");
        assert!(path.exists());

        let data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert_eq!(data["remoteDialogSeen"], true);
        assert_eq!(data["hasCompletedOnboarding"], true);
        assert_eq!(data["oauthAccount"]["organizationUuid"], "org-123");
        assert_eq!(data["projects"]["/home/testbox"]["hasTrustDialogAccepted"], true);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
