//! Image resolution and MRU (most recently used) history.

use crate::home_dir;
use std::fs;
use std::path::PathBuf;

const MAX_MRU: usize = 10;

fn mru_file() -> PathBuf {
    home_dir().join(".config/margatroid/image-mru.json")
}

/// Resolve user input to a full image reference.
///
/// - Contains `/` or `:` → verbatim
/// - Otherwise → `docker.io/library/{input}:latest`
pub fn resolve(input: &str) -> String {
    if input.contains('/') || input.contains(':') {
        input.to_string()
    } else {
        format!("docker.io/library/{input}:latest")
    }
}

/// Load the MRU image list (most recent first).
pub fn load_mru() -> Vec<String> {
    let path = mru_file();
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Validate a session name: non-empty, alphanumeric + hyphens + underscores.
pub fn is_valid_session_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Record an image usage, pushing it to the front of the MRU list.
pub fn record_usage(image: &str) -> std::io::Result<()> {
    let path = mru_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut mru = load_mru();

    // Remove if already present, then prepend
    mru.retain(|i| i != image);
    mru.insert(0, image.to_string());
    mru.truncate(MAX_MRU);

    let content = serde_json::to_string_pretty(&mru)
        .map_err(std::io::Error::other)?;

    // Atomic write via tmp+rename to avoid corruption on crash
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_bare_name() {
        assert_eq!(resolve("ubuntu"), "docker.io/library/ubuntu:latest");
        assert_eq!(resolve("debian"), "docker.io/library/debian:latest");
        assert_eq!(resolve("node"), "docker.io/library/node:latest");
    }

    #[test]
    fn resolve_with_tag() {
        assert_eq!(resolve("node:22"), "node:22");
        assert_eq!(resolve("ubuntu:24.04"), "ubuntu:24.04");
    }

    #[test]
    fn resolve_with_registry() {
        assert_eq!(resolve("ghcr.io/org/img:tag"), "ghcr.io/org/img:tag");
        assert_eq!(
            resolve("docker.io/library/ubuntu"),
            "docker.io/library/ubuntu"
        );
    }

    #[test]
    fn valid_session_names() {
        assert!(is_valid_session_name("my-session"));
        assert!(is_valid_session_name("test_123"));
        assert!(is_valid_session_name("a"));
        assert!(is_valid_session_name("AbC-123_def"));
    }

    #[test]
    fn invalid_session_names() {
        assert!(!is_valid_session_name(""));
        assert!(!is_valid_session_name("has space"));
        assert!(!is_valid_session_name("has.dot"));
        assert!(!is_valid_session_name("path/slash"));
        assert!(!is_valid_session_name("semi;colon"));
    }

    #[test]
    fn mru_with_tempdir() {
        let dir = std::env::temp_dir().join(format!("orch-test-mru-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mru_path = dir.join("image-mru.json");

        // Write empty MRU
        std::fs::write(&mru_path, "[]").unwrap();

        // Read it back
        let mru: Vec<String> =
            serde_json::from_str(&std::fs::read_to_string(&mru_path).unwrap()).unwrap();
        assert!(mru.is_empty());

        // Write some entries
        let entries = vec!["ubuntu".to_string(), "debian".to_string()];
        std::fs::write(&mru_path, serde_json::to_string(&entries).unwrap()).unwrap();

        let mru: Vec<String> =
            serde_json::from_str(&std::fs::read_to_string(&mru_path).unwrap()).unwrap();
        assert_eq!(mru.len(), 2);
        assert_eq!(mru[0], "ubuntu");

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
