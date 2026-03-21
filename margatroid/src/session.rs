//! High-level session operations tying together all modules.

use crate::{claude_config, discovery, image, margatroid_dir, podman, remote_control, state, tmux};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Stopped,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    pub image: String,
    pub status: SessionStatus,
    pub container_id: Option<String>,
    pub session_dir: PathBuf,
    pub last_uuid: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("state error: {0}")]
    State(#[from] state::StateError),
    #[error("config error: {0}")]
    Config(#[from] claude_config::ConfigError),
    #[error("tmux error: {0}")]
    Tmux(#[from] tmux::TmuxError),
    #[error("podman error: {0}")]
    Podman(#[from] podman::PodmanError),
    #[error("remote control error: {0}")]
    RemoteControl(#[from] remote_control::RemoteControlError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

type Result<T> = std::result::Result<T, SessionError>;

/// List all known sessions by merging sessions.json, tmux windows, and ~/sessions/ on disk.
pub fn list_all() -> Result<Vec<Session>> {
    let saved = state::load()?;
    let running = tmux::running_window_names();
    let sessions_dir = margatroid_dir().join("sessions");

    let mut result: HashMap<String, Session> = HashMap::new();

    // Start with saved sessions
    for (name, info) in &saved {
        let session_dir = sessions_dir.join(name);
        let last_uuid = discovery::find_last_uuid(&session_dir);
        let status = if running.contains(name) {
            SessionStatus::Running
        } else {
            SessionStatus::Stopped
        };
        let container_id = if status == SessionStatus::Running {
            podman::inspect_id(name)
        } else {
            None
        };

        result.insert(
            name.clone(),
            Session {
                name: name.clone(),
                image: info.image.clone(),
                status,
                container_id,
                session_dir,
                last_uuid,
            },
        );
    }

    // Add any directories in ~/sessions/ not in saved state
    if sessions_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&sessions_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('_') || !entry.path().is_dir() {
                    continue;
                }
                // Skip directories with invalid session names
                if !image::is_valid_session_name(&name) {
                    continue;
                }
                if result.contains_key(&name) {
                    continue;
                }
                let session_dir = entry.path();
                let last_uuid = discovery::find_last_uuid(&session_dir);
                let status = if running.contains(&name) {
                    SessionStatus::Running
                } else {
                    SessionStatus::Stopped
                };
                let container_id = if status == SessionStatus::Running {
                    podman::inspect_id(&name)
                } else {
                    None
                };

                result.insert(
                    name.clone(),
                    Session {
                        name: name.clone(),
                        image: "unknown".to_string(),
                        status,
                        container_id,
                        session_dir,
                        last_uuid,
                    },
                );
            }
        }
    }

    let mut sessions: Vec<Session> = result.into_values().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

/// Set up host directories, trust, and CLAUDE.md for a session.
pub fn setup(name: &str) -> Result<PathBuf> {
    let session_dir = margatroid_dir().join("sessions").join(name);
    claude_config::ensure_trusted(&session_dir)?;
    claude_config::write_claude_md(&session_dir, name)?;
    Ok(session_dir)
}

/// Full launch sequence: setup, register, rename tmux window, fork helper, exec into podman.
/// This function does NOT return on success (it execs into podman or claude).
pub fn launch(name: &str, image_input: &str, inject_resume: bool) -> Result<()> {
    tracing::info!(name, image = image_input, "launching session");

    let session_dir = setup(name)?;
    tracing::debug!(dir = %session_dir.display(), "session directory ready");

    let resolved_image = image::resolve(image_input);

    state::register(name, image_input)?;
    let _ = image::record_usage(image_input);

    // Rename tmux window
    if let Ok(pane_id) = std::env::var("TMUX_PANE") {
        let _ = tmux::rename_window(&pane_id, name);
    } else {
        let _ = tmux::rename_window(name, name);
    }

    // Determine resume action
    let resume_action = discovery::determine_resume_action(&session_dir);
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let session_name = format!("{user}@{hostname}: {name}");

    // For fresh sessions, use `claude remote-control` directly — no helper needed.
    // For resume sessions, start claude with --resume and fork the helper to send
    // /remote-control after the session settles.
    let use_rc_subcommand = matches!(resume_action, discovery::ResumeAction::Fresh) && !inject_resume;

    let claude_args = if use_rc_subcommand {
        tracing::info!(name, "starting fresh session (remote-control subcommand)");
        eprintln!("  Starting fresh session...");
        vec!["remote-control".to_string(), "--name".to_string(), session_name]
    } else {
        let mut args = vec!["--name".to_string(), session_name];
        match &resume_action {
            discovery::ResumeAction::Fresh => {
                tracing::info!(name, "starting fresh session");
                eprintln!("  Starting fresh session...");
            }
            discovery::ResumeAction::ResumeClean(uuid) => {
                tracing::info!(name, uuid = &uuid[..8.min(uuid.len())], "resuming clean session");
                eprintln!("  Resuming session {}...", uuid.get(..8).unwrap_or(uuid));
                args.extend(["--resume".to_string(), uuid.clone()]);
            }
            discovery::ResumeAction::ResumeInterrupted(uuid) => {
                tracing::info!(name, uuid = &uuid[..8.min(uuid.len())], "resuming interrupted session");
                eprintln!(
                    "  Resuming interrupted session {}...",
                    uuid.get(..8).unwrap_or(uuid)
                );
                args.extend(["--resume".to_string(), uuid.clone()]);
            }
        }
        args
    };
    eprintln!();

    if !use_rc_subcommand {
        // Fork remote-control helper for resume sessions
        remote_control::fork_helper(name, &session_dir, inject_resume || matches!(resume_action, discovery::ResumeAction::ResumeInterrupted(_)))?;
        tracing::debug!(name, "remote-control helper forked");
    }

    if image_input == "host" {
        // Uncontainerized: exec claude directly on the host
        let claude_bin = podman::find_claude_bin();
        tracing::info!(name, bin = %claude_bin.display(), "exec claude (host mode)");
        let mut cmd = std::process::Command::new(claude_bin);
        cmd.args(&claude_args);
        cmd.current_dir(&session_dir);
        let err = exec_command(&mut cmd);
        return Err(SessionError::Other(format!("exec failed: {err}")));
    }

    // Clean up stale container
    podman::remove_stale(name)?;

    // Exec into podman (replaces this process)
    let mut cmd = podman::build_run_command(name, &resolved_image, &session_dir, &claude_args);
    tracing::info!(
        name,
        image = %resolved_image,
        program = ?cmd.get_program(),
        args = ?cmd.get_args().collect::<Vec<_>>(),
        "exec podman"
    );

    let err = exec_command(&mut cmd);
    Err(SessionError::Other(format!("exec failed: {err}")))
}

/// Stop a running session's container.
pub fn stop(name: &str) -> Result<()> {
    podman::stop(name)?;
    podman::rm(name)?;
    Ok(())
}

/// Restart a session: stop the container, then launch in a new tmux window.
pub fn restart(name: &str) -> Result<()> {
    // Get the image from saved state
    let sessions = state::load()?;
    let image = sessions
        .get(name)
        .map(|i| i.image.clone())
        .unwrap_or_else(|| "ubuntu".to_string());

    stop(name)?;

    // Launch in a new tmux window
    let tui_bin = margatroid_dir().join("bin/margatroid-tui");
    let tui_path = tui_bin.to_string_lossy().into_owned();
    tmux::new_window(name, &[&tui_path, name, &image])?;

    Ok(())
}

/// Delete a session. Optionally remove data from disk.
pub fn delete(name: &str, remove_data: bool) -> Result<()> {
    // Stop container if running
    let _ = stop(name);

    // Deregister
    state::deregister(name)?;

    if remove_data {
        let session_dir = margatroid_dir().join("sessions").join(name);
        if session_dir.is_dir() {
            fs::remove_dir_all(&session_dir)?;
        }
    }

    Ok(())
}

/// Exec into a command (does not return on success).
#[cfg(unix)]
fn exec_command(cmd: &mut std::process::Command) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    cmd.exec()
}
