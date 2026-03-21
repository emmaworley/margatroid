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
///
/// Container layout:
///   /home/<name>/              ← $HOME (session_dir on host)
///     .claude.json             ← per-session config (seeded by setup_session)
///     .claude/.credentials.json ← ro mount from host credentials
///     CLAUDE.md                ← session instructions
///     (session working files)
///   <claude_bin_dir>/          ← ro mount of claude binary's directory
pub fn build_run_command(
    name: &str,
    image: &str,
    session_dir: &Path,
    claude_args: &[String],
) -> Command {
    let home = home_dir();
    let claude_bin = find_claude_bin();
    let claude_bin_dir = claude_bin.parent().unwrap_or(&claude_bin);
    let claude_bin_name = claude_bin.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "claude".to_string());
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();

    // Container paths
    let c_home = format!("/home/{name}");
    let c_bin_dir = claude_bin_dir.to_string_lossy();

    // Host credentials path (ro mounted into container)
    let host_creds = home.join(".claude/.credentials.json");

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

    // Mounts:
    // - Session dir as container home (rw) — contains .claude.json, CLAUDE.md, working files
    // - Host credentials as ro mount inside the session's .claude/
    // - Claude binary directory (ro)
    cmd.arg(format!("-v={}:{c_home}:rw", session_dir.display()));
    cmd.arg(format!("-v={}:{c_home}/.claude/.credentials.json:ro", host_creds.display()));
    cmd.arg(format!("-v={c_bin_dir}:{c_bin_dir}:ro"));

    // Environment
    cmd.args(["-e", &format!("HOME={c_home}")]);
    cmd.args(["-e", &format!("PATH={c_bin_dir}:/usr/local/bin:/usr/bin:/bin")]);
    cmd.args(["-e", "LANG=en_US.UTF-8"]);
    cmd.args(["-e", &format!("USER={name}")]);
    cmd.args(["-e", &format!("LOGNAME={name}")]);
    cmd.args(["-e", "SHELL=/bin/bash"]);
    cmd.args(["-e", "DISABLE_AUTOUPDATER=1"]);

    // Working directory
    cmd.arg(format!("-w={c_home}"));

    // Image
    cmd.arg(image);

    // Command
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
    fn container_name() {
        let args = test_cmd("test-session", "ubuntu:latest", "/tmp/s", &[]);
        assert!(args.contains(&"--name=margatroid-test-session".to_string()));
    }

    #[test]
    fn home_is_session_name() {
        let args = test_cmd("myproj", "debian", "/tmp/s", &[]);
        assert!(args.contains(&"HOME=/home/myproj".to_string()));
        assert!(args.contains(&"-w=/home/myproj".to_string()));
        assert!(args.contains(&"USER=myproj".to_string()));
    }

    #[test]
    fn session_dir_mounted_as_home() {
        let args = test_cmd("box", "ubuntu", "/tmp/sessions/box", &[]);
        let mount = args.iter().find(|a| a.starts_with("-v=/tmp/sessions/box:"));
        assert!(mount.is_some(), "missing session dir mount, args: {args:?}");
        assert!(mount.unwrap().contains("/home/box:rw"), "session dir should mount as /home/<name>:rw");
    }

    #[test]
    fn credentials_mounted_readonly() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        let creds_mount = args.iter().find(|a| a.contains(".credentials.json") && a.ends_with(":ro"));
        assert!(creds_mount.is_some(), "missing ro credentials mount, args: {args:?}");
    }

    #[test]
    fn no_host_claude_json_mounted() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        // Should NOT mount the host's ~/.claude.json or ~/.claude/ directly
        let home = home_dir();
        let host_claude_json = format!("{}/.claude.json", home.display());
        let has_host_config = args.iter().any(|a| a.starts_with("-v=") && a.contains(&host_claude_json) && !a.contains(".credentials.json"));
        assert!(!has_host_config, "should not mount host ~/.claude.json, args: {args:?}");
    }

    #[test]
    fn claude_binary_dir_readonly() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        let bin_dir = find_claude_bin();
        let parent = bin_dir.parent().unwrap().to_string_lossy();
        let mount = args.iter().find(|a| a.starts_with(&format!("-v={parent}:")) && a.ends_with(":ro"));
        assert!(mount.is_some(), "missing ro claude bin dir mount, args: {args:?}");
    }

    #[test]
    fn passes_claude_args() {
        let claude_args = vec!["--name".to_string(), "my-session".to_string()];
        let args = test_cmd("test", "ubuntu", "/tmp/s", &claude_args);
        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"my-session".to_string()));
    }

    #[test]
    fn no_entrypoint() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        assert!(!args.contains(&"--entrypoint".to_string()));
    }

    #[test]
    fn uses_current_uid() {
        let args = test_cmd("test", "ubuntu", "/tmp/s", &[]);
        let uid = nix::unistd::getuid().as_raw();
        let gid = nix::unistd::getgid().as_raw();
        assert!(args.contains(&format!("--userns=keep-id:uid={uid},gid={gid}")));
        assert!(args.contains(&format!("--user={uid}:{gid}")));
    }

    #[test]
    fn find_claude_bin_returns_path() {
        let bin = find_claude_bin();
        assert!(!bin.as_os_str().is_empty());
    }
}
