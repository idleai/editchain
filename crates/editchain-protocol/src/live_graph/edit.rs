//! Topology edits touch their endpoint paths and affected fork/merge boundaries.

use super::{is_git, Lane, LiveBlockMeta, LiveGraph, Spine};
use std::collections::BTreeSet;
use std::ops::Bound::{Excluded, Unbounded};

impl LiveGraph {
    /// Apply keyed edits without replaying the rest of the lane assignment.
    pub fn edit(&mut self, removed: &[String], upserts: &[LiveBlockMeta]) {
        self.changed.clear();
        self.set_headers(removed, upserts);
        let removed = removed
            .iter()
            .filter(|key| self.nodes.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        let upserts = upserts
            .iter()
            .filter(|meta| meta.task_header.is_none())
            .cloned()
            .collect::<Vec<_>>();
        self.edit_nodes(&removed, &upserts);
    }

    fn edit_nodes(&mut self, removed: &[String], upserts: &[LiveBlockMeta]) {
        let bootstrap = self.nodes.is_empty();
        let changed: BTreeSet<_> = removed
            .iter()
            .cloned()
            .chain(upserts.iter().map(|node| node.key.clone()))
            .collect();
        let mut targets = changed.clone();
        for node in changed
            .iter()
            .filter_map(|key| self.nodes.get(key))
            .chain(upserts)
        {
            targets.extend(node.parents.iter().cloned());
            if let Some(next) = self.order.range((Excluded(node.order()), Unbounded)).next() {
                let _: bool = targets.insert(next.1.clone());
            }
        }
        let mut affected = changed.clone();
        for target in &targets {
            affected.extend(self.incoming.get(target).into_iter().flatten().cloned());
        }
        self.changed
            .extend(affected.iter().chain(&targets).cloned());
        for key in &affected {
            self.remove_paths(key);
        }
        for key in &changed {
            if let Some(old) = self.nodes.remove(key) {
                let _: bool = self.order.remove(&old.order());
                self.lanes.remove_dot(&old, removed.contains(key));
                for parent in old.parents {
                    if let Some(children) = self.incoming.get_mut(&parent) {
                        let _: bool = children.remove(key);
                    }
                }
            }
        }
        for node in upserts {
            for parent in &node.parents {
                let _: bool = self
                    .incoming
                    .entry(parent.clone())
                    .or_default()
                    .insert(node.key.clone());
            }
            let _: bool = self.order.insert(node.order());
            drop(self.nodes.insert(node.key.clone(), node.clone()));
        }
        if bootstrap {
            self.bootstrap();
            return;
        }
        self.update_spines(&targets);
        // Existing sibling paths remain occupied while assigning a new branch.
        for key in &affected {
            self.add_paths(key);
        }
        let mut visiting = BTreeSet::new();
        for node in upserts {
            self.assign(&node.key, &mut visiting);
        }
        for target in &targets {
            affected.extend(self.incoming.get(target).into_iter().flatten().cloned());
        }
        for key in affected {
            self.add_paths(&key);
        }
    }

    fn update_spines(&mut self, targets: &BTreeSet<String>) {
        for key in targets {
            let Some(target) = self.nodes.get(key).filter(|node| is_git(node)) else {
                drop(self.spines.remove(key));
                continue;
            };
            let end = target.order();
            let mut children = self
                .incoming
                .get(key)
                .into_iter()
                .flatten()
                .filter_map(|child| self.nodes.get(child))
                .filter(|node| !is_git(node) && node.order() < end)
                .map(LiveBlockMeta::order);
            let Some(first) = children.next() else {
                drop(self.spines.remove(key));
                continue;
            };
            let Some(second) = children.next() else {
                drop(self.spines.remove(key));
                continue;
            };
            let start = children.fold(first.min(second), std::cmp::min);
            let mut index = 0usize;
            while self.spines.iter().any(|(parent, spine)| {
                parent != key
                    && spine.lane == Lane::Spine(index)
                    && spine.start < end
                    && start < spine.end
            }) {
                index = index.saturating_add(1);
            }
            let lane = Lane::Spine(index);
            let _coverage = self.lanes.coverage(lane);
            drop(self.spines.insert(key.clone(), Spine { lane, start, end }));
        }
    }

    fn assign(&mut self, key: &str, visiting: &mut BTreeSet<String>) {
        // Existing node lanes stay attached to their identities across revisions.
        let Some(node) = self.nodes.get(key).cloned() else {
            return;
        };
        if let Some(lane) = self.lanes.node(key) {
            self.lanes.put(&node, lane);
            return;
        }
        if !visiting.insert(key.to_owned()) {
            return;
        }
        for parent in &node.parents {
            if !visiting.contains(parent) && self.lanes.node(parent).is_none() {
                self.assign(parent, visiting);
            }
        }
        let start = node.order();
        let parent = node.parents.iter().find_map(|key| self.nodes.get(key));
        let end = parent
            .map_or_else(|| start.clone(), LiveBlockMeta::order)
            .max(start.clone());
        let inherited = parent.and_then(|parent| self.lanes.node(&parent.key));
        let lane = if is_git(&node) {
            Lane::Git
        } else if let Some(lane @ Lane::Operation(_)) = inherited {
            if self.lanes.coverage(lane).available(&start, &end) {
                lane
            } else {
                self.lanes.allocate(false, &start, &end)
            }
        } else {
            self.lanes.allocate(false, &start, &end)
        };
        self.lanes.put(&node, lane);
        self.add_paths(key);
        let _: bool = visiting.remove(key);
    }
}
