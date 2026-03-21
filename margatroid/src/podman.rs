//! Podman container lifecycle management.

use crate::home_dir;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum PodmanError {
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

    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();

    let mut cmd = Command::new("podman");
    cmd.args([
        "run",
        "--rm",
        "-it",
        "--init",
        &format!("--name=margatroid-{name}"),
        &format!("--hostname={hostname}"),
        &format!("--userns=keep-id:uid={uid},gid={gid}"),
        &format!("--user={uid}:{gid}"),
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
        .args(["rm", "-f", &format!("margatroid-{name}")])
        .output()?;
    // Ignore errors — container may not exist
    Ok(())
}

/// Stop a container gracefully (5s timeout).
pub fn stop(name: &str) -> Result<()> {
    let _output = Command::new("podman")
        .args(["stop", "-t", "5", &format!("margatroid-{name}")])
        .output()?;
    Ok(())
}

/// Force-remove a container.
pub fn rm(name: &str) -> Result<()> {
    let _output = Command::new("podman")
        .args(["rm", "-f", &format!("margatroid-{name}")])
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
            &format!("margatroid-{name}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn build_run_command_has_correct_container_name() {
        let session_dir = PathBuf::from("/home/claude/sessions/test-session");
        let cmd = build_run_command("test-session", "ubuntu:latest", &session_dir, &[]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

        assert!(args.contains(&"--name=margatroid-test-session".to_string()));
    }

    #[test]
    fn build_run_command_has_workdir() {
        let session_dir = PathBuf::from("/home/claude/sessions/myproj");
        let cmd = build_run_command("myproj", "debian", &session_dir, &[]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

        assert!(args.contains(&"-w=/home/claude/sessions/myproj".to_string()));
    }

    #[test]
    fn build_run_command_passes_claude_args() {
        let session_dir = PathBuf::from("/home/claude/sessions/test");
        let claude_args = vec!["--name".to_string(), "my-session".to_string()];
        let cmd = build_run_command("test", "ubuntu", &session_dir, &claude_args);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"my-session".to_string()));
    }

    #[test]
    fn build_run_command_has_bind_mounts() {
        let session_dir = PathBuf::from("/home/claude/sessions/test");
        let cmd = build_run_command("test", "ubuntu", &session_dir, &[]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

        // Should have session dir mount
        let has_session_mount = args.iter().any(|a| {
            a.contains("/home/claude/sessions/test") && a.starts_with("-v=")
        });
        assert!(has_session_mount, "missing session dir bind mount");
    }

    #[test]
    fn build_run_command_sets_env_vars() {
        let session_dir = PathBuf::from("/tmp/sessions/test");
        let cmd = build_run_command("test", "ubuntu", &session_dir, &[]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

        // Check environment variables are set
        assert!(args.contains(&"HOME=/home/claude".to_string()));
        assert!(args.contains(&"DISABLE_AUTOUPDATER=1".to_string()));
    }

    #[test]
    fn build_run_command_uses_entrypoint() {
        let session_dir = PathBuf::from("/tmp/sessions/test");
        let cmd = build_run_command("test", "ubuntu", &session_dir, &[]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

        let ep_idx = args.iter().position(|a| a == "--entrypoint").unwrap();
        assert_eq!(args[ep_idx + 1], "/home/claude/.local/bin/claude");
    }

    #[test]
    fn build_run_command_uses_current_uid() {
        let session_dir = PathBuf::from("/tmp/sessions/test");
        let cmd = build_run_command("test", "ubuntu", &session_dir, &[]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

        let uid = nix::unistd::getuid().as_raw();
        let gid = nix::unistd::getgid().as_raw();
        let expected_userns = format!("--userns=keep-id:uid={uid},gid={gid}");
        let expected_user = format!("--user={uid}:{gid}");
        assert!(args.contains(&expected_userns), "missing userns with current uid");
        assert!(args.contains(&expected_user), "missing user with current uid");
    }
}
