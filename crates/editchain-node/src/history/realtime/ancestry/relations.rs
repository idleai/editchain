//! Structural reference edges coexist with source order. A verified spawn
//! replaces only the child's inherited Git base; completion adds a real merge.

use super::{Ancestry, BTreeSet, HashMap, LiveProjection, OpId};
use editchain_project::live::topology::{RelationChanges, RelationEdge};
use std::collections::BTreeMap;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Structural {
    edges: HashMap<OpId, BTreeMap<(OpId, bool), u64>>,
    references: HashMap<String, BTreeSet<OpId>>,
    users: HashMap<OpId, BTreeSet<String>>,
}

impl Structural {
    pub(super) fn watch(&mut self, key: &str, targets: BTreeSet<OpId>) {
        for old in self.references.remove(key).into_iter().flatten() {
            if let Some(users) = self.users.get_mut(&old) {
                let _: bool = users.remove(key);
            }
        }
        for target in &targets {
            let _: bool = self.users.entry(*target).or_default().insert(key.into());
        }
        if !targets.is_empty() {
            drop(self.references.insert(key.into(), targets));
        }
    }
    pub(super) fn users(&self, id: OpId) -> impl Iterator<Item = &String> {
        self.users.get(&id).into_iter().flatten()
    }
    fn edit(&mut self, edge: RelationEdge, added: bool) {
        let edges = self.edges.entry(edge.anchor).or_default();
        let key = (edge.target, edge.spawn);
        let count = edges.entry(key).or_default();
        *count = if added {
            count.saturating_add(1)
        } else {
            count.saturating_sub(1)
        };
        if *count == 0 {
            let _: Option<u64> = edges.remove(&key);
        }
    }
    pub(super) fn targets(&self, id: OpId) -> Vec<OpId> {
        self.edges
            .get(&id)
            .into_iter()
            .flat_map(|edges| edges.keys().map(|(target, _)| *target))
            .collect()
    }
    pub(super) fn spawn(&self, id: OpId) -> bool {
        self.edges
            .get(&id)
            .is_some_and(|edges| edges.keys().any(|(_, spawn)| *spawn))
    }
}

impl Ancestry {
    pub(in crate::history::realtime) fn observe_relationships(&mut self, changes: RelationChanges) {
        let anchors: BTreeSet<_> = changes
            .removed
            .iter()
            .chain(&changes.added)
            .map(|edge| edge.anchor)
            .collect();
        for edge in changes.removed {
            self.structural.edit(edge, false);
        }
        for edge in changes.added {
            self.structural.edit(edge, true);
        }
        for anchor in &anchors {
            self.pending
                .extend(self.owners.get(anchor).into_iter().flatten().cloned());
        }
        self.invalidate(anchors);
    }

    pub(super) fn reference_owners(
        &self,
        target: OpId,
        projection: &LiveProjection,
    ) -> BTreeSet<String> {
        projection
            .item_owners(target)
            .into_iter()
            .chain(self.owners.get(&target).into_iter().flatten().cloned())
            .filter(|owner| self.owned.contains_key(owner))
            .collect()
    }

    pub(super) fn reference(
        &mut self,
        target: OpId,
        projection: &LiveProjection,
    ) -> BTreeSet<String> {
        let owners = self.reference_owners(target, projection);
        if owners.is_empty() {
            self.resolve(target, projection)
        } else {
            owners
        }
    }
}
