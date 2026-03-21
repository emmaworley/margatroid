#![deny(warnings)]

//! Systemd entry point for the Margatroid session manager.
//!
//! Creates the shared tmux session, restores saved sessions from
//! sessions.json, and stays alive polling for tmux session existence.

use margatroid::{home_dir, margatroid_dir, state, tmux, TMUX_SESSION};
use std::thread;
use std::time::Duration;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let tmux_conf = home_dir().join(".tmux.conf");
    let tui_bin = margatroid_dir().join("bin/margatroid-tui");
    let tui_path = tui_bin.to_string_lossy().into_owned();

    // Create the tmux session if it doesn't exist.
    // The initial window runs the TUI so the user sees the session manager
    // when they attach.
    if !tmux::has_session() {
        let conf_str = tmux_conf.to_string_lossy().into_owned();
        let tui_cmd = [tui_path.as_str()];
        if tmux_conf.exists() {
            if let Err(e) = tmux::create_session_with_config("_Session Manager", &conf_str, &tui_cmd) {
                tracing::error!("failed to create tmux session: {e}");
                std::process::exit(1);
            }
        } else if let Err(e) = tmux::create_session("_Session Manager", &tui_cmd) {
            tracing::error!("failed to create tmux session: {e}");
            std::process::exit(1);
        }
        tracing::info!("created tmux session '{TMUX_SESSION}'");
    } else {
        // Session exists — reload config
        if tmux_conf.exists() {
            let _ = tmux::source_config(&tmux_conf.to_string_lossy());
        }
    }

    // Get existing windows to avoid duplicates
    let existing = tmux::running_window_names();

    // Restore saved sessions
    let sessions = match state::load() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to load sessions: {e}");
            std::collections::HashMap::new()
        }
    };

    for (name, info) in &sessions {
        // Validate session name from state file before using in commands
        if !margatroid::image::is_valid_session_name(name) {
            tracing::warn!("skipping session with invalid name: {name:?}");
            continue;
        }

        if existing.contains(name) {
            tracing::info!("session {name} already running, skipping");
            continue;
        }

        tracing::info!("restoring session {name} (image: {})", info.image);

        if let Err(e) = tmux::new_window(name, &[&tui_path, name, &info.image]) {
            tracing::error!("failed to restore session {name}: {e}");
        }

        // Stagger startups to avoid overwhelming the API
        thread::sleep(Duration::from_secs(3));
    }

    // Install signal handlers
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();

    ctrlc_handler(r);

    // Stay alive — poll for tmux session existence
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        if !tmux::has_session() {
            tracing::info!("tmux session gone, exiting");
            break;
        }
        thread::sleep(Duration::from_secs(5));
    }
}

fn ctrlc_handler(running: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let _ = ctrlc::set_handler(move || {
        running.store(false, std::sync::atomic::Ordering::Relaxed);
    });
}
