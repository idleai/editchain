use ratatui::{
    layout::{Alignment, Constraint, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table},
    Frame,
};
use crate::app::App;
use crate::dag::lanes::LaneCell;
use crate::theme::Theme;

/// Render the DAG log pane (left side of the split).
pub fn render_dag_log(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let snapshot = match &app.snapshot {
        Some(s) => s,
        None => return,
    };

    let visible = &app.visible_rows;
    if visible.is_empty() {
        let empty = Paragraph::new("No operations").block(
            Block::default().borders(Borders::ALL).title("EditChain DAG"),
        );
        frame.render_widget(empty, area);
        return;
    }

    // Use cached DAG rows
    let dag_rows = &app.dag_rows;

    // Determine visible range based on scroll offset
    let viewport_height = (area.height.saturating_sub(2) as usize).max(1);
    let scroll = app.scroll_offset.min(visible.len().saturating_sub(1));
    let end = (scroll + viewport_height).min(visible.len());

    // Column widths
    let time_width = 9usize;
    let dag_width = 8usize;
    let kind_width = 12usize;
    let actor_width = 10usize;
    let tags_width = 12usize;

    // Build header row
    let header_style = Style::default().fg(theme.header_fg).bg(theme.header_bg);
    let header_spans = vec![
        Span::styled("Time     ", header_style),
        Span::styled("DAG      ", header_style),
        Span::styled("Kind       ", header_style),
        Span::styled("Actor     ", header_style),
        Span::styled("Tags       ", header_style),
        Span::styled("Preview", header_style),
    ];
    let header_row = Row::new(vec![Line::from(header_spans)]);

    // Build rows — only for visible range
    let mut rows: Vec<Row> = Vec::with_capacity(end.saturating_sub(scroll));
    for i in scroll..end {
        let ord = visible[i];
        let header = match snapshot.header_at(ord) {
            Some(h) => h,
            None => continue,
        };

        let is_selected = i == app.selected_visible_index;

        // Time column
        let clock_str = format_clock(header.clock_value);

        // DAG lane column — from cache
        let dag_str = if let Some(row) = dag_rows.get(i) {
            render_lane_cells(&row.cells)
        } else {
            " ".to_string()
        };

        // Kind column
        let kind_str = format!("{:<10}", header.kind_name());

        // Actor column
        let actor_str = format!("{:<8}", header.actor);

        // Tags column
        let tags_str = format_tags(header.tags);

        // Preview column (truncated)
        let preview_str = header.preview.as_deref().unwrap_or("");
        let preview_truncated = if preview_str.len() > 60 {
            format!("{}…", &preview_str[..59])
        } else {
            preview_str.to_string()
        };

        let row_style = if is_selected {
            theme.selection_style()
        } else {
            Style::default()
        };

        let spans = vec![
            Span::styled(format!("{:<8} ", clock_str), Style::default().fg(theme.dag_line)),
            Span::styled(format!("{:<8}", dag_str), if is_selected { theme.dag_node_style() } else { Style::default().fg(theme.dag_line) }),
            Span::styled(kind_str, theme.kind_style(header.kind_code)),
            Span::styled(actor_str, Style::default()),
            Span::styled(format!("{:<12}", tags_str), Style::default()),
            Span::styled(preview_truncated, Style::default()),
        ];

        rows.push(Row::new(vec![Line::from(spans)]).style(row_style));
    }

    // Calculate available width for the table
    let col_widths = [
        time_width as u16,
        dag_width as u16,
        kind_width as u16,
        actor_width as u16,
        tags_width as u16,
        area.width.saturating_sub(
            (time_width + dag_width + kind_width + actor_width + tags_width + 2) as u16
        ).max(10),
    ];

    let table = Table::new(
        rows,
        col_widths.map(Constraint::Length).to_vec(),
    )
    .header(header_row)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" EditChain DAG ")
            .title_alignment(Alignment::Left),
    );

    frame.render_widget(table, area);
}

/// Format a clock value as a time string.
fn format_clock(ms: u64) -> String {
    if ms == 0 {
        "---".to_string()
    } else {
        let secs = ms / 1000;
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }
}

/// Render lane cells into a compact string.
fn render_lane_cells(cells: &[LaneCell]) -> String {
    use LaneCell::*;
    let mut s = String::with_capacity(cells.len());
    for cell in cells {
        match cell {
            Empty => s.push(' '),
            Vertical => s.push('│'),
            Node => s.push('●'),
            SelectedNode => s.push('◉'),
            Horizontal => s.push('─'),
            BranchLeft => s.push('╰'),
            BranchRight => s.push('╮'),
            MergeLeft => s.push('╭'),
            MergeRight => s.push('╯'),
            Crossing => s.push('┼'),
            ManyParents => s.push('⋯'),
        }
    }
    s
}

/// Format tag bits into a short string.
fn format_tags(tags: u64) -> String {
    let mut parts = Vec::new();
    if tags & (1 << 0) != 0 { parts.push("agent"); }
    if tags & (1 << 1) != 0 { parts.push("human"); }
    if tags & (1 << 2) != 0 { parts.push("file"); }
    if tags & (1 << 3) != 0 { parts.push("msg"); }
    if tags & (1 << 4) != 0 { parts.push("tool"); }
    if tags & (1 << 5) != 0 { parts.push("cmd"); }
    if tags & (1 << 6) != 0 { parts.push("import"); }
    if tags & (1 << 7) != 0 { parts.push("refl"); }
    if tags & (1 << 8) != 0 { parts.push("note"); }
    if tags & (1 << 9) != 0 { parts.push("err"); }
    if tags & (1 << 10) != 0 { parts.push("priv"); }
    if tags & (1 << 11) != 0 { parts.push("large"); }
    if parts.is_empty() { return "-".to_string(); }
    parts.join(",")
}