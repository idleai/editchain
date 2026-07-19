use ratatui::{
    layout::Rect,
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use crate::app::{App, StatusState};
use crate::theme::Theme;

/// Render the status bar at the bottom of the screen.
pub fn render_status_bar(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let (status_text, status_style) = match &app.status {
        StatusState::Ready => (" Ready ".to_string(), Style::default().fg(theme.status_fg).bg(theme.status_bg)),
        StatusState::Info(msg) => (format!(" {} ", msg), Style::default().fg(theme.status_fg).bg(theme.status_bg)),
        StatusState::Warning(msg) => (format!(" ⚠ {} ", msg), Style::default().fg(Color::Yellow).bg(theme.status_bg)),
        StatusState::Error(msg) => (format!(" ✗ {} ", msg), Style::default().fg(Color::Red).bg(theme.status_bg)),
    };

    // Keybinding hints
    let hints = " / search  f filters  ? help  Tab focus  j/k scroll  q quit ";

    let spans = vec![
        Span::styled(status_text, status_style),
        Span::raw(" "),
        Span::styled(hints, Style::default().fg(theme.status_fg).bg(theme.status_bg).dim()),
    ];

    let paragraph = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(theme.status_bg));

    frame.render_widget(paragraph, area);
}