#![deny(warnings)]

pub mod claude_config;
pub mod discovery;
pub mod image;
pub mod podman;
pub mod remote_control;
pub mod session;
pub mod state;
pub mod tmux;

use std::path::PathBuf;

/// Home directory for the claude user.
pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/claude".into()))
}

/// The shared tmux session name.
pub const TMUX_SESSION: &str = "claude";
