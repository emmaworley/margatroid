//! Persistent session state in ~/.config/claude-sessions/sessions.json.
//! All mutations are protected by flock.

use crate::home_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub image: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("lock error: {0}")]
    Lock(String),
}

type Result<T> = std::result::Result<T, StateError>;

fn sessions_file() -> PathBuf {
    home_dir().join(".config/claude-sessions/sessions.json")
}

fn lock_file() -> PathBuf {
    home_dir().join(".config/claude-sessions/sessions.json.lock")
}

/// Load sessions from disk (no locking).
pub fn load() -> Result<HashMap<String, SessionInfo>> {
    let path = sessions_file();
    match fs::read_to_string(&path) {
        Ok(content) => Ok(serde_json::from_str(&content)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(e) => Err(e.into()),
    }
}

/// Save sessions to disk atomically (no locking — caller must hold lock).
fn save_inner(sessions: &HashMap<String, SessionInfo>) -> Result<()> {
    let path = sessions_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let content = serde_json::to_string_pretty(sessions)?;
    fs::write(&tmp, content)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Execute a closure under an exclusive flock on the sessions file.
pub fn with_lock<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    let lf = lock_file();
    if let Some(parent) = lf.parent() {
        fs::create_dir_all(parent)?;
    }

    // Open with O_CLOEXEC so the lock FD isn't inherited by child processes (fork).
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_CLOEXEC)
        .open(&lf)?;

    // Use flock for exclusive locking. O_CLOEXEC above ensures the FD
    // won't leak into child processes created by fork().
    use std::os::unix::io::AsRawFd;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        return Err(StateError::Lock(format!(
            "flock failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    // Lock released when `file` drops
    f()
}

/// Register a session (creates or updates). Holds flock.
pub fn register(name: &str, image: &str) -> Result<()> {
    with_lock(|| {
        let mut sessions = load()?;
        sessions.insert(
            name.to_string(),
            SessionInfo {
                image: image.to_string(),
            },
        );
        save_inner(&sessions)?;
        Ok(())
    })
}

/// Deregister a session. Holds flock.
pub fn deregister(name: &str) -> Result<()> {
    with_lock(|| {
        let mut sessions = load()?;
        sessions.remove(name);
        save_inner(&sessions)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_info_roundtrip() {
        let mut sessions = HashMap::new();
        sessions.insert(
            "test-session".to_string(),
            SessionInfo {
                image: "ubuntu".to_string(),
            },
        );
        sessions.insert(
            "another".to_string(),
            SessionInfo {
                image: "debian".to_string(),
            },
        );

        let json = serde_json::to_string_pretty(&sessions).unwrap();
        let parsed: HashMap<String, SessionInfo> = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed["test-session"].image, "ubuntu");
        assert_eq!(parsed["another"].image, "debian");
    }

    #[test]
    fn load_empty_file() {
        let json = "{}";
        let parsed: HashMap<String, SessionInfo> = serde_json::from_str(json).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn load_v1_format() {
        // sessions.json uses {"name": {"image": "..."}} format
        let json = r#"{"daily-briefings": {"image": "ubuntu"}, "librarian": {"image": "node"}}"#;
        let parsed: HashMap<String, SessionInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed["daily-briefings"].image, "ubuntu");
        assert_eq!(parsed["librarian"].image, "node");
    }
}
