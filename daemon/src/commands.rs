//! Parse and dispatch text commands from the remote control interface.

use margatroid::session::{self, SessionStatus};

use margatroid::image::is_valid_session_name;

/// Handle a text command and return a Markdown-formatted response.
pub fn handle_command(input: &str) -> String {
    let input = input.trim();
    let parts: Vec<&str> = input.split_whitespace().collect();

    if parts.is_empty() {
        return help_text();
    }

    // Strip leading / if present
    let cmd = parts[0].strip_prefix('/').unwrap_or(parts[0]);

    match cmd.to_lowercase().as_str() {
        "list" | "ls" => cmd_list(),
        "start" => parse_start(&parts),
        "stop" => {
            if parts.len() < 2 {
                "Usage: `/stop <name>`".to_string()
            } else if !is_valid_session_name(parts[1]) {
                "Invalid name: use only letters, numbers, hyphens, underscores.".to_string()
            } else {
                cmd_stop(parts[1])
            }
        }
        "restart" => {
            if parts.len() < 2 {
                "Usage: `/restart <name>`".to_string()
            } else if !is_valid_session_name(parts[1]) {
                "Invalid name: use only letters, numbers, hyphens, underscores.".to_string()
            } else {
                cmd_restart(parts[1])
            }
        }
        "delete" => {
            if parts.len() < 2 {
                "Usage: `/delete <name> [--data]`".to_string()
            } else if !is_valid_session_name(parts[1]) {
                "Invalid name: use only letters, numbers, hyphens, underscores.".to_string()
            } else {
                let remove_data = parts.contains(&"--data");
                cmd_delete(parts[1], remove_data)
            }
        }
        "info" => {
            if parts.len() < 2 {
                "Usage: `/info <name>`".to_string()
            } else if !is_valid_session_name(parts[1]) {
                "Invalid name: use only letters, numbers, hyphens, underscores.".to_string()
            } else {
                cmd_info(parts[1])
            }
        }
        "help" | "?" => help_text(),
        _ => format!(
            "Unknown command: `{}`. Type `/help` for available commands.",
            parts[0]
        ),
    }
}

fn cmd_list() -> String {
    match session::list_all() {
        Ok(sessions) => {
            if sessions.is_empty() {
                return "No sessions found.".to_string();
            }

            let mut out = "## Sessions\n\n| Name | Image | Status | Container |\n|------|-------|--------|-----------|\n".to_string();

            let mut running = 0;
            let mut stopped = 0;

            for s in &sessions {
                let status = match s.status {
                    SessionStatus::Running => {
                        running += 1;
                        "Running"
                    }
                    SessionStatus::Stopped => {
                        stopped += 1;
                        "Stopped"
                    }
                };
                let container = s.container_id.as_deref().unwrap_or("-");
                out.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    s.name, s.image, status, container
                ));
            }

            out.push_str(&format!(
                "\n{} sessions ({} running, {} stopped)",
                sessions.len(),
                running,
                stopped
            ));
            out
        }
        Err(e) => format!("Error listing sessions: {e}"),
    }
}

fn parse_start(parts: &[&str]) -> String {
    if parts.len() < 3 {
        return "Usage: `/start <name> <--image=IMAGE|--host>`\n\n\
                Examples:\n\
                - `/start myproject --image=ubuntu`\n\
                - `/start myproject --image=node:22`\n\
                - `/start myproject --host`"
            .to_string();
    }
    let name = parts[1];
    if !is_valid_session_name(name) {
        return "Invalid name: use only letters, numbers, hyphens, underscores.".to_string();
    }
    // Look for --image= or --host in remaining args
    let mut image: Option<&str> = None;
    let mut host = false;
    for &part in &parts[2..] {
        if let Some(img) = part.strip_prefix("--image=") {
            image = Some(img);
        } else if part == "--host" {
            host = true;
        }
    }
    if host {
        cmd_start(name, "host")
    } else if let Some(img) = image {
        if img.is_empty() {
            return "Usage: `--image=IMAGE` requires a value".to_string();
        }
        cmd_start(name, img)
    } else {
        "Usage: `/start <name> <--image=IMAGE|--host>`".to_string()
    }
}

fn cmd_start(name: &str, image: &str) -> String {
    // Setup and register (don't exec — we're a daemon)
    match session::setup(name, image) {
        Ok(_) => {}
        Err(e) => return format!("Setup failed: {e}"),
    }

    if let Err(e) = margatroid::state::register(name, image) {
        return format!("Registration failed: {e}");
    }

    // Launch in a new tmux window
    let tui_bin = margatroid::margatroid_dir().join("bin/margatroid-tui");
    let tui_path = tui_bin.to_string_lossy().into_owned();

    match margatroid::tmux::new_window(name, &[&tui_path, name, image]) {
        Ok(_) => format!("Started session `{name}` with image `{image}`"),
        Err(e) => format!("Failed to start: {e}"),
    }
}

fn cmd_stop(name: &str) -> String {
    match session::stop(name) {
        Ok(_) => format!("Stopped session `{name}`"),
        Err(e) => format!("Failed to stop: {e}"),
    }
}

fn cmd_restart(name: &str) -> String {
    match session::restart(name) {
        Ok(_) => format!("Restarted session `{name}`"),
        Err(e) => format!("Failed to restart: {e}"),
    }
}

fn cmd_delete(name: &str, remove_data: bool) -> String {
    match session::delete(name, remove_data) {
        Ok(_) => {
            if remove_data {
                format!("Deleted session `{name}` and its data")
            } else {
                format!("Deleted session `{name}` (data preserved)")
            }
        }
        Err(e) => format!("Failed to delete: {e}"),
    }
}

fn cmd_info(name: &str) -> String {
    match session::list_all() {
        Ok(sessions) => {
            match sessions.iter().find(|s| s.name == name) {
                Some(s) => {
                    let status = match s.status {
                        SessionStatus::Running => "Running",
                        SessionStatus::Stopped => "Stopped",
                    };
                    let container = s.container_id.as_deref().unwrap_or("none");
                    let uuid = s.last_uuid.as_deref().unwrap_or("none");

                    format!(
                        "## Session: {}\n\n\
                         - **Image**: {}\n\
                         - **Status**: {}\n\
                         - **Container**: `{}`\n\
                         - **Directory**: `{}`\n\
                         - **Last Session**: `{}`",
                        s.name,
                        s.image,
                        status,
                        container,
                        s.session_dir.display(),
                        uuid,
                    )
                }
                None => format!("Session `{name}` not found"),
            }
        }
        Err(e) => format!("Error: {e}"),
    }
}

fn help_text() -> String {
    "## Commands\n\n\
     | Command | Description |\n\
     |---------|-------------|\n\
     | `/list` | List all sessions |\n\
     | `/start <name> --image=IMAGE` | Start a containerized session |\n\
     | `/start <name> --host` | Start an uncontainerized session |\n\
     | `/stop <name>` | Stop a running session |\n\
     | `/restart <name>` | Restart a session |\n\
     | `/delete <name> [--data]` | Delete a session |\n\
     | `/info <name>` | Show session details |\n\
     | `/help` | Show this help |"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_command() {
        let result = handle_command("/help");
        assert!(result.contains("Commands"));
        assert!(result.contains("/list"));
    }

    #[test]
    fn help_with_question_mark() {
        let result = handle_command("?");
        assert!(result.contains("Commands"));
    }

    #[test]
    fn empty_input_shows_help() {
        let result = handle_command("");
        assert!(result.contains("Commands"));
    }

    #[test]
    fn unknown_command() {
        let result = handle_command("/foobar");
        assert!(result.contains("Unknown command"));
        assert!(result.contains("/foobar"));
    }

    #[test]
    fn start_missing_args() {
        let result = handle_command("/start");
        assert!(result.contains("Usage:"));
    }

    #[test]
    fn start_missing_mode() {
        let result = handle_command("/start myproject");
        assert!(result.contains("Usage:"));
    }

    #[test]
    fn start_invalid_name() {
        let result = handle_command("/start bad;name --host");
        assert!(result.contains("Invalid name"));
    }

    #[test]
    fn start_with_special_chars() {
        let result = handle_command("/start ../escape --host");
        assert!(result.contains("Invalid name"));
    }

    #[test]
    fn start_empty_image() {
        let result = handle_command("/start myproject --image=");
        assert!(result.contains("requires a value"));
    }

    #[test]
    fn stop_missing_name() {
        let result = handle_command("/stop");
        assert!(result.contains("Usage:"));
    }

    #[test]
    fn stop_invalid_name() {
        let result = handle_command("/stop bad;name");
        assert!(result.contains("Invalid name"));
    }

    #[test]
    fn restart_missing_name() {
        let result = handle_command("/restart");
        assert!(result.contains("Usage:"));
    }

    #[test]
    fn delete_missing_name() {
        let result = handle_command("/delete");
        assert!(result.contains("Usage:"));
    }

    #[test]
    fn delete_invalid_name() {
        let result = handle_command("/delete path/traversal");
        assert!(result.contains("Invalid name"));
    }

    #[test]
    fn info_missing_name() {
        let result = handle_command("/info");
        assert!(result.contains("Usage:"));
    }

    #[test]
    fn info_invalid_name() {
        let result = handle_command("/info semi;colon");
        assert!(result.contains("Invalid name"));
    }

    #[test]
    fn command_without_slash_prefix() {
        let result = handle_command("help");
        assert!(result.contains("Commands"));
    }

    #[test]
    fn command_case_insensitive() {
        let result = handle_command("/HELP");
        assert!(result.contains("Commands"));

        let result2 = handle_command("/List");
        // /list will try to list sessions, not return "Unknown command"
        assert!(!result2.contains("Unknown command"));
    }

    #[test]
    fn whitespace_trimmed() {
        let result = handle_command("  /help  ");
        assert!(result.contains("Commands"));
    }
}
