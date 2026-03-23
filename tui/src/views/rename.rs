//! Session rename dialog.

use crate::app::{App, View};
use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::*;

pub fn draw(app: &App, idx: usize, frame: &mut Frame) {
    let area = frame.area();

    let session = match app.sessions.get(idx) {
        Some(s) => s,
        None => {
            frame.render_widget(Paragraph::new("Session not found"), area);
            return;
        }
    };

    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);

    let title = Paragraph::new(format!(" Rename session \"{}\"", session.name))
        .style(Style::default().fg(Color::Cyan).bold());
    frame.render_widget(title, chunks[0]);

    let input = Paragraph::new(format!("  New name: {}_", app.name_buf))
        .style(Style::default().fg(Color::White));
    frame.render_widget(input, chunks[1]);

    let help = Paragraph::new(" Enter confirm  Esc cancel")
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(help, chunks[3]);
}

pub fn handle_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => {
            app.name_buf.clear();
            app.view = View::SessionList;
        }
        KeyCode::Enter => {
            let new_name = app.name_buf.trim().to_string();
            if let View::Rename(idx) = app.view {
                if let Some(session) = app.sessions.get(idx) {
                    if new_name.is_empty() {
                        app.status_message = Some("Name cannot be empty".to_string());
                    } else {
                        let old_name = session.name.clone();
                        match margatroid::session::rename(&old_name, &new_name) {
                            Ok(()) => {
                                app.status_message =
                                    Some(format!("Renamed {old_name} → {new_name}"));
                            }
                            Err(e) => {
                                app.status_message = Some(format!("Rename failed: {e}"));
                            }
                        }
                        app.refresh_sessions();
                        app.cursor = app.cursor.min(app.sessions.len().saturating_sub(1));
                    }
                }
            }
            app.name_buf.clear();
            app.view = View::SessionList;
        }
        KeyCode::Backspace => {
            app.name_buf.pop();
        }
        KeyCode::Char(c) => {
            app.name_buf.push(c);
        }
        _ => {}
    }
}
