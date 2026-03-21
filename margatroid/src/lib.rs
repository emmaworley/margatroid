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

/// Home directory for the user.
pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/claude".into()))
}

/// Root directory for all margatroid data (`~/.margatroid`).
pub fn margatroid_dir() -> PathBuf {
    std::env::var("MARGATROID_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".margatroid"))
}

/// The shared tmux session name.
pub const TMUX_SESSION: &str = "margatroid";
