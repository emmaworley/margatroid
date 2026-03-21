//! Delete confirmation dialog.

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
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);

    let title = Paragraph::new(format!(" Delete session \"{}\"?", session.name))
        .style(Style::default().fg(Color::Red).bold());
    frame.render_widget(title, chunks[0]);

    let checkbox = if app.delete_data { "[x]" } else { "[ ]" };
    let data_line = Paragraph::new(format!(
        "  {checkbox} Also delete session data from disk\n      ({})",
        session.session_dir.display()
    ))
    .style(Style::default().fg(Color::White));
    frame.render_widget(data_line, chunks[1]);

    let help = Paragraph::new(" Space toggle  Enter confirm  Esc cancel")
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(help, chunks[4]);
}

pub fn handle_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => {
            app.view = View::SessionList;
        }
        KeyCode::Char(' ') => {
            app.delete_data = !app.delete_data;
        }
        KeyCode::Enter => {
            if let View::ConfirmDelete(idx) = app.view {
                if let Some(session) = app.sessions.get(idx) {
                    let name = session.name.clone();
                    let remove_data = app.delete_data;
                    if let Err(e) = margatroid::session::delete(&name, remove_data) {
                        app.status_message = Some(format!("Delete failed: {e}"));
                    } else {
                        app.status_message = Some(format!("Deleted {name}"));
                    }
                    app.refresh_sessions();
                    app.cursor = app.cursor.min(app.sessions.len().saturating_sub(1));
                }
            }
            app.view = View::SessionList;
        }
        _ => {}
    }
}
