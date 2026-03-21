//! Fork a background helper that waits for Claude to settle,
//! optionally injects a resume prompt, then sends /remote-control.

use crate::{tmux, TMUX_SESSION};
use crate::home_dir;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum RemoteControlError {
    #[error("fork failed: {0}")]
    Fork(String),
}

/// Fork a background helper process that:
/// 1. Waits for the ❯ prompt to appear in the tmux pane
/// 2. Optionally sends a resume prompt
/// 3. Sends /remote-control
/// 4. Waits for bridge-pointer.json to appear
pub fn fork_helper(
    name: &str,
    session_dir: &Path,
    inject_resume: bool,
) -> Result<(), RemoteControlError> {
    let name = name.to_string();
    let session_dir = session_dir.to_path_buf();

    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Parent { .. }) => Ok(()),
        Ok(nix::unistd::ForkResult::Child) => {
            // Detach from parent's session
            let _ = nix::unistd::setsid();

            // Close inherited file descriptors (3..1024) to avoid leaking
            // lock files and network connections from the parent process.
            // FDs 0-2 are stdin/stdout/stderr which we keep.
            for fd in 3..1024 {
                let _ = nix::unistd::close(fd);
            }

            // Ignore SIGINT in the helper
            unsafe {
                nix::sys::signal::signal(
                    nix::sys::signal::Signal::SIGINT,
                    nix::sys::signal::SigHandler::SigIgn,
                )
                .ok();
            }

            helper_main(&name, &session_dir, inject_resume);
            std::process::exit(0);
        }
        Err(e) => Err(RemoteControlError::Fork(e.to_string())),
    }
}

fn helper_main(name: &str, session_dir: &Path, inject_resume: bool) {
    let target = format!("{TMUX_SESSION}:{name}");

    // Wait for the ❯ prompt (up to 2 minutes)
    let settled = wait_for_prompt(&target, Duration::from_secs(120));

    if !settled {
        tracing::warn!("session {name} did not settle within 120s");
    }

    if inject_resume {
        // Send resume prompt
        let _ = tmux::send_keys(
            &target,
            &[
                "It looks like your previous work was interrupted. Please review what you were doing and continue from where you left off.",
                "Enter",
            ],
        );

        // Wait for prompt again after Claude processes the resume
        wait_for_prompt(&target, Duration::from_secs(300));
    }

    // Send /remote-control
    tracing::info!("sending /remote-control to {name}");
    let _ = tmux::send_keys(&target, &["/remote-control", "Enter"]);

    // Claude Code shows a confirmation prompt after /remote-control.
    // Wait for it to appear, then send Enter to confirm.
    if wait_for_text(&target, Duration::from_secs(30), &["y/n", "Y/n", "confirm", "Enter"]) {
        tracing::info!("confirming remote-control for {name}");
        let _ = tmux::send_keys(&target, &["y", "Enter"]);
    } else {
        tracing::warn!("no confirmation prompt detected for {name}, sending Enter anyway");
        let _ = tmux::send_keys(&target, &["Enter"]);
    }

    // Wait for bridge-pointer.json to appear
    let slug = session_dir.to_string_lossy().replace('/', "-");
    let bridge_file = home_dir()
        .join(".claude/projects")
        .join(&slug)
        .join("bridge-pointer.json");

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if bridge_file.exists() {
            tracing::info!("bridge ready for {name}");
            return;
        }
        thread::sleep(Duration::from_secs(1));
    }

    tracing::warn!("bridge not found after 60s for {name}");
}

fn wait_for_prompt(target: &str, timeout: Duration) -> bool {
    wait_for_text(target, timeout, &["\u{276f}", "❯"])
}

fn wait_for_text(target: &str, timeout: Duration, needles: &[&str]) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_secs(2));
        if let Ok(content) = tmux::capture_pane(target) {
            if needles.iter().any(|n| content.contains(n)) {
                return true;
            }
        }
    }
    false
}
