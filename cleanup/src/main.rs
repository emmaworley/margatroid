#![deny(warnings)]

//! Pane-died cleanup handler.
//!
//! Called by tmux hook: margatroid-cleanup <window_name> <pane_id>
//! Stops the container, deregisters the session, and kills the dead pane.

use margatroid::{podman, state, tmux};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: margatroid-cleanup <window_name> <pane_id>");
        std::process::exit(1);
    }

    let window_name = &args[1];
    let pane_id = &args[2];

    // Skip cleanup for internal windows (prefixed with _)
    if window_name.starts_with('_') {
        let _ = tmux::kill_pane(pane_id);
        return;
    }

    // Stop and remove the container
    let _ = podman::stop(window_name);
    let _ = podman::rm(window_name);

    // Deregister from session state
    if let Err(e) = state::deregister(window_name) {
        eprintln!("Failed to deregister session {window_name}: {e}");
    }

    // Kill the dead pane/window
    let _ = tmux::kill_pane(pane_id);
}
