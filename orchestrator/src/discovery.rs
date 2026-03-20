//! Session UUID discovery from JSONL files and resume detection.

use crate::home_dir;
use std::fs;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// How to handle session resumption.
#[derive(Debug, Clone)]
pub enum ResumeAction {
    /// No previous session found.
    Fresh,
    /// Session was idle or cleanly exited — just resume.
    ResumeClean(String),
    /// Session was interrupted mid-work — inject resume prompt.
    ResumeInterrupted(String),
}

fn projects_dir() -> PathBuf {
    home_dir().join(".claude/projects")
}

fn slugify_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

/// Find the most recent session UUID for the given session directory.
pub fn find_last_uuid(session_dir: &Path) -> Option<String> {
    let slug = slugify_path(session_dir);
    let project_dir = projects_dir().join(&slug);

    if !project_dir.is_dir() {
        return None;
    }

    let mut jsonl_files: Vec<_> = fs::read_dir(&project_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "jsonl")
                .unwrap_or(false)
        })
        .collect();

    jsonl_files.sort_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()));

    jsonl_files
        .last()
        .and_then(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
}

/// Determine the resume action for a session directory.
pub fn determine_resume_action(session_dir: &Path) -> ResumeAction {
    let uuid = match find_last_uuid(session_dir) {
        Some(u) => u,
        None => return ResumeAction::Fresh,
    };

    if was_session_idle(session_dir, &uuid) {
        ResumeAction::ResumeClean(uuid)
    } else {
        ResumeAction::ResumeInterrupted(uuid)
    }
}

/// Check if the session was idle (waiting for input) or cleanly exited.
/// Reads the tail of the JSONL file to determine the final state.
fn was_session_idle(session_dir: &Path, uuid: &str) -> bool {
    let slug = slugify_path(session_dir);
    let jsonl_path = projects_dir().join(&slug).join(format!("{uuid}.jsonl"));

    match read_tail(&jsonl_path, 4096) {
        Some(tail) => is_tail_idle(&tail),
        None => true, // Can't read → treat as clean (safe default)
    }
}

/// Analyze JSONL tail content to determine if the session was idle.
/// Exported for testing.
pub fn is_tail_idle(tail: &str) -> bool {
    let lines: Vec<&str> = tail.lines().rev().take(20).collect();

    let mut found_system_event = false;

    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }

        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match event_type {
                "result" => {
                    let subtype = event.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
                    if subtype == "success" {
                        return true;
                    }
                }
                "assistant" => return false,
                "user" => {
                    let content = event
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    if content.contains("/exit") {
                        return true;
                    }
                    // User sent a message that wasn't /exit — work was in progress
                    return false;
                }
                // Track system/control events (remote-control, bridge_status, etc.)
                _ => {
                    found_system_event = true;
                    continue;
                }
            }
        }
    }

    // If we parsed JSON events but none were semantic (only system events),
    // this was likely an active worker session (running /remote-control)
    // that got killed — treat as interrupted.
    // If we found no JSON at all (empty file / no events), treat as idle
    // since there's nothing to resume.
    !found_system_event
}

/// Slugify a path for display. Public for testing.
pub fn slugify(path: &Path) -> String {
    slugify_path(path)
}

/// Read the last `n` bytes of a file, handling partial UTF-8 at the seek boundary.
fn read_tail(path: &Path, n: u64) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(n);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    // If we seeked into the middle of a multi-byte UTF-8 char, lossy conversion
    // replaces the leading fragment with U+FFFD, which is harmless for JSONL parsing.
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_after_success_result() {
        let tail = r#"{"type":"result","subtype":"success","result":"done"}
"#;
        assert!(is_tail_idle(tail));
    }

    #[test]
    fn interrupted_mid_assistant() {
        let tail = r#"{"type":"user","message":{"content":"fix the bug"}}
{"type":"assistant","message":{"content":"I'll fix"}}
"#;
        assert!(!is_tail_idle(tail));
    }

    #[test]
    fn idle_after_exit() {
        let tail = r#"{"type":"user","message":{"content":"/exit"}}
"#;
        assert!(is_tail_idle(tail));
    }

    #[test]
    fn idle_on_empty() {
        assert!(is_tail_idle(""));
    }

    #[test]
    fn idle_on_garbage() {
        // Unparseable lines → no system events → treated as idle (nothing to resume)
        assert!(is_tail_idle("not json\nalso not json\n"));
    }

    #[test]
    fn interrupted_only_system_events() {
        // Sessions killed while running /remote-control have only system events at the tail
        let tail = r#"{"type":"system","subtype":"local_command"}
{"type":"system","subtype":"bridge_status"}
{"type":"system","subtype":"local_command"}
"#;
        assert!(!is_tail_idle(tail));
    }

    #[test]
    fn idle_success_before_system_events() {
        // Result success followed by system events — session is idle
        let tail = r#"{"type":"result","subtype":"success","result":"ok"}
{"type":"system","subtype":"bridge_status"}
{"type":"system","subtype":"local_command"}
"#;
        assert!(is_tail_idle(tail));
    }

    #[test]
    fn interrupted_user_message_not_exit() {
        // User sent a regular message — work was in progress
        let tail = r#"{"type":"user","message":{"content":"fix the bug"}}
{"type":"system","subtype":"local_command"}
"#;
        assert!(!is_tail_idle(tail));
    }

    #[test]
    fn idle_success_after_assistant() {
        // Result success comes after the assistant message — session is idle
        let tail = r#"{"type":"assistant","message":{"content":"done"}}
{"type":"result","subtype":"success","result":"ok"}
"#;
        assert!(is_tail_idle(tail));
    }

    #[test]
    fn slugify_path_works() {
        assert_eq!(
            slugify(Path::new("/home/claude/sessions/test")),
            "-home-claude-sessions-test"
        );
    }

    #[test]
    fn read_tail_small_file() {
        let dir = std::env::temp_dir().join(format!("orch-test-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jsonl");

        let content = "line one\nline two\nline three\n";
        std::fs::write(&path, content).unwrap();

        // Request more than file size — should get whole file
        let result = read_tail(&path, 1000).unwrap();
        assert_eq!(result, content);

        // Request last 15 bytes
        let result = read_tail(&path, 15).unwrap();
        assert!(result.contains("three"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
