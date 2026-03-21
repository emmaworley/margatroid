//! Main session list view.

use crate::app::{App, RunResult, View};
use crossterm::event::KeyCode;
use margatroid::session::SessionStatus;
use ratatui::prelude::*;
use ratatui::widgets::*;

pub fn draw(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3), // title
        Constraint::Min(5),   // table
        Constraint::Length(2), // help
    ])
    .split(area);

    // Title
    let title = Paragraph::new(" Claude Session Manager")
        .style(Style::default().fg(Color::Cyan).bold());
    frame.render_widget(title, chunks[0]);

    // Session table
    let header = Row::new(vec!["Name", "Image", "Status", "Container"])
        .style(Style::default().bold().fg(Color::White))
        .bottom_margin(1);

    let rows: Vec<Row> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let status_str = match s.status {
                SessionStatus::Running => "Running",
                SessionStatus::Stopped => "Stopped",
            };
            let status_style = match s.status {
                SessionStatus::Running => Style::default().fg(Color::Green),
                SessionStatus::Stopped => Style::default().fg(Color::DarkGray),
            };
            let container = s.container_id.as_deref().unwrap_or("-");

            let row = Row::new(vec![
                Cell::from(s.name.as_str()),
                Cell::from(s.image.as_str()),
                Cell::from(status_str).style(status_style),
                Cell::from(container),
            ]);

            if i == app.cursor {
                row.style(Style::default().bg(Color::DarkGray))
            } else {
                row
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Percentage(15),
            Constraint::Percentage(25),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::TOP));

    frame.render_widget(table, chunks[1]);

    // Help bar
    let help = Paragraph::new(
        " ↑↓ navigate  Enter resume  n new  s stop  r restart  d delete  i info",
    )
    .style(Style::default().fg(Color::Yellow));
    frame.render_widget(help, chunks[2]);

    // Status message
    if let Some(msg) = &app.status_message {
        let status = Paragraph::new(msg.as_str())
            .style(Style::default().fg(Color::Red))
            .alignment(Alignment::Right);
        let status_area = Rect {
            x: area.width / 2,
            y: 0,
            width: area.width / 2,
            height: 1,
        };
        frame.render_widget(status, status_area);
    }
}

pub fn handle_key(app: &mut App, key: KeyCode) -> Option<RunResult> {
    app.status_message = None;

    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            if app.cursor > 0 {
                app.cursor -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !app.sessions.is_empty() && app.cursor < app.sessions.len() - 1 {
                app.cursor += 1;
            }
        }
        KeyCode::Char('n') => {
            app.cursor = 0;
            app.view = View::CreateImage;
        }
        KeyCode::Enter => {
            if let Some(session) = app.sessions.get(app.cursor) {
                if session.status == SessionStatus::Stopped {
                    return Some(RunResult::Launch {
                        name: session.name.clone(),
                        image: session.image.clone(),
                    });
                } else {
                    app.status_message = Some("Session already running".to_string());
                }
            }
        }
        KeyCode::Char('s') => {
            if let Some(session) = app.sessions.get(app.cursor) {
                if session.status == SessionStatus::Running {
                    let name = session.name.clone();
                    if let Err(e) = margatroid::session::stop(&name) {
                        app.status_message = Some(format!("Stop failed: {e}"));
                    } else {
                        app.refresh_sessions();
                        app.status_message = Some(format!("Stopped {name}"));
                    }
                }
            }
        }
        KeyCode::Char('r') => {
            if let Some(session) = app.sessions.get(app.cursor) {
                let name = session.name.clone();
                if let Err(e) = margatroid::session::restart(&name) {
                    app.status_message = Some(format!("Restart failed: {e}"));
                } else {
                    app.refresh_sessions();
                    app.status_message = Some(format!("Restarted {name}"));
                }
            }
        }
        KeyCode::Char('d') => {
            if !app.sessions.is_empty() {
                app.delete_data = false;
                app.view = View::ConfirmDelete(app.cursor);
            }
        }
        KeyCode::Char('i') => {
            if !app.sessions.is_empty() {
                app.view = View::Detail(app.cursor);
            }
        }
        _ => {}
    }
    None
}
