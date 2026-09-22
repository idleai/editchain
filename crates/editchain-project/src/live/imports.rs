//! Readiness of current provider occurrences and legacy normalized imports.

use super::{decode_evidence, LiveChanges, LiveProjection};
use editchain_core::{Op, OpId, OpKind};

impl LiveProjection {
    /// Whether a received import has authored content ready for display.
    /// Current provider occurrences require a complete, unambiguous proof.
    /// Legacy numeric children can stand alone even when the raw import is hashed.
    #[must_use]
    pub fn import_ready(&self, source: OpId) -> bool {
        self.selected.contains_key(&source)
            || crate::materialization::complete_derivation(
                source,
                self.import_children(source),
                &self.ops,
            )
            || self.legacy_import_ready(source)
    }

    /// Restore legacy rows hidden by an earlier readiness rule, using only
    /// retained accepted records. No canonical history is imported or changed.
    pub fn refresh_legacy_imports(&self) -> LiveChanges {
        let mut changes = LiveChanges::default();
        for source in self.children.keys() {
            if self.legacy_import_ready(*source) {
                self.publish_record(*source, &mut changes);
            }
        }
        changes
    }

    fn import_children(&self, source: OpId) -> impl Iterator<Item = &Op> {
        self.children
            .get(&source)
            .into_iter()
            .flatten()
            .filter_map(|id| self.ops.get(id).map(AsRef::as_ref))
    }

    fn legacy_import_ready(&self, source: OpId) -> bool {
        let Some(raw) = self.ops.get(&source) else {
            return false;
        };
        let OpKind::Import(import) = &raw.kind else {
            return false;
        };
        if crate::human::work_record(raw).is_some() {
            return false;
        }
        let mut normalized = false;
        for child in self.import_children(source) {
            if matches!(child.kind, OpKind::Import(_)) {
                continue;
            }
            // A typed derivation or a current output must never be mistaken
            // for an old numeric lane while the rest of its proof is arriving.
            if decode_evidence(child).is_some() {
                return false;
            }
            if import.raw_hash.is_some()
                && (!legacy_lane(source, child) || child.scope != raw.scope)
            {
                return false;
            }
            normalized = true;
        }
        normalized
    }
}

fn legacy_lane(source: OpId, child: &Op) -> bool {
    matches!(child.parents, editchain_core::ParentSet::One(parent) if parent == source)
        && child.id.node == source.node
        && child.id.boot == source.boot
        && source.seq > 0
        && source.seq.trailing_zeros() >= 16
        && child.id.seq > source.seq
        && child.id.seq & !0xffff == source.seq
}
