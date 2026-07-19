use ratatui::{
    layout::{Alignment, Rect},
    style::{Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use crate::theme::Theme;

/// Render the help popup overlay.
pub fn render_help_popup(frame: &mut Frame, area: Rect, _theme: &Theme) {
    // Center the popup
    let popup_width = 50.min(area.width.saturating_sub(4));
    let popup_height = 24.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
    };

    // Clear area behind popup
    frame.render_widget(Clear, popup_area);

    let help_text = vec![
        Line::from(Span::styled(" EditChain TUI Help ", Style::default().bold())),
        Line::from(""),
        Line::from(" Navigation"),
        Line::from("   j / ↓         Move down"),
        Line::from("   k / ↑         Move up"),
        Line::from("   Ctrl-d        Half page down"),
        Line::from("   Ctrl-u        Half page up"),
        Line::from("   PageDown      Page down"),
        Line::from("   PageUp        Page up"),
        Line::from("   g             Go to top"),
        Line::from("   G             Go to bottom"),
        Line::from(""),
        Line::from(" Focus & Tabs"),
        Line::from("   Tab           Toggle DAG log / Inspector focus"),
        Line::from("   h             Jump to parent operation"),
        Line::from("   l             Jump to child operation"),
        Line::from(""),
        Line::from(" Search & Filters"),
        Line::from("   /             Open search"),
        Line::from("   f             Open filters"),
        Line::from("   r             Toggle raw imports"),
        Line::from("   P             Toggle private records"),
        Line::from(""),
        Line::from(" General"),
        Line::from("   y             Copy OpId to clipboard"),
        Line::from("   ?             Toggle this help"),
        Line::from("   Esc           Close popup"),
        Line::from("   q             Quit"),
    ];

    let paragraph = Paragraph::new(Text::from(help_text))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Help ")
                .title_alignment(Alignment::Center)
                .style(Style::default()),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, popup_area);
}