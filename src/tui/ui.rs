//! UI rendering for the TUI.

use super::App;
use ansi_to_tui::IntoText;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};

/// Render the full UI
pub fn render(frame: &mut Frame, app: &App, input_area_height: u16) {
    let chunks = Layout::vertical([
        Constraint::Length(1),                     // Status bar
        Constraint::Min(5),                        // Chat area
        Constraint::Length(input_area_height + 2), // Input area (textarea + border)
    ])
    .split(frame.area());

    render_status_bar(frame, app, chunks[0]);
    render_chat_area(frame, app, chunks[1]);
    // Input area is rendered by caller (textarea widget)
}

/// Get the input area rect for external rendering
pub fn get_input_area(frame: &Frame, input_area_height: u16) -> Rect {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(input_area_height + 2),
    ])
    .split(frame.area());
    chunks[2]
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let activity_style = if app.activity().is_busy() {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };

    let status_line = Line::from(vec![
        Span::styled(
            "[clemini] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(&app.model, Style::default().fg(Color::Green)),
        Span::raw(" | "),
        Span::styled(
            format!("~{}k tokens", app.estimated_tokens / 1000),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" | "),
        Span::styled(
            format!("#{}", app.interaction_count),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" | "),
        Span::styled(app.activity().display(), activity_style),
    ]);

    let status_bar = Paragraph::new(status_line).style(Style::default().bg(Color::DarkGray));

    frame.render_widget(status_bar, area);
}

fn render_chat_area(frame: &mut Frame, app: &App, area: Rect) {
    // Join all chat lines and convert ANSI codes to ratatui styles
    let chat_content = app
        .chat_lines()
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");

    // Parse ANSI codes into ratatui Text (with proper styling)
    let chat_text = chat_content
        .into_text()
        .unwrap_or_else(|_| Text::raw(&chat_content));

    let total_lines = chat_text.lines.len();
    let visible_height = area.height.saturating_sub(2) as usize; // Account for borders

    // Calculate scroll position (offset from bottom)
    let scroll_offset = app.scroll_offset() as usize;
    let max_scroll = total_lines.saturating_sub(visible_height);
    let effective_scroll = scroll_offset.min(max_scroll);

    // Scroll position from top (for rendering)
    let scroll_from_top = max_scroll.saturating_sub(effective_scroll);

    let chat = Paragraph::new(chat_text)
        .block(Block::default().borders(Borders::ALL).title(" Chat "))
        .wrap(Wrap { trim: true })
        .scroll((scroll_from_top as u16, 0));

    frame.render_widget(chat, area);

    // Render scrollbar if content exceeds view
    if total_lines > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));

        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_from_top);

        frame.render_stateful_widget(
            scrollbar,
            area.inner(ratatui::layout::Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut scrollbar_state,
        );
    }
}
