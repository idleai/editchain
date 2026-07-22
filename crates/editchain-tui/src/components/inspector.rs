use crate::app::{App, InspectorTab};
use crate::data::header::OpHeader;
use crate::theme::Theme;
use ratatui::{
    layout::{Alignment, Rect},
    style::{Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// Render the inspector pane (right side of the split).
#[expect(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::manual_let_else,
    reason = "TUI inspector; arithmetic bounded by small tab count; manual let-else is clearer for early return"
)]
pub(crate) fn render_inspector(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let snapshot = if let Some(s) = &app.snapshot {
        s
    } else {
        let empty = Paragraph::new("No chain loaded")
            .block(Block::default().borders(Borders::ALL).title(" Inspector "));
        frame.render_widget(empty, area);
        return;
    };

    let selected_op = if let Some(id) = app.selected_op {
        id
    } else {
        let empty = Paragraph::new("No operation selected")
            .block(Block::default().borders(Borders::ALL).title(" Inspector "));
        frame.render_widget(empty, area);
        return;
    };

    let ord = if let Some(o) = snapshot.ordinal_of(&selected_op) {
        o
    } else {
        let empty = Paragraph::new("Operation not found")
            .block(Block::default().borders(Borders::ALL).title(" Inspector "));
        frame.render_widget(empty, area);
        return;
    };

    let header = if let Some(h) = snapshot.header_at(ord) {
        h
    } else {
        let empty = Paragraph::new("Header not available")
            .block(Block::default().borders(Borders::ALL).title(" Inspector "));
        frame.render_widget(empty, area);
        return;
    };

    // Tab bar
    let tab_names = ["Summary", "Content", "Relations", "Raw"];
    let mut tab_spans = Vec::new();
    for (i, name) in tab_names.iter().enumerate() {
        let is_active = match app.inspector_tab {
            InspectorTab::Summary => i == 0,
            InspectorTab::Content => i == 1,
            InspectorTab::Relations => i == 2,
            InspectorTab::Raw => i == 3,
        };
        if is_active {
            tab_spans.push(Span::styled(
                format!(" {name} "),
                Style::default().fg(theme.header_fg).bg(theme.header_bg),
            ));
        } else {
            tab_spans.push(Span::styled(
                format!(" {name} "),
                Style::default().fg(theme.fg),
            ));
        }
        if i < tab_names.len() - 1 {
            tab_spans.push(Span::raw(" │ "));
        }
    }

    let tab_line = Line::from(tab_spans);

    // Content based on active tab
    let content = match app.inspector_tab {
        InspectorTab::Summary => render_summary(header),
        InspectorTab::Content => render_content(header),
        InspectorTab::Relations => render_relations(ord as usize, snapshot),
        InspectorTab::Raw => render_raw(header),
    };

    let mut lines = vec![tab_line];
    lines.extend(content);

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Inspector ")
                .title_alignment(Alignment::Left),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render the Summary tab.
fn render_summary(header: &OpHeader) -> Vec<Line<'_>> {
    vec![
        Line::from(format!(" OpId:      {}", header.id)),
        Line::from(format!(" Node:      {}", header.id.node.0)),
        Line::from(format!(" Boot:      {}", header.id.boot)),
        Line::from(format!(" Sequence:  {}", header.id.seq)),
        Line::from(format!(" Actor:     {}", header.actor)),
        Line::from(format!(
            " Clock:     {} (sub: {})",
            header.clock_value, header.clock_sub
        )),
        Line::from(format!(
            " Scope:     {} ({})",
            header.scope_discriminant, header.scope_value
        )),
        Line::from(format!(" Tags:      {:#018b}", header.tags)),
        Line::from(format!(
            " Kind:      {} ({})",
            header.kind_name(),
            header.kind_code
        )),
        Line::from(format!(" Stage:     {:?}", header.stage_code)),
        Line::from(format!(" Parents:   {}", header.parent_count)),
        Line::from(format!(
            " Preview:   {}",
            header.preview.as_deref().unwrap_or("")
        )),
    ]
}

/// Render the Content tab (kind-specific).
fn render_content(header: &OpHeader) -> Vec<Line<'_>> {
    // For MVP, show a placeholder based on kind
    let kind_info = match header.kind_code {
        0 => "Chain initialization record",
        1 => "Actor registration",
        2 => "Conversation message",
        3 => "Tool call",
        4 => "Shell command",
        5 => "File revision",
        6 => "Agent reflection",
        7 => "Imported external record",
        8 => "Relationship note",
        9 => "Error diagnostic",
        _ => "Unknown operation kind",
    };

    vec![
        Line::from(Span::styled(
            format!(" {} ", header.kind_name()),
            Style::default().bold(),
        )),
        Line::from(String::new()),
        Line::from(format!(" {kind_info}")),
        Line::from(String::new()),
        Line::from(format!(
            " Preview: {}",
            header.preview.as_deref().unwrap_or("")
        )),
        Line::from(String::new()),
        Line::from(Span::styled(
            " Full content decoding coming in Milestone 3.",
            Style::default().dim(),
        )),
    ]
}

/// Render the Relations tab.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "children.len() > 10 check ensures subtraction is safe"
)]
fn render_relations(ord: usize, snapshot: &crate::data::snapshot::TuiSnapshot) -> Vec<Line<'_>> {
    let mut lines = Vec::new();

    // Parents
    if let Some(parents) = snapshot.parents.get(ord) {
        if parents.is_empty() {
            lines.push(Line::from(" Parents:   (root operation)"));
        } else {
            lines.push(Line::from(format!(" Parents:   {}", parents.len())));
            for &p_ord in parents {
                if let Some(h) = snapshot.header_at(p_ord) {
                    lines.push(Line::from(format!("   {}  {}", h.id, h.kind_name())));
                }
            }
        }
    }

    lines.push(Line::from(""));

    // Children
    if let Some(children) = snapshot.children.get(ord) {
        if children.is_empty() {
            lines.push(Line::from(" Children:  (no children)"));
        } else {
            lines.push(Line::from(format!(" Children:  {}", children.len())));
            for &c_ord in children.iter().take(10) {
                if let Some(h) = snapshot.header_at(c_ord) {
                    lines.push(Line::from(format!("   {}  {}", h.id, h.kind_name())));
                }
            }
            if children.len() > 10 {
                lines.push(Line::from(format!(
                    "   ... and {} more",
                    children.len() - 10
                )));
            }
        }
    }

    lines
}

/// Render the Raw JSON tab.
fn render_raw(header: &OpHeader) -> Vec<Line<'_>> {
    // For MVP, show a JSON-like representation of the header fields
    let json = format!(
        r#"{{"id":"{}","actor":{},"clock":{},"tags":{},"kind":"{}","preview":"{}"}}"#,
        header.id,
        header.actor,
        header.clock_value,
        header.tags,
        header.kind_name(),
        header.preview.as_deref().unwrap_or("").escape_default(),
    );

    vec![
        Line::from(Span::styled(" Raw Header:", Style::default().bold())),
        Line::from(""),
        Line::from(json),
        Line::from(""),
        Line::from(Span::styled(
            " Full operation JSON coming in Milestone 3.",
            Style::default().dim(),
        )),
    ]
}
