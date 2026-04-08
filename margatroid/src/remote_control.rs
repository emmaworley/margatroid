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
/// 2. Optionally accepts the bypass-permissions confirmation
/// 3. Optionally sends a resume prompt
/// 4. Sends /remote-control
pub fn fork_helper(
    name: &str,
    inject_resume: bool,
    skip_permissions: bool,
) -> Result<(), RemoteControlError> {
    let name = name.to_string();

    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Parent { .. }) => Ok(()),
        Ok(nix::unistd::ForkResult::Child) => {
            // Detach from parent's session
            let _ = nix::unistd::setsid();

            // Redirect stdin/stdout/stderr to /dev/null. The parent's PTY is
            // shared with the relay/podman/Claude process, so any writes by
            // this helper (e.g. tracing logs) would corrupt Claude's terminal
            // output. Tracing init in helper_main will install a no-op
            // subscriber too, but redirect FDs as a belt-and-braces measure.
            if let Ok(devnull) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
            {
                use std::os::unix::io::AsRawFd;
                let null_fd = devnull.as_raw_fd();
                let _ = nix::unistd::dup2(null_fd, 0);
                let _ = nix::unistd::dup2(null_fd, 1);
                let _ = nix::unistd::dup2(null_fd, 2);
            }

            // Close inherited file descriptors (3..1024) to avoid leaking
            // lock files and network connections from the parent process.
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

            helper_main(&name, inject_resume, skip_permissions);
            std::process::exit(0);
        }
        Err(e) => Err(RemoteControlError::Fork(e.to_string())),
    }
}

fn helper_main(name: &str, inject_resume: bool, skip_permissions: bool) {
    let target = format!("{TMUX_SESSION}:{name}");

    if skip_permissions {
        // Claude shows a "Bypass Permissions" confirmation prompt with
        // "No, exit" selected by default. Navigate Down to "Yes, I accept"
        // then press Enter. The keys must be sent in separate calls with
        // a delay; sending Down+Enter in one tmux send-keys call delivers
        // them too fast for Claude's React UI to process the Down before
        // Enter arrives, resulting in Enter being applied to the still-
        // default "No, exit" option.
        if wait_for_text(&target, "Enter to confirm", Duration::from_secs(60)) {
            let _ = tmux::send_keys(&target, &["Down"]);
            thread::sleep(Duration::from_millis(200));
            let _ = tmux::send_keys(&target, &["Enter"]);
        }
    }

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
                "Your previous session may have been interrupted by a restart. If there is unfinished work, please review and continue from where you left off. If no work was in progress, wait for user input.",
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
    wait_for_text(target, "❯", timeout)
}

fn wait_for_text(target: &str, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_secs(2));
        if let Ok(content) = tmux::capture_pane(target) {
            if content.contains(needle) {
                return true;
            }
        }
    }
    false
}
