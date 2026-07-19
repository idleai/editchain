use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crate::action::Action;

/// Map keyboard events to actions.
pub fn map_key(event: KeyEvent) -> Action {
    match (event.code, event.modifiers) {
        (KeyCode::Char('q'), _) => Action::Quit,
        (KeyCode::Char('?'), _) => Action::OpenHelp,
        (KeyCode::Esc, _) => Action::ClosePopup,
        (KeyCode::Tab, _) => Action::ToggleFocus,
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Action::Down,
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Action::Up,
        (KeyCode::Char('g'), _) => Action::Top,
        (KeyCode::Char('G'), _) => Action::Bottom,
        (KeyCode::PageDown, _) => Action::PageDown,
        (KeyCode::PageUp, _) => Action::PageUp,
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => Action::HalfPageDown,
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => Action::HalfPageUp,
        (KeyCode::Char('1'), _) => Action::SelectTab1,
        (KeyCode::Char('2'), _) => Action::SelectTab2,
        (KeyCode::Char('3'), _) => Action::SelectTab3,
        (KeyCode::Char('4'), _) => Action::SelectTab4,
        (KeyCode::Enter, _) => Action::EnterInspect,
        (KeyCode::Char('h'), _) => Action::JumpToParent,
        (KeyCode::Char('l'), _) => Action::JumpToChild,
        (KeyCode::Char('y'), _) => Action::CopyOpId,
        (KeyCode::Char('/'), _) => Action::OpenSearch,
        (KeyCode::Char('f'), _) => Action::OpenFilters,
        (KeyCode::Char('r'), _) => Action::ToggleRawImports,
        (KeyCode::Char('P'), _) => Action::TogglePrivate,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Action::Redraw,
        _ => Action::None,
    }
}