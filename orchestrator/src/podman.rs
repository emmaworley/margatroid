//! Podman container lifecycle management.

use crate::home_dir;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum PodmanError {
    #[error("podman command failed: {0}")]
    Command(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, PodmanError>;

/// Build the podman run command for a session. Does not execute it.
pub fn build_run_command(
    name: &str,
    image: &str,
    session_dir: &Path,
    claude_args: &[String],
) -> Command {
    let home = home_dir();
    let claude_bin = home.join(".local/bin/claude");
    let claude_json = home.join(".claude.json");
    let claude_dir = home.join(".claude");
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    let mut cmd = Command::new("podman");
    cmd.args([
        "run",
        "--rm",
        "-it",
        "--init",
        &format!("--name=claude-{name}"),
        &format!("--hostname={hostname}"),
        "--userns=keep-id:uid=1001,gid=1001",
        "--user=1001:1001",
        "--entrypoint",
        "/home/claude/.local/bin/claude",
    ]);

    // Bind mounts
    cmd.arg(format!(
        "-v={}:/home/claude/.local/bin/claude:ro,z",
        claude_bin.display()
    ));
    cmd.arg(format!(
        "-v={}:/home/claude/sessions/{name}:rw,z",
        session_dir.display()
    ));
    cmd.arg(format!(
        "-v={}:/home/claude/.claude:rw,z",
        claude_dir.display()
    ));
    cmd.arg(format!(
        "-v={}:/home/claude/.claude.json:rw,z",
        claude_json.display()
    ));

    // Environment
    cmd.args(["-e", "HOME=/home/claude"]);
    cmd.args(["-e", "LANG=en_US.UTF-8"]);
    cmd.args(["-e", "USER=claude"]);
    cmd.args(["-e", "LOGNAME=claude"]);
    cmd.args(["-e", "SHELL=/bin/bash"]);
    cmd.args(["-e", "DISABLE_AUTOUPDATER=1"]);

    // Working directory
    cmd.arg(format!("-w=/home/claude/sessions/{name}"));

    // Image
    cmd.arg(image);

    // Claude CLI args
    cmd.args(claude_args);

    cmd
}

/// Force-remove a stale container with the given session name.
pub fn remove_stale(name: &str) -> Result<()> {
    let _output = Command::new("podman")
        .args(["rm", "-f", &format!("claude-{name}")])
        .output()?;
    // Ignore errors — container may not exist
    Ok(())
}

/// Stop a container gracefully (5s timeout).
pub fn stop(name: &str) -> Result<()> {
    let _output = Command::new("podman")
        .args(["stop", "-t", "5", &format!("claude-{name}")])
        .output()?;
    Ok(())
}

/// Force-remove a container.
pub fn rm(name: &str) -> Result<()> {
    let _output = Command::new("podman")
        .args(["rm", "-f", &format!("claude-{name}")])
        .output()?;
    Ok(())
}

/// Get the container ID for a running session, if any.
pub fn inspect_id(name: &str) -> Option<String> {
    let output = Command::new("podman")
        .args([
            "inspect",
            "--format",
            "{{.Id}}",
            &format!("claude-{name}"),
        ])
        .output()
        .ok()?;
    if output.status.success() {
        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if id.is_empty() {
            None
        } else {
            Some(id[..12.min(id.len())].to_string())
        }
    } else {
        None
    }
}
