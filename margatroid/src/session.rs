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
    pub skip_permissions: bool,
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
                skip_permissions: info.skip_permissions,
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
                        skip_permissions: false,
                    },
                );
            }
        }
    }

    let mut sessions: Vec<Session> = result.into_values().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

/// Set up host directories, per-session config, and CLAUDE.md for a session.
pub fn setup(name: &str, image: &str) -> Result<PathBuf> {
    let session_dir = margatroid_dir().join("sessions").join(name);
    let container_home = format!("/home/{name}");
    let host_mode = image == "host";
    claude_config::setup_session(&session_dir, name, &container_home, host_mode, image)?;
    Ok(session_dir)
}

/// Full launch sequence: setup, register, rename tmux window, fork helper, exec into podman.
/// This function does NOT return on success (it execs into podman or claude).
pub fn launch(
    name: &str,
    image_input: &str,
    inject_resume: bool,
    skip_permissions: bool,
) -> Result<()> {
    tracing::info!(name, image = image_input, skip_permissions, "launching session");

    let session_dir = setup(name, image_input)?;
    tracing::debug!(dir = %session_dir.display(), "session directory ready");

    let resolved_image = image::resolve(image_input);

    state::register_with_options(name, image_input, skip_permissions)?;

    // Rename tmux window
    if let Ok(pane_id) = std::env::var("TMUX_PANE") {
        let _ = tmux::rename_window(&pane_id, name);
    } else {
        let _ = tmux::rename_window(name, name);
    }

    // Determine resume action. For container sessions:
    // - The JSONL slug is based on the container path (/home/<name>)
    // - The JSONL files live in the session dir's .claude/projects/ (mounted rw)
    //   not in the host's ~/.claude/projects/
    let resume_action = if image_input == "host" {
        discovery::determine_resume_action(&session_dir)
    } else {
        let container_path = std::path::PathBuf::from(format!("/home/{name}"));
        let projects_root = session_dir.join(".claude/projects");
        discovery::determine_resume_action_in(&container_path, &projects_root)
    };
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let session_name = format!("{user}@{hostname}: {name}");

    let mut claude_args = vec!["--name".to_string(), session_name];
    if skip_permissions {
        claude_args.push("--dangerously-skip-permissions".to_string());
    }

    let should_inject = match &resume_action {
        discovery::ResumeAction::Fresh => {
            tracing::info!(name, "starting fresh session");
            eprintln!("  Starting fresh session...");
            false
        }
        discovery::ResumeAction::ResumeClean(uuid) => {
            tracing::info!(name, uuid = &uuid[..8.min(uuid.len())], "resuming clean session");
            eprintln!("  Resuming session {}...", uuid.get(..8).unwrap_or(uuid));
            claude_args.extend(["--resume".to_string(), uuid.clone()]);
            false
        }
        discovery::ResumeAction::ResumeInterrupted(uuid) => {
            tracing::info!(name, uuid = &uuid[..8.min(uuid.len())], "resuming interrupted session");
            eprintln!(
                "  Resuming interrupted session {}...",
                uuid.get(..8).unwrap_or(uuid)
            );
            claude_args.extend(["--resume".to_string(), uuid.clone()]);
            true
        }
    };
    // True whenever we're passing --resume to Claude Code (either clean or
    // interrupted). Used by the helper to look for — and auto-accept — the
    // "Resume from summary (recommended)" prompt that appears for large/old
    // resumed sessions.
    let is_resume = !matches!(resume_action, discovery::ResumeAction::Fresh);
    eprintln!();

    // Fork remote-control helper
    remote_control::fork_helper(name, inject_resume || should_inject, skip_permissions, is_resume)?;
    tracing::debug!(name, "remote-control helper forked");

    // Build the inner command (what the relay will fork/exec).
    let inner_cmd = if image_input == "host" {
        let claude_bin = podman::find_claude_bin();
        tracing::info!(name, bin = %claude_bin.display(), "host mode");
        let mut cmd = std::process::Command::new(claude_bin);
        cmd.args(&claude_args);
        cmd.current_dir(&session_dir);
        cmd
    } else {
        podman::remove_stale(name)?;
        let cmd = podman::build_run_command(name, &resolved_image, &session_dir, &claude_args);
        tracing::info!(
            name,
            image = %resolved_image,
            program = ?cmd.get_program(),
            args = ?cmd.get_args().collect::<Vec<_>>(),
            "container mode"
        );
        cmd
    };

    // Wrap with the relay binary, which owns the PTY and exposes a Unix socket.
    let relay_bin = find_relay_binary();
    let mut relay_cmd = std::process::Command::new(&relay_bin);
    relay_cmd.arg(name);
    relay_cmd.arg(inner_cmd.get_program());
    relay_cmd.args(inner_cmd.get_args());
    // Inherit environment and working directory from the inner command.
    if let Some(dir) = inner_cmd.get_current_dir() {
        relay_cmd.current_dir(dir);
    }
    for (k, v) in inner_cmd.get_envs() {
        match v {
            Some(val) => { relay_cmd.env(k, val); }
            None => { relay_cmd.env_remove(k); }
        }
    }
    tracing::info!(name, relay = %relay_bin.display(), "exec relay");

    let err = exec_command(&mut relay_cmd);
    Err(SessionError::Other(format!("exec failed: {err}")))
}

/// Stop a running session. For containerized sessions, stops the podman
/// container. For host sessions, sends /exit to Claude Code via tmux
/// and waits for it to deregister gracefully.
pub fn stop(name: &str) -> Result<()> {
    let sessions = state::load()?;
    let is_host = sessions
        .get(name)
        .map(|s| s.image == "host")
        .unwrap_or(false);

    if is_host {
        // Send /exit to Claude Code and give it time to deregister
        let target = format!("{}:{name}", crate::TMUX_SESSION);
        let _ = tmux::send_keys(&target, &["/exit", "Enter"]);
        // Wait up to 10s for the pane process to exit
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if !tmux::running_window_names().contains(name) {
                return Ok(());
            }
        }
        // Force kill if still running
        let _ = tmux::send_keys(&target, &["q"]);
    } else {
        podman::stop(name)?;
        podman::rm(name)?;
    }
    Ok(())
}

/// Restart a session: stop the container, then launch in a new tmux window.
pub fn restart(name: &str) -> Result<()> {
    // Get the image and options from saved state
    let sessions = state::load()?;
    let info = sessions.get(name);
    let image = info
        .map(|i| i.image.clone())
        .unwrap_or_else(|| "ubuntu".to_string());
    let skip_permissions = info.map(|i| i.skip_permissions).unwrap_or(false);

    stop(name)?;

    // Launch in a new tmux window
    let tui_bin = margatroid_dir().join("bin/margatroid-tui");
    let tui_path = tui_bin.to_string_lossy().into_owned();
    let mut args = vec![tui_path.as_str(), name, image.as_str()];
    if skip_permissions {
        args.push("--skip-permissions");
    }
    tmux::new_window(name, &args)?;

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

/// Rename a session. The session must be stopped first.
pub fn rename(old_name: &str, new_name: &str) -> Result<()> {
    // Validate the new name.
    if !crate::image::is_valid_session_name(new_name) {
        return Err(SessionError::Other(
            "Invalid name: use only letters, numbers, hyphens, underscores.".into(),
        ));
    }

    // Check the session exists.
    let sessions = state::load()?;
    let info = sessions.get(old_name).ok_or_else(|| {
        SessionError::Other(format!("Session `{old_name}` not found"))
    })?;
    let image = info.image.clone();
    let skip_permissions = info.skip_permissions;

    // Check the new name isn't taken.
    if sessions.contains_key(new_name) {
        return Err(SessionError::Other(
            format!("Session `{new_name}` already exists"),
        ));
    }

    // Check the session is stopped (not running in tmux).
    let running = tmux::running_window_names();
    if running.contains(old_name) {
        return Err(SessionError::Other(
            format!("Session `{old_name}` is running. Stop it first with `/stop {old_name}`."),
        ));
    }

    // Rename the session directory.
    let sessions_dir = margatroid_dir().join("sessions");
    let old_dir = sessions_dir.join(old_name);
    let new_dir = sessions_dir.join(new_name);
    if old_dir.is_dir() {
        fs::rename(&old_dir, &new_dir)?;
    }

    // Update state: deregister old, register new.
    state::deregister(old_name)?;
    state::register_with_options(new_name, &image, skip_permissions)?;

    // Update the margatroid block in CLAUDE.md with the new name.
    // The upsert will replace the existing block, preserving user content.
    let container_home = format!("/home/{new_name}");
    let host_mode = image == "host";
    let _ = claude_config::setup_session(&new_dir, new_name, &container_home, host_mode, &image);

    // Rename the JSONL project directory so resume detection finds the old
    // session under the new name's slug.
    if host_mode {
        // Host sessions: JSONL in ~/.claude/projects/<slug-of-session-dir>
        let host_projects = crate::home_dir().join(".claude/projects");
        let old_slug = discovery::slugify(&old_dir);
        let new_slug = discovery::slugify(&new_dir);
        let old_project = host_projects.join(&old_slug);
        let new_project = host_projects.join(&new_slug);
        if old_project.is_dir() && !new_project.exists() {
            let _ = fs::rename(&old_project, &new_project);
        }
    } else {
        // Container sessions: JSONL in session_dir/.claude/projects/<slug-of-container-path>
        let projects_dir = new_dir.join(".claude/projects");
        let old_slug = discovery::slugify(std::path::Path::new(&format!("/home/{old_name}")));
        let new_slug = discovery::slugify(std::path::Path::new(&format!("/home/{new_name}")));
        let old_project = projects_dir.join(&old_slug);
        let new_project = projects_dir.join(&new_slug);
        if old_project.is_dir() && !new_project.exists() {
            let _ = fs::rename(&old_project, &new_project);
        }
    }

    Ok(())
}

/// Find the margatroid-relay binary (installed location or co-located with current exe).
fn find_relay_binary() -> std::path::PathBuf {
    let installed = crate::margatroid_dir().join("bin/margatroid-relay");
    if installed.exists() {
        return installed;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let dev = dir.join("margatroid-relay");
            if dev.exists() {
                return dev;
            }
        }
    }
    // Fallback: assume it's in PATH.
    std::path::PathBuf::from("margatroid-relay")
}

/// Exec into a command (does not return on success).
#[cfg(unix)]
fn exec_command(cmd: &mut std::process::Command) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    cmd.exec()
}
