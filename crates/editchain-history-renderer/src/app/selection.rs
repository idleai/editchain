//! Selection identity and the observed roving tab stop are reducer-owned.

use super::coordinates::ExpandedRow;

#[derive(Debug, Clone, Default)]
pub(crate) struct SelectionState {
    semantic: idle_history::Selection,
    roving: Option<ExpandedRow>,
}

impl SelectionState {
    pub(super) fn key(&self) -> Option<&str> {
        self.semantic.key()
    }
    pub(super) fn select_with_continuity(&mut self, key: &str, continuity: &str) {
        self.semantic.select_with_continuity(key, continuity);
    }
    pub(super) fn continuity_key(&self) -> Option<&str> {
        self.semantic.continuity_key()
    }
    pub(super) fn clear(&mut self) {
        self.semantic.clear();
    }
    pub(super) const fn roving(&self) -> Option<ExpandedRow> {
        self.roving
    }
    pub(super) fn set_roving(&mut self, row: Option<ExpandedRow>) {
        self.roving = row;
    }
}
