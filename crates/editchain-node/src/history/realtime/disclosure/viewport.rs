//! Whole task paths stay open while any member is actually visible. Prefetch
//! grants no visibility, and explicit disclosure choices override automatic ones.

use super::{BTreeSet, LiveWork, LiveWorkspace, Result};
use editchain_protocol::{LiveOrder, ViewportLiveRequest};

#[derive(Debug)]
pub(in crate::history::realtime) struct Viewport {
    keys: BTreeSet<String>,
    capacity: usize,
    at_head: bool,
}

impl LiveWorkspace {
    fn task_path(&self, key: &str) -> Option<&String> {
        self.orders
            .get(key)
            .and_then(|order| self.blocks.get(order))
            .and_then(|block| block.meta.task_group.as_ref())
    }

    pub(in crate::history::realtime) fn observe_viewport(
        &mut self,
        request: &ViewportLiveRequest,
    ) -> Result<()> {
        let keys: BTreeSet<_> = request
            .keys
            .iter()
            .filter_map(|key| {
                self.disclosure
                    .position(key)
                    .map(|position| position.0.clone())
            })
            .collect();
        let initial = self
            .viewport
            .as_ref()
            .is_none_or(|view| view.keys.is_empty());
        let visible: BTreeSet<_> = keys
            .iter()
            .filter_map(|key| self.task_path(key).cloned())
            .collect();
        let mut dirty = BTreeSet::new();
        if initial && request.at_head {
            dirty.extend(self.open_automatic(self.latest_paths(&keys)));
        }
        let expired: Vec<_> = self
            .disclosure
            .automatic
            .difference(&visible)
            .cloned()
            .collect();
        for group in expired {
            let _removed = self.disclosure.automatic.remove(&group);
            let _old = self.disclosure.groups.insert(group.clone(), true);
            dirty.extend(self.tasks.member_keys(&group));
        }
        self.viewport = Some(Viewport {
            keys,
            capacity: usize::from(request.capacity),
            at_head: request.at_head,
        });
        if !dirty.is_empty() {
            self.poisoned = true;
            for key in dirty {
                let _changed = self.remeasure(&key);
            }
            self.publish(Vec::new(), Vec::new(), LiveWork::default())?;
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Initially open the newest task's visible paths, without walking history
    /// or implicitly opening older tasks when the user scrolls back to the head.
    fn latest_paths(&self, keys: &BTreeSet<String>) -> BTreeSet<String> {
        let latest = keys
            .iter()
            .filter_map(|key| {
                self.orders
                    .get(key)
                    .zip(self.inputs.get(key)?.task.as_ref())
            })
            .min_by_key(|(order, _)| *order)
            .map(|(_, task)| &task.key);
        keys.iter()
            .filter(|key| {
                self.inputs
                    .get(*key)
                    .and_then(|row| row.task.as_ref())
                    .map(|task| &task.key)
                    == latest
            })
            .filter_map(|key| self.task_path(key).cloned())
            .collect()
    }

    /// Only a fold transition walks a path. Subsequent +1 appends to an open
    /// group remeasure the changed physical boundaries, regardless of its size.
    fn open_automatic(&mut self, groups: BTreeSet<String>) -> BTreeSet<String> {
        let mut dirty = BTreeSet::new();
        for group in groups {
            if self.disclosure.group_expanded(&group)
                || self.disclosure.explicitly_closed.contains_key(&group)
            {
                continue;
            }
            let _old = self.disclosure.groups.insert(group.clone(), false);
            let _inserted = self.disclosure.automatic.insert(group.clone());
            dirty.extend(self.tasks.member_keys(&group));
        }
        dirty
    }

    pub(super) fn open_arrivals(&mut self, changed: &BTreeSet<String>) -> BTreeSet<String> {
        let Some(viewport) = self.viewport.as_ref().filter(|_| !self.preparing) else {
            return BTreeSet::new();
        };
        let mut orders: Vec<_> = viewport
            .keys
            .iter()
            .filter_map(|key| self.orders.get(key))
            .collect();
        orders.sort();
        let in_view = |order: &LiveOrder| match (orders.first(), orders.last()) {
            (Some(first), Some(last)) => (viewport.at_head || *first <= order) && order <= *last,
            _ => viewport.at_head,
        };
        let candidates: BTreeSet<_> = changed
            .iter()
            .filter_map(|key| {
                self.orders
                    .get(key)
                    .filter(|order| in_view(order))
                    .map(|order| (order, key))
            })
            .collect();
        // Bound candidate paths in a multi-task burst. An individual path opens
        // completely; the webview still fetches only its virtualized viewport.
        let groups = candidates
            .iter()
            .take(viewport.capacity)
            .filter_map(|(_, key)| self.task_path(key).cloned())
            .collect();
        self.open_automatic(groups)
    }

    pub(in crate::history::realtime) fn expire_viewport(&mut self) -> Result<()> {
        let mut keys = std::mem::take(&mut self.disclosure.transient);
        for group in std::mem::take(&mut self.disclosure.automatic) {
            let _old = self.disclosure.groups.insert(group.clone(), true);
            keys.extend(self.tasks.member_keys(&group));
        }
        if keys.is_empty() {
            return Ok(());
        }
        for key in keys {
            let _changed = self.remeasure(&key);
        }
        self.checkpoint()
    }
}
