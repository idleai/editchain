//! Successive immutable receipts update one live edit without copying its history.

use super::{put, retire, LiveChanges, LiveProjection};
use editchain_core::{Op, OpId};
use editchain_index::{Map, OrderedSet};
use std::collections::BTreeSet;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct HumanEdits {
    sources: Map<OpId, OpId>,
    groups: Map<OpId, OrderedSet<OpId>>,
}

impl HumanEdits {
    pub(super) fn observe(&mut self, source: OpId, op: Option<&Op>) -> BTreeSet<OpId> {
        let mut changed = BTreeSet::new();
        if let Some(group) = self.sources.remove(&source) {
            if let Some(sources) = self.groups.get_mut(&group) {
                let _removed = sources.remove(&source);
            }
            let _new = changed.insert(group);
        }
        if let Some(work) = op.and_then(crate::human::work_record) {
            if let Some(group) = work.edit_group {
                let _old = self.sources.insert(source, group);
                if matches!(
                    work.kind,
                    editchain_core::human::HumanWorkKind::Edit
                        | editchain_core::human::HumanWorkKind::ObservedEdit
                ) {
                    let _new = self.groups.entry(group).or_default().insert(source);
                }
                let _new = changed.insert(group);
            }
        }
        changed
    }

    pub(super) fn group(&self, source: OpId) -> Option<OpId> {
        self.sources.get(&source).copied()
    }
}

impl LiveProjection {
    pub(super) fn publish_human(&self, group: OpId, output: &mut LiveChanges) {
        let key = format!("human-edit:{group}");
        retire(key.clone(), output);
        let Some(sources) = self.human.groups.get(&group) else {
            return;
        };
        let Some((&first, &last)) = sources.first().zip(sources.last()) else {
            return;
        };
        let mut children: Vec<_> = self
            .children
            .get(&last)
            .into_iter()
            .flatten()
            .filter(|id| {
                self.ops
                    .get(id)
                    .is_some_and(|op| !matches!(op.kind, editchain_core::OpKind::Import(_)))
            })
            .copied()
            .collect();
        children.sort_unstable();
        let mut row = self.row(key, last, first, &children);
        // The actual file change is already the primary row. Do not put it
        // inside a second generic Human work disclosure group.
        row.task = None;
        put(row, output);
    }
}
