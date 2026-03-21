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

/// Find the claude binary by searching PATH, falling back to ~/.local/bin/claude.
pub fn find_claude_bin() -> std::path::PathBuf {
    // Check PATH first
    if let Ok(output) = Command::new("which").arg("claude").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return std::path::PathBuf::from(path);
            }
        }
    }
    // Fallback
    home_dir().join(".local/bin/claude")
}

/// Build the podman run command for a session. Does not execute it.
pub fn build_run_command(
    name: &str,
    image: &str,
    session_dir: &Path,
    claude_args: &[String],
) -> Command {
    let home = home_dir();
    let home_str = home.to_string_lossy();
    let claude_bin = find_claude_bin();
    let claude_json = home.join(".claude.json");
    let claude_dir = home.join(".claude");
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();

    // Container-internal paths mirror the host home directory so that
    // bind mounts preserve ownership and file permissions.
    let c_claude_bin = format!("{home_str}/.local/bin/claude");
    let c_session_dir = format!("{home_str}/sessions/{name}");
    let c_claude_dir = format!("{home_str}/.claude");
    let c_claude_json = format!("{home_str}/.claude.json");

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
        &c_claude_bin,
    ]);

    // Bind mounts
    cmd.arg(format!("-v={}:{c_claude_bin}:ro,z", claude_bin.display()));
    cmd.arg(format!("-v={}:{c_session_dir}:rw,z", session_dir.display()));
    cmd.arg(format!("-v={}:{c_claude_dir}:rw,z", claude_dir.display()));
    cmd.arg(format!("-v={}:{c_claude_json}:rw,z", claude_json.display()));

    // Environment
    cmd.args(["-e", &format!("HOME={home_str}")]);
    cmd.args(["-e", "LANG=en_US.UTF-8"]);
    cmd.args(["-e", &format!("USER={}", std::env::var("USER").unwrap_or_else(|_| "claude".into()))]);
    cmd.args(["-e", &format!("LOGNAME={}", std::env::var("USER").unwrap_or_else(|_| "claude".into()))]);
    cmd.args(["-e", "SHELL=/bin/bash"]);
    cmd.args(["-e", "DISABLE_AUTOUPDATER=1"]);

    // Working directory
    cmd.arg(format!("-w={c_session_dir}"));

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

    fn test_cmd(name: &str, image: &str, session_dir: &str, claude_args: &[String]) -> Vec<String> {
        let cmd = build_run_command(name, image, &PathBuf::from(session_dir), claude_args);
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn build_run_command_has_correct_container_name() {
        let args = test_cmd("test-session", "ubuntu:latest", "/tmp/s", &[]);
        assert!(args.contains(&"--name=margatroid-test-session".to_string()));
    }

    #[test]
    fn build_run_command_has_workdir() {
        let args = test_cmd("myproj", "debian", "/tmp/s", &[]);
        let home = home_dir();
        let expected = format!("-w={}/sessions/myproj", home.display());
        assert!(args.contains(&expected), "missing workdir, args: {args:?}");
    }

    #[test]
    fn build_run_command_passes_claude_args() {
        let claude_args = vec!["--name".to_string(), "my-session".to_string()];
        let args = test_cmd("test", "ubuntu", "/tmp/s", &claude_args);
        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"my-session".to_string()));
    }

    #[test]
    fn build_run_command_has_bind_mounts() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        let has_session_mount = args.iter().any(|a| a.contains("/tmp/s:") && a.starts_with("-v="));
        assert!(has_session_mount, "missing session dir bind mount, args: {args:?}");
    }

    #[test]
    fn build_run_command_sets_env_vars() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        let home = home_dir();
        let expected_home = format!("HOME={}", home.display());
        assert!(args.contains(&expected_home), "missing HOME env");
        assert!(args.contains(&"DISABLE_AUTOUPDATER=1".to_string()));
    }

    #[test]
    fn build_run_command_uses_entrypoint() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        let ep_idx = args.iter().position(|a| a == "--entrypoint").unwrap();
        let home = home_dir();
        let expected = format!("{}/.local/bin/claude", home.display());
        assert!(args[ep_idx + 1].contains(".local/bin/claude") || args[ep_idx + 1] == expected,
            "unexpected entrypoint: {}", args[ep_idx + 1]);
    }

    #[test]
    fn build_run_command_uses_current_uid() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        let uid = nix::unistd::getuid().as_raw();
        let gid = nix::unistd::getgid().as_raw();
        let expected_userns = format!("--userns=keep-id:uid={uid},gid={gid}");
        let expected_user = format!("--user={uid}:{gid}");
        assert!(args.contains(&expected_userns), "missing userns with current uid");
        assert!(args.contains(&expected_user), "missing user with current uid");
    }

    #[test]
    fn find_claude_bin_returns_path() {
        let bin = find_claude_bin();
        // Should return some path (either from PATH or fallback)
        assert!(!bin.as_os_str().is_empty());
    }
}
