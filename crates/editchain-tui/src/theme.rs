use ratatui::style::{Color, Modifier, Style};

/// Color theme for the `EditChain` TUI.
#[expect(dead_code, reason = "WIP TUI — theme fields used in rendering")]
#[derive(Debug, Clone)]
pub(crate) struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub dag_node: Color,
    pub dag_line: Color,
    pub dag_merge: Color,
    pub header_bg: Color,
    pub header_fg: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub filter_active: Color,
    pub kind_colors: Vec<(u8, Color)>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::Reset,
            selection_bg: Color::Blue,
            selection_fg: Color::White,
            dag_node: Color::Cyan,
            dag_line: Color::DarkGray,
            dag_merge: Color::Yellow,
            header_bg: Color::DarkGray,
            header_fg: Color::White,
            status_bg: Color::DarkGray,
            status_fg: Color::White,
            filter_active: Color::Yellow,
            kind_colors: vec![
                (0, Color::Green),    // ChainStart
                (1, Color::Cyan),     // Actor
                (2, Color::White),    // Message
                (3, Color::Magenta),  // Tool
                (4, Color::Red),      // Command
                (5, Color::Yellow),   // File
                (6, Color::Blue),     // Reflection
                (7, Color::DarkGray), // Import
                (8, Color::Cyan),     // Note
                (9, Color::Red),      // Error
            ],
        }
    }
}

impl Theme {
    /// Style for a kind code.
    pub(crate) fn kind_style(&self, kind_code: u8) -> Style {
        let color = self
            .kind_colors
            .iter()
            .find(|(k, _)| *k == kind_code)
            .map_or(self.fg, |(_, c)| *c);
        Style::default().fg(color)
    }

    /// Style for the selected row.
    pub(crate) fn selection_style(&self) -> Style {
        Style::default()
            .bg(self.selection_bg)
            .fg(self.selection_fg)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for the DAG node marker.
    pub(crate) fn dag_node_style(&self) -> Style {
        Style::default()
            .fg(self.dag_node)
            .add_modifier(Modifier::BOLD)
    }

    // /// Style for DAG connecting lines.
    // pub fn dag_line_style(&self) -> Style {
    //     Style::default().fg(self.dag_line)
    // }
}
