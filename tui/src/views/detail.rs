//! Session detail view.

use crate::app::{App, View};
use crossterm::event::KeyCode;
use orchestrator::session::SessionStatus;
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
        Constraint::Min(8),
        Constraint::Length(2),
    ])
    .split(area);

    let title = Paragraph::new(format!(" Session: {}", session.name))
        .style(Style::default().fg(Color::Cyan).bold());
    frame.render_widget(title, chunks[0]);

    let status_str = match session.status {
        SessionStatus::Running => "Running",
        SessionStatus::Stopped => "Stopped",
    };

    let container = session.container_id.as_deref().unwrap_or("none");
    let uuid = session.last_uuid.as_deref().unwrap_or("none");
    let has_claude_md = session.session_dir.join("CLAUDE.md").exists();

    let info = vec![
        Line::from(vec![
            Span::styled("  Image:         ", Style::default().fg(Color::DarkGray)),
            Span::raw(&session.image),
        ]),
        Line::from(vec![
            Span::styled("  Status:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                status_str,
                if session.status == SessionStatus::Running {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
        ]),
        Line::from(vec![
            Span::styled("  Container:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(container),
        ]),
        Line::from(vec![
            Span::styled("  Directory:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(session.session_dir.display().to_string()),
        ]),
        Line::from(vec![
            Span::styled("  Last Session:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(uuid),
        ]),
        Line::from(vec![
            Span::styled("  Has CLAUDE.md: ", Style::default().fg(Color::DarkGray)),
            Span::raw(if has_claude_md { "Yes" } else { "No" }),
        ]),
    ];

    let detail = Paragraph::new(info).block(Block::default().borders(Borders::TOP));
    frame.render_widget(detail, chunks[1]);

    let help = Paragraph::new(" Esc back")
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(help, chunks[2]);
}

pub fn handle_key(app: &mut App, key: KeyCode) {
    if key == KeyCode::Esc {
        app.view = View::SessionList;
    }
}
