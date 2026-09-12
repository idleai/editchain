//! Viewer-owned causal path disclosure. Appends touch only the changed member and
//! graph boundaries; only an explicit user toggle walks a section's members.

use super::{ExpandedRow, LiveBlockMeta, LiveIndex};
use editchain_protocol::TaskStatus;
use std::collections::BTreeSet;

#[derive(Debug, Default, Clone)]
pub(super) struct Group {
    pub(super) collapsed: bool,
    members: BTreeSet<String>,
}

impl LiveIndex {
    pub(super) fn bootstrap_groups(&mut self, blocks: &[LiveBlockMeta]) {
        for meta in blocks {
            self.register_member(meta);
            if meta
                .task_summary
                .as_ref()
                .is_some_and(|task| task.status == TaskStatus::Completed)
            {
                if let Some(group) = &meta.task_group {
                    self.groups.entry(group.clone()).or_default().collapsed = true;
                }
            }
        }
        for meta in blocks {
            self.remeasure(&meta.key);
        }
    }

    pub(super) fn register_member(&mut self, meta: &LiveBlockMeta) {
        if let Some(group) = &meta.task_group {
            let _: bool = self
                .groups
                .entry(group.clone())
                .or_default()
                .members
                .insert(meta.key.clone());
        }
    }

    pub(super) fn forget_member(&mut self, meta: &LiveBlockMeta) {
        if let Some(group) = meta
            .task_group
            .as_ref()
            .and_then(|key| self.groups.get_mut(key))
        {
            let _: bool = group.members.remove(&meta.key);
        }
    }

    pub(super) fn remeasure(&mut self, key: &str) {
        let Some(order) = self.orders.get(key) else {
            return;
        };
        let Some(mut block) = self.tree.remove(order) else {
            return;
        };
        block.hidden = !block.exposed
            && block.meta.task_summary.is_none()
            && self.graph.foldable(key)
            && block
                .meta
                .task_group
                .as_ref()
                .and_then(|key| self.groups.get(key))
                .is_some_and(|group| group.collapsed);
        block.summarized = !block.exposed
            && block.meta.task_summary.is_some()
            && block
                .meta
                .task_group
                .as_ref()
                .and_then(|key| self.groups.get(key))
                .is_some_and(|group| group.collapsed);
        self.insert(block);
    }

    pub(super) fn toggle_group(&mut self, key: &str) -> bool {
        let Some(group) = self.groups.get_mut(key) else {
            return false;
        };
        group.collapsed = !group.collapsed;
        let members = group.members.clone();
        for key in members {
            if let Some(order) = self.orders.get(&key) {
                if let Some(mut block) = self.tree.remove(order) {
                    block.exposed = false;
                    self.insert(block);
                }
            }
            self.remeasure(&key);
        }
        true
    }

    /// Search reveals its exact member without opening the whole task.
    pub(in crate::app) fn reveal(&mut self, row: ExpandedRow) {
        let Some((key, _)) = self.position(row.get()) else {
            return;
        };
        let Some(order) = self.orders.get(&key) else {
            return;
        };
        if let Some(mut block) = self.tree.remove(order) {
            block.exposed = true;
            block.hidden = false;
            block.summarized = false;
            self.insert(block);
        }
    }
}
