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
/// Returns the canonicalized (symlinks resolved) path.
pub fn find_claude_bin() -> std::path::PathBuf {
    // Check PATH first
    if let Ok(output) = Command::new("which").arg("claude").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                let p = std::path::PathBuf::from(&path);
                // Resolve symlinks so we mount the real file/directory
                return std::fs::canonicalize(&p).unwrap_or(p);
            }
        }
    }
    // Fallback
    let fallback = home_dir().join(".local/bin/claude");
    std::fs::canonicalize(&fallback).unwrap_or(fallback)
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
    let claude_bin_dir = claude_bin.parent().unwrap_or(&claude_bin);
    let claude_bin_name = claude_bin.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "claude".to_string());
    let claude_json = home.join(".claude.json");
    let claude_dir = home.join(".claude");
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());
    let user = std::env::var("USER").unwrap_or_else(|_| "claude".into());

    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();

    // Container-internal paths mirror the host so bind mounts preserve permissions.
    let c_bin_dir = claude_bin_dir.to_string_lossy();
    let c_session_dir = format!("{home_str}/sessions/{name}");
    let c_claude_dir = format!("{home_str}/.claude");
    let c_claude_json = format!("{home_str}/.claude.json");

    let mut cmd = Command::new("podman");
    cmd.args([
        "run",
        "--rm",
        "-it",
        "--init",
        "--security-opt", "label=disable",
        &format!("--name=margatroid-{name}"),
        &format!("--hostname={hostname}"),
        &format!("--userns=keep-id:uid={uid},gid={gid}"),
        &format!("--user={uid}:{gid}"),
    ]);

    // Mount the directory containing claude (not the file itself) to avoid
    // SELinux relabeling and symlink issues that cause Permission denied.
    cmd.arg(format!("-v={c_bin_dir}:{c_bin_dir}:ro"));
    cmd.arg(format!("-v={}:{c_session_dir}:rw", session_dir.display()));
    cmd.arg(format!("-v={}:{c_claude_dir}:rw", claude_dir.display()));
    cmd.arg(format!("-v={}:{c_claude_json}:rw", claude_json.display()));

    // Environment
    cmd.args(["-e", &format!("HOME={home_str}")]);
    cmd.args(["-e", &format!("PATH={c_bin_dir}:/usr/local/bin:/usr/bin:/bin")]);
    cmd.args(["-e", "LANG=en_US.UTF-8"]);
    cmd.args(["-e", &format!("USER={user}")]);
    cmd.args(["-e", &format!("LOGNAME={user}")]);
    cmd.args(["-e", "SHELL=/bin/bash"]);
    cmd.args(["-e", "DISABLE_AUTOUPDATER=1"]);

    // Working directory
    cmd.arg(format!("-w={c_session_dir}"));

    // Image
    cmd.arg(image);

    // Command: run claude directly (not via --entrypoint, which has
    // issues with catatonit + bind-mounted binaries on some systems)
    cmd.arg(&claude_bin_name);
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
    fn build_run_command_has_claude_command() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        // claude binary name should appear after the image name
        let img_idx = args.iter().position(|a| a == "ubuntu").unwrap();
        let cmd_name = &args[img_idx + 1];
        assert!(cmd_name.contains("claude"), "expected claude command after image, got: {cmd_name}");
    }

    #[test]
    fn build_run_command_has_no_entrypoint() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        assert!(!args.contains(&"--entrypoint".to_string()),
            "should not use --entrypoint, args: {args:?}");
    }

    #[test]
    fn build_run_command_mounts_bin_dir_readonly() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        // The claude binary's parent directory should be mounted read-only (no :z)
        let ro_mount = args.iter().find(|a| a.starts_with("-v=") && a.ends_with(":ro"));
        assert!(ro_mount.is_some(), "missing read-only bin dir mount, args: {args:?}");
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
