//! Create session flow: image selection → name entry.

use crate::app::{App, RunResult, View};
use crossterm::event::KeyCode;
use margatroid::image::is_valid_session_name;
use ratatui::prelude::*;
use ratatui::widgets::*;

// --- Image selection ---

pub fn draw_image(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(2),
    ])
    .split(area);

    let title = Paragraph::new(" Claude Session Manager")
        .style(Style::default().fg(Color::Cyan).bold());
    frame.render_widget(title, chunks[0]);

    let label = Paragraph::new("  Select base image:")
        .style(Style::default().fg(Color::White));
    frame.render_widget(label, chunks[1]);

    let items = app.image_items();
    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let style = if i == app.cursor {
                Style::default().bg(Color::Cyan).fg(Color::Black)
            } else if i == items.len() - 1 {
                // "Enter custom image..." in different color
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            ListItem::new(format!("  {name}")).style(style)
        })
        .collect();

    let list = List::new(list_items);
    frame.render_widget(list, chunks[2]);

    let help = Paragraph::new(" ↑↓ navigate  Enter select  Esc back")
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(help, chunks[3]);
}

pub fn handle_key_image(app: &mut App, key: KeyCode) -> Option<RunResult> {
    let items = app.image_items();
    match key {
        KeyCode::Esc => {
            app.cursor = 0;
            app.view = View::SessionList;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.cursor > 0 {
                app.cursor -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.cursor < items.len() - 1 {
                app.cursor += 1;
            }
        }
        KeyCode::Enter => {
            if app.cursor == items.len() - 1 {
                // Custom image
                app.custom_img_buf.clear();
                app.view = View::CreateCustomImage;
            } else {
                app.selected_image = items[app.cursor].clone();
                app.name_buf.clear();
                app.view = View::CreateName;
            }
        }
        _ => {}
    }
    None
}

// --- Custom image entry ---

pub fn draw_custom_image(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);

    let title = Paragraph::new(" Claude Session Manager")
        .style(Style::default().fg(Color::Cyan).bold());
    frame.render_widget(title, chunks[0]);

    let label = Paragraph::new("  Image reference (e.g. debian, node:22, ghcr.io/org/img:tag):")
        .style(Style::default().fg(Color::White));
    frame.render_widget(label, chunks[1]);

    let input = Paragraph::new(format!("    {}_", app.custom_img_buf))
        .style(Style::default().fg(Color::Green));
    frame.render_widget(input, chunks[2]);

    let help = Paragraph::new(" Enter confirm  Esc back")
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(help, chunks[5]);
}

pub fn handle_key_custom_image(app: &mut App, key: KeyCode) -> Option<RunResult> {
    match key {
        KeyCode::Esc => {
            app.cursor = 0;
            app.view = View::CreateImage;
        }
        KeyCode::Enter => {
            let img = app.custom_img_buf.trim().to_string();
            if !img.is_empty() {
                app.selected_image = img;
                app.name_buf.clear();
                app.view = View::CreateName;
            }
        }
        KeyCode::Backspace => {
            app.custom_img_buf.pop();
        }
        KeyCode::Char(c) if !c.is_control() => {
            app.custom_img_buf.push(c);
        }
        _ => {}
    }
    None
}

// --- Name entry ---

pub fn draw_name(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);

    let title = Paragraph::new(" Claude Session Manager")
        .style(Style::default().fg(Color::Cyan).bold());
    frame.render_widget(title, chunks[0]);

    let label = Paragraph::new(format!("  Session name (image: {}):", app.selected_image))
        .style(Style::default().fg(Color::White));
    frame.render_widget(label, chunks[1]);

    let input = Paragraph::new(format!("    {}_", app.name_buf))
        .style(Style::default().fg(Color::Green));
    frame.render_widget(input, chunks[2]);

    let hint = Paragraph::new("  (letters, numbers, hyphens, underscores)")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint, chunks[4]);

    let help = Paragraph::new(" Enter confirm  Esc back")
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(help, chunks[6]);
}

pub fn handle_key_name(app: &mut App, key: KeyCode) -> Option<RunResult> {
    match key {
        KeyCode::Esc => {
            app.cursor = 0;
            app.view = View::CreateImage;
        }
        KeyCode::Enter => {
            let name = app.name_buf.trim().to_string();
            if is_valid_session_name(&name) {
                return Some(RunResult::Launch {
                    name,
                    image: app.selected_image.clone(),
                });
            }
        }
        KeyCode::Backspace => {
            app.name_buf.pop();
        }
        KeyCode::Char(c) if c.is_alphanumeric() || c == '-' || c == '_' => {
            app.name_buf.push(c);
        }
        _ => {}
    }
    None
}
