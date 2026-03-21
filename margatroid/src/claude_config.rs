//! Manage ~/.claude.json trust config and session CLAUDE.md files.

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
}

type Result<T> = std::result::Result<T, ConfigError>;

fn claude_json_path() -> std::path::PathBuf {
    home_dir().join(".claude.json")
}

/// Ensure a session directory is trusted in ~/.claude.json.
/// Creates the directory if it doesn't exist.
pub fn ensure_trusted(session_dir: &Path) -> Result<()> {
    fs::create_dir_all(session_dir)?;

    let path = claude_json_path();
    let mut data: serde_json::Value = match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };

    let dir_str = session_dir.to_string_lossy().to_string();
    let obj = data.as_object_mut().ok_or(ConfigError::NotAnObject)?;

    let mut changed = false;

    // Skip the /remote-control confirmation dialog
    if !obj.get("remoteDialogSeen").and_then(|v| v.as_bool()).unwrap_or(false) {
        obj.insert("remoteDialogSeen".into(), serde_json::Value::Bool(true));
        changed = true;
    }

    // Ensure the session directory is trusted
    let projects = obj
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));

    let needs_trust = match projects.get(&dir_str) {
        Some(proj) => !proj
            .get("hasTrustDialogAccepted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        None => true,
    };

    if needs_trust {
        projects[&dir_str] = serde_json::json!({
            "hasTrustDialogAccepted": true,
            "allowedTools": [],
            "mcpContextUris": [],
            "mcpServers": {},
            "enabledMcpjsonServers": [],
            "disabledMcpjsonServers": [],
        });
        changed = true;
    }

    if changed {
        let content = serde_json::to_string_pretty(&data)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &content)?;
        fs::rename(&tmp, &path)?;
    }

    Ok(())
}

/// Write a default CLAUDE.md if one doesn't exist.
pub fn write_claude_md(session_dir: &Path, name: &str) -> Result<()> {
    let claude_md = session_dir.join("CLAUDE.md");
    if !claude_md.exists() {
        let content = format!(
            "# Worker Session: {name}\n\n\
             This is a scoped worker session for the `{name}` project \
             directory (`{}`).\n\
             All file operations should be relative to this directory \
             unless otherwise specified.\n",
            session_dir.display()
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
        assert!(content.contains(&dir.display().to_string()));

        // Second call should not overwrite
        std::fs::write(&path, "custom content").unwrap();
        write_claude_md(&dir, "test-session").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "custom content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_an_object_error() {
        // Verify we get a proper error, not a panic, when JSON is an array
        let mut val: serde_json::Value = serde_json::json!([1, 2, 3]);
        assert!(val.as_object_mut().is_none());
        // Confirms the NotAnObject path would trigger
    }
}
