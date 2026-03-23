//! TUI application state machine.

use crate::views::{confirm, create, detail, rename, session_list};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use margatroid::session::{self, Session};
use ratatui::prelude::*;
use std::io::stdout;

#[derive(Debug, Clone, PartialEq)]
pub enum View {
    SessionList,
    CreateImage,
    CreateName,
    CreateCustomImage,
    ConfirmHost,
    Detail(usize),
    ConfirmDelete(usize),
    Rename(usize),
}

pub struct App {
    pub sessions: Vec<Session>,
    pub view: View,
    pub cursor: usize,
    pub name_buf: String,
    pub custom_img_buf: String,
    pub selected_image: String,
    pub delete_data: bool,
    pub status_message: Option<String>,
}

impl App {
    pub fn new() -> Self {
        let sessions = session::list_all().unwrap_or_default();
        Self {
            sessions,
            view: View::SessionList,
            cursor: 0,
            name_buf: String::new(),
            custom_img_buf: String::new(),
            selected_image: String::new(),
            delete_data: false,
            status_message: None,
        }
    }

    pub fn refresh_sessions(&mut self) {
        self.sessions = session::list_all().unwrap_or_default();
    }

    /// Get the image items for the create flow.
    pub fn image_items(&self) -> Vec<String> {
        vec![
            "ubuntu".to_string(),
            "alpine".to_string(),
            "Other image...".to_string(),
            "No container (host)".to_string(),
        ]
    }
}

/// Launch result from the TUI event loop.
pub enum RunResult {
    Launch { name: String, image: String },
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    let mut last_refresh = std::time::Instant::now();

    loop {
        let result = loop {
            terminal.draw(|f| draw(&app, f))?;

            // Auto-refresh session list every 3 seconds
            if last_refresh.elapsed() >= std::time::Duration::from_secs(3)
                && app.view == View::SessionList
            {
                let old_cursor = app.cursor;
                app.refresh_sessions();
                app.cursor = old_cursor.min(app.sessions.len().saturating_sub(1));
                last_refresh = std::time::Instant::now();
            }

            // Poll with 1s timeout so we can auto-refresh even without input
            if !event::poll(std::time::Duration::from_secs(1))? {
                continue;
            }

            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if let Some(RunResult::Launch { name, image }) = handle_key(&mut app, key.code) {
                        break RunResult::Launch { name, image };
                    }
                }
                Event::Resize(_, _) => {
                    // tmux sends SIGWINCH on attach — force a full redraw
                    // so the screen isn't blank after reattaching.
                    terminal.clear()?;
                }
                _ => {}
            }
        };

        match result {
            RunResult::Launch { name, image } => {
                // Setup session and launch in a new tmux window.
                // The manager pane stays alive — sessions always open in their own pane.
                if let Err(e) = session::setup(&name, &image) {
                    disable_raw_mode()?;
                    return Err(format!("Setup failed: {e}").into());
                }
                if let Err(e) = margatroid::state::register(&name, &image) {
                    disable_raw_mode()?;
                    return Err(format!("Registration failed: {e}").into());
                }
                let tui_bin = margatroid::margatroid_dir().join("bin/margatroid-tui");
                let tui_path = tui_bin.to_string_lossy().into_owned();

                if let Err(e) = margatroid::tmux::new_window(&name, &[&tui_path, &name, &image]) {
                    disable_raw_mode()?;
                    return Err(format!("Failed to start: {e}").into());
                }

                // Refresh and loop back to TUI instead of recursing
                app.refresh_sessions();
                app.view = View::SessionList;
                last_refresh = std::time::Instant::now();
            }
        }
    }
}

fn draw(app: &App, frame: &mut Frame) {
    match &app.view {
        View::SessionList => session_list::draw(app, frame),
        View::CreateImage => create::draw_image(app, frame),
        View::CreateName => create::draw_name(app, frame),
        View::CreateCustomImage => create::draw_custom_image(app, frame),
        View::ConfirmHost => create::draw_confirm_host(app, frame),
        View::Detail(idx) => detail::draw(app, *idx, frame),
        View::ConfirmDelete(idx) => confirm::draw(app, *idx, frame),
        View::Rename(idx) => rename::draw(app, *idx, frame),
    }
}

fn handle_key(app: &mut App, key: KeyCode) -> Option<RunResult> {
    match &app.view {
        View::SessionList => session_list::handle_key(app, key),
        View::CreateImage => create::handle_key_image(app, key),
        View::CreateName => create::handle_key_name(app, key),
        View::CreateCustomImage => create::handle_key_custom_image(app, key),
        View::ConfirmHost => {
            create::handle_key_confirm_host(app, key);
            None
        }
        View::Detail(_) => {
            detail::handle_key(app, key);
            None
        }
        View::ConfirmDelete(_) => {
            confirm::handle_key(app, key);
            None
        }
        View::Rename(_) => {
            rename::handle_key(app, key);
            None
        }
    }
}
