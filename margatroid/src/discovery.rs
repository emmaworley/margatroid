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

fn default_projects_dir() -> PathBuf {
    home_dir().join(".claude/projects")
}

fn slugify_path(path: &Path) -> String {
    path.to_string_lossy().replace(['/', '.'], "-")
}

/// Find the most recent session UUID for the given session directory.
/// `projects_root` overrides where to look for project JSONL files.
pub fn find_last_uuid(session_dir: &Path) -> Option<String> {
    find_last_uuid_in(session_dir, &default_projects_dir())
}

fn find_last_uuid_in(session_dir: &Path, projects_root: &Path) -> Option<String> {
    let slug = slugify_path(session_dir);
    let project_dir = projects_root.join(&slug);

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
    determine_resume_action_in(session_dir, &default_projects_dir())
}

/// Like `determine_resume_action` but looks for JSONL files in a custom
/// projects root instead of the default `~/.claude/projects/`.
pub fn determine_resume_action_in(session_dir: &Path, projects_root: &Path) -> ResumeAction {
    let uuid = match find_last_uuid_in(session_dir, projects_root) {
        Some(u) => u,
        None => return ResumeAction::Fresh,
    };

    if was_session_idle(session_dir, &uuid, projects_root) {
        ResumeAction::ResumeClean(uuid)
    } else {
        ResumeAction::ResumeInterrupted(uuid)
    }
}

/// Check if the session was idle (waiting for input) or cleanly exited.
/// Reads the tail of the JSONL file to determine the final state.
fn was_session_idle(session_dir: &Path, uuid: &str, projects_root: &Path) -> bool {
    let slug = slugify_path(session_dir);
    let jsonl_path = projects_root.join(&slug).join(format!("{uuid}.jsonl"));

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
        // Dots are replaced with dashes, matching Claude Code's slugification.
        assert_eq!(
            slugify(Path::new("/home/margatroid/.margatroid/sessions/dev")),
            "-home-margatroid--margatroid-sessions-dev"
        );
    }

    #[test]
    fn determine_resume_with_custom_projects_root() {
        let dir = std::env::temp_dir().join(format!("orch-test-resume-{}", std::process::id()));
        let projects_root = dir.join("projects");
        let container_path = Path::new("/home/testbox");
        let slug = slugify(container_path); // "-home-testbox"
        let project_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&project_dir).unwrap();

        // No JSONL files → Fresh
        let action = determine_resume_action_in(container_path, &projects_root);
        assert!(matches!(action, ResumeAction::Fresh));

        // Write a JSONL with a success result → ResumeClean
        let uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let jsonl = project_dir.join(format!("{uuid}.jsonl"));
        std::fs::write(
            &jsonl,
            r#"{"type":"result","subtype":"success","result":"done"}"#,
        )
        .unwrap();

        let action = determine_resume_action_in(container_path, &projects_root);
        match action {
            ResumeAction::ResumeClean(u) => assert_eq!(u, uuid),
            other => panic!("expected ResumeClean, got {other:?}"),
        }

        // Write interrupted content → ResumeInterrupted
        std::fs::write(
            &jsonl,
            r#"{"type":"user","message":{"content":"fix bug"}}
{"type":"assistant","message":{"content":"working on it"}}"#,
        )
        .unwrap();

        let action = determine_resume_action_in(container_path, &projects_root);
        match action {
            ResumeAction::ResumeInterrupted(u) => assert_eq!(u, uuid),
            other => panic!("expected ResumeInterrupted, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn container_session_uses_session_dir_projects() {
        // Simulate the container session layout:
        // session_dir/.claude/projects/-home-mybox/<uuid>.jsonl
        let dir = std::env::temp_dir().join(format!("orch-test-container-{}", std::process::id()));
        let session_dir = dir.join("sessions/mybox");
        let container_path = Path::new("/home/mybox");
        let slug = slugify(container_path); // "-home-mybox"
        let projects_in_session = session_dir.join(".claude/projects").join(&slug);
        std::fs::create_dir_all(&projects_in_session).unwrap();

        let uuid = "11111111-2222-3333-4444-555555555555";
        std::fs::write(
            projects_in_session.join(format!("{uuid}.jsonl")),
            r#"{"type":"result","subtype":"success","result":"ok"}"#,
        )
        .unwrap();

        // Looking in default projects dir (host) should find nothing
        let action = determine_resume_action(container_path);
        assert!(matches!(action, ResumeAction::Fresh));

        // Looking in session dir's projects root should find it
        let projects_root = session_dir.join(".claude/projects");
        let action = determine_resume_action_in(container_path, &projects_root);
        match action {
            ResumeAction::ResumeClean(u) => assert_eq!(u, uuid),
            other => panic!("expected ResumeClean, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
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
