//! Selection identity and the observed roving tab stop are reducer-owned.

use super::coordinates::ExpandedRow;

#[derive(Debug, Clone, Default)]
pub(crate) struct SelectionState {
    key: Option<String>,
    roving: Option<ExpandedRow>,
}

impl SelectionState {
    pub(super) fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
    pub(super) fn select(&mut self, key: &str) {
        self.key = Some(key.to_owned());
    }
    pub(super) fn clear(&mut self) {
        self.key = None;
    }
    pub(super) const fn roving(&self) -> Option<ExpandedRow> {
        self.roving
    }
    pub(super) fn set_roving(&mut self, row: Option<ExpandedRow>) {
        self.roving = row;
    }
}
