//! Thin wrappers around tmux commands.

use crate::TMUX_SESSION;
use std::process::Command;


#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("tmux command failed: {0}")]
    Command(String),
    #[error("tmux io error: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, TmuxError>;

fn run(args: &[&str]) -> Result<String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .map_err(TmuxError::Io)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(TmuxError::Command(format!(
            "tmux {} failed: {}",
            args.join(" "),
            stderr
        )))
    }
}

fn run_ok(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if the shared tmux session exists.
pub fn has_session() -> bool {
    run_ok(&["has-session", "-t", TMUX_SESSION])
}

/// Create the shared tmux session with an initial window name.
pub fn create_session(initial_window: &str) -> Result<()> {
    run(&["new-session", "-d", "-s", TMUX_SESSION, "-n", initial_window])?;
    Ok(())
}

/// Create the session with a specific config file.
pub fn create_session_with_config(initial_window: &str, config: &str) -> Result<()> {
    run(&[
        "-f",
        config,
        "new-session",
        "-d",
        "-s",
        TMUX_SESSION,
        "-n",
        initial_window,
    ])?;
    Ok(())
}

/// Source a tmux config file.
pub fn source_config(config: &str) -> Result<()> {
    run(&["source-file", config])?;
    Ok(())
}

/// List window names in the shared session.
pub fn list_windows() -> Result<Vec<String>> {
    let output = run(&[
        "list-windows",
        "-t",
        TMUX_SESSION,
        "-F",
        "#{window_name}",
    ])?;
    Ok(output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect())
}

/// Create a new window running the given command.
pub fn new_window(name: &str, cmd: &[&str]) -> Result<()> {
    let target = format!("{TMUX_SESSION}:");
    let mut args = vec![
        "new-window",
        "-t",
        &target,
        "-n",
        name,
        "--",
    ];
    args.extend(cmd);
    run(&args)?;
    Ok(())
}

/// Rename a window (target can be a pane id or window specifier).
pub fn rename_window(target: &str, name: &str) -> Result<()> {
    run(&["rename-window", "-t", target, name])?;
    Ok(())
}

/// Kill a pane by its id.
pub fn kill_pane(pane_id: &str) -> Result<()> {
    run(&["kill-pane", "-t", pane_id])?;
    Ok(())
}

/// Capture the visible content of a pane.
pub fn capture_pane(target: &str) -> Result<String> {
    run(&["capture-pane", "-t", target, "-p"])
}

/// Send keys to a tmux target.
pub fn send_keys(target: &str, keys: &[&str]) -> Result<()> {
    let mut args = vec!["send-keys", "-t", target];
    args.extend(keys);
    run(&args)?;
    Ok(())
}

/// Get the set of currently running window names.
pub fn running_window_names() -> std::collections::HashSet<String> {
    list_windows()
        .unwrap_or_default()
        .into_iter()
        .collect()
}
