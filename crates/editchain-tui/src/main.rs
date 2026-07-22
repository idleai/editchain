//! Editchain TUI — terminal visualization for edit chains.

mod action;
mod app;
mod components;
mod dag;
mod data;
mod event;
mod keymap;
mod theme;

use clap::Parser;
use editchain_node as _;
use serde as _;
use serde_json as _;
use std::path::PathBuf;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};

use crate::app::{App, Popup};
use crate::components::{dag_log, help_popup, inspector, status_bar};
use crate::theme::Theme;

#[derive(Parser)]
#[command(
    name = "editchain-tui",
    about = "EditChain TUI — terminal visualization for edit chains"
)]
struct Cli {
    /// Path to the chain directory
    path: PathBuf,
}

#[expect(
    clippy::let_underscore_must_use,
    reason = "TUI draw result is discarded intentionally"
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Install panic hook that restores terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _: Option<()> = disable_raw_mode().ok();
        let _: Option<()> = execute!(std::io::stdout(), LeaveAlternateScreen).ok();
        original_hook(panic_info);
    }));

    // Initialize app
    let mut app = App::new();
    let theme = Theme::default();

    // Load chain from disk
    match data::loader::load_chain(&cli.path) {
        Ok(snapshot) => {
            app.load_snapshot(snapshot);
        }
        Err(e) => {
            app.status = app::StatusState::Error(format!("Failed to load chain: {e}"));
        }
    }

    // Main event loop — poll_event blocks for 100ms, so no extra sleep needed
    loop {
        // Update terminal dimensions
        if let Ok(size) = terminal.size() {
            app.terminal_height = size.height;
            app.terminal_width = size.width;
        }

        // Draw
        let _: bool = terminal.draw(|frame| render(frame, &app, &theme)).is_ok();

        // Handle events — blocks ~100ms waiting for input
        if let Some(action) = event::poll_event(100) {
            if app.handle_action(action) && app.should_quit {
                break;
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

/// Render the full UI.
#[expect(
    clippy::indexing_slicing,
    reason = "Layout split returns known-length slices"
)]
fn render(frame: &mut Frame<'_>, app: &App, theme: &Theme) {
    let area = frame.area();

    // Main layout: split pane + status bar
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1), // status bar
        ])
        .split(area);

    let content_area = main_layout[0];
    let status_area = main_layout[1];

    // Content layout: DAG log (left) + Inspector (right)
    let content_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(60), // DAG log
            Constraint::Percentage(40), // Inspector
        ])
        .split(content_area);

    // Render DAG log
    dag_log::render_dag_log(frame, content_layout[0], app, theme);

    // Render inspector
    inspector::render_inspector(frame, content_layout[1], app, theme);

    // Render status bar
    status_bar::render_status_bar(frame, status_area, app, theme);

    // Render popup overlays
    if let Some(ref popup) = app.popup {
        match popup {
            Popup::Help => {
                help_popup::render_help_popup(frame, area, theme);
            }
            Popup::Search => {
                // TODO: Search popup (Milestone 4)
                let search_area = centered_rect(60, 20, area);
                let paragraph = Paragraph::new("Search (coming in Milestone 4)")
                    .block(Block::default().borders(Borders::ALL).title(" Search "));
                frame.render_widget(paragraph, search_area);
            }
            Popup::Filters => {
                // TODO: Filters popup (Milestone 4)
                let filter_area = centered_rect(60, 20, area);
                let paragraph = Paragraph::new("Filters (coming in Milestone 4)")
                    .block(Block::default().borders(Borders::ALL).title(" Filters "));
                frame.render_widget(paragraph, filter_area);
            }
        }
    }
}

/// Create a centered rectangle for popups.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}
