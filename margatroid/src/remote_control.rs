//! Fork a background helper that waits for Claude to settle,
//! optionally injects a resume prompt, then sends /remote-control.

use crate::{tmux, TMUX_SESSION};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum RemoteControlError {
    #[error("fork failed: {0}")]
    Fork(String),
}

/// Fork a background helper process that:
/// 1. Waits for the prompt to appear in the tmux pane
/// 2. Optionally sends a resume prompt
/// 3. Sends /remote-control
pub fn fork_helper(
    name: &str,
    inject_resume: bool,
) -> Result<(), RemoteControlError> {
    let name = name.to_string();

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

            helper_main(&name, inject_resume);
            std::process::exit(0);
        }
        Err(e) => Err(RemoteControlError::Fork(e.to_string())),
    }
}

fn helper_main(name: &str, inject_resume: bool) {
    let target = format!("{TMUX_SESSION}:{name}");

    // Wait for the prompt (up to 2 minutes)
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

    // Send /remote-control (confirmation is pre-accepted via remoteDialogSeen
    // in the per-session .claude.json, set by setup_session)
    tracing::info!("sending /remote-control to {name}");
    let _ = tmux::send_keys(&target, &["/remote-control", "Enter"]);
}

fn wait_for_prompt(target: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_secs(2));
        if let Ok(content) = tmux::capture_pane(target) {
            if content.contains('\u{276f}') || content.contains('❯') {
                return true;
            }
        }
    }
    false
}
