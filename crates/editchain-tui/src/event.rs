use crate::action::Action;
use crate::keymap;
use crossterm::event::{self, Event};

/// Run one tick of the event loop: poll for keyboard events and map to actions.
pub(crate) fn poll_event(timeout_ms: u64) -> Option<Action> {
    if event::poll(std::time::Duration::from_millis(timeout_ms)).ok()? {
        if let Event::Key(key_event) = event::read().ok()? {
            return Some(keymap::map_key(key_event));
        }
    }
    None
}
