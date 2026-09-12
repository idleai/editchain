//! Native rank/select and disclosure for a bounded webview. Task fold
//! transitions visit that path; ordinary appends touch changed boundaries.

use super::{LiveWorkspace, Result};
use editchain_index::Map;
use editchain_protocol::{rank::Measure, ExpansionSpanDto, LiveWork};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

mod viewport;
pub(super) use viewport::Viewport;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct Disclosure {
    views: Map<String, View>,
    groups: Map<String, bool>,
    positions: Map<String, (String, u64)>,
    /// Automatically opened paths close only when all members leave the viewport.
    #[serde(default)]
    automatic: BTreeSet<String>,
    /// User closes override future arrivals, just as explicit opens survive scrolling.
    #[serde(default)]
    explicitly_closed: Map<String, ()>,
    /// Legacy per-row exposure, drained once on reopen without a schema migration.
    #[serde(default)]
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    transient: BTreeSet<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct View {
    keys: Vec<String>,
    spans: Vec<ExpansionSpanDto>,
    open: BTreeSet<String>,
    slots: Vec<u64>,
    exposed: bool,
    hidden: bool,
    #[serde(default)]
    summarized: bool,
}

impl View {
    fn rebuild(&mut self) {
        self.slots.clear();
        let mut hidden_until = 0;
        for (slot, key) in self.keys.iter().enumerate() {
            let slot = u64::try_from(slot).unwrap_or(u64::MAX);
            if slot < hidden_until {
                continue;
            }
            self.slots.push(slot);
            if !self.open.contains(key) {
                if let Some(span) = self.spans.iter().find(|span| span.row == slot) {
                    hidden_until = slot.saturating_add(span.descendant_count).saturating_add(1);
                }
            }
        }
    }
}

impl Disclosure {
    pub(super) fn position(&self, key: &str) -> Option<&(String, u64)> {
        self.positions.get(key)
    }
    pub(super) fn slots(&self, key: &str) -> &[u64] {
        self.views
            .get(key)
            .filter(|view| !view.hidden)
            .map_or(&[], |view| {
                if view.summarized {
                    view.slots.get(..1).unwrap_or_default()
                } else {
                    &view.slots
                }
            })
    }
    pub(super) fn forget_group(&mut self, group: &str) {
        let _old = self.groups.remove(group);
        let _old = self.automatic.remove(group);
        let _old = self.explicitly_closed.remove(group);
    }
    pub(super) fn group_expanded(&self, group: &str) -> bool {
        !self.groups.get(group).copied().unwrap_or(true)
    }
    pub(super) fn settled(&self, key: &str) -> bool {
        self.views.get(key).is_some_and(|view| !view.exposed)
    }
    pub(super) fn expanded(&self, block: &str, slot: u64) -> bool {
        self.views.get(block).is_some_and(|view| {
            view.keys
                .get(usize::try_from(slot).unwrap_or(usize::MAX))
                .is_some_and(|key| view.open.contains(key))
        })
    }
    pub(super) fn remove(&mut self, key: &str) {
        let _removed = self.transient.remove(key);
        if let Some(view) = self.views.remove(key) {
            for identity in view.keys {
                if self
                    .positions
                    .get(&identity)
                    .is_some_and(|position| position.0 == key)
                {
                    drop(self.positions.remove(&identity));
                }
            }
        }
        let _old = self.groups.remove(key);
    }
}

impl LiveWorkspace {
    pub(super) fn reveal_matches(&mut self, keys: &[String]) -> bool {
        let mut changed = false;
        for key in keys {
            if self
                .disclosure
                .views
                .get(key)
                .is_some_and(|view| view.hidden || view.summarized)
            {
                if let Some(view) = self.disclosure.views.get_mut(key) {
                    view.exposed = true;
                }
                let _changed = self.remeasure(key);
                changed = true;
            }
        }
        changed
    }
    pub(super) fn update_disclosure(
        &mut self,
        removed: &[String],
        changed: &[String],
        arrivals: &BTreeSet<String>,
    ) -> Result<()> {
        for key in removed {
            self.disclosure.remove(key);
        }
        let mut dirty: BTreeSet<_> = self.graph.changed_boundaries().cloned().collect();
        for key in changed {
            let Some(block) = self
                .orders
                .get(key)
                .and_then(|order| self.blocks.get(order))
            else {
                continue;
            };
            let rows = self.rows.rows(block)?;
            let mut view = self.disclosure.views.remove(key).unwrap_or_default();
            for identity in &view.keys {
                drop(self.disclosure.positions.remove(identity));
            }
            view.keys = rows.iter().map(|row| row.continuity_key.clone()).collect();
            view.spans.clone_from(&block.meta.spans);
            view.open.retain(|key| view.keys.contains(key));
            view.rebuild();
            for (slot, identity) in view.keys.iter().enumerate() {
                drop(
                    self.disclosure
                        .positions
                        .insert(identity.clone(), (key.clone(), u64::try_from(slot)?)),
                );
            }
            if let Some((_task, group)) = block
                .meta
                .task_summary
                .as_ref()
                .zip(block.meta.task_group.as_ref())
            {
                let _collapsed = self.disclosure.groups.entry(group.clone()).or_insert(true);
            }
            drop(self.disclosure.views.insert(key.clone(), view));
            let _inserted = dirty.insert(key.clone());
        }
        dirty.extend(self.open_arrivals(arrivals));
        for key in dirty {
            let _changed = self.remeasure(&key);
        }
        Ok(())
    }

    pub(super) fn regroup_disclosure(&mut self) {
        self.disclosure.groups.clear();
        self.disclosure.automatic.clear();
        self.disclosure.explicitly_closed.clear();
        self.disclosure.transient.clear();
        let keys: Vec<_> = self.orders.keys().cloned().collect();
        for key in &keys {
            if let Some(view) = self.disclosure.views.get_mut(key) {
                view.exposed = false;
            }
            if let Some(block) = self
                .orders
                .get(key)
                .and_then(|order| self.blocks.get(order))
            {
                if let Some((_task, group)) = block
                    .meta
                    .task_summary
                    .as_ref()
                    .zip(block.meta.task_group.as_ref())
                {
                    let _old = self.disclosure.groups.insert(group.clone(), true);
                }
            }
        }
        for key in keys {
            let _changed = self.remeasure(&key);
        }
    }

    fn remeasure(&mut self, key: &str) -> bool {
        let Some(order) = self.orders.get(key).cloned() else {
            return false;
        };
        let Some(block) = self.blocks.get(&order).cloned() else {
            return false;
        };
        let Some(view) = self.disclosure.views.get_mut(key) else {
            return false;
        };
        let old = (view.hidden, view.summarized);
        let exposed = view.exposed;
        view.hidden = !exposed
            && block.meta.task_summary.is_none()
            && self.graph.foldable(key)
            && block
                .meta
                .task_group
                .as_ref()
                .is_some_and(|key| self.disclosure.groups.get(key).copied().unwrap_or(true));
        view.summarized =
            !exposed
                && block.meta.task_summary.is_some()
                && block.meta.task_group.as_ref().is_some_and(|group| {
                    self.disclosure.groups.get(group).copied().unwrap_or(true)
                });
        let measure = Measure {
            expanded: block.meta.row_count,
            visible: if view.hidden {
                0
            } else if view.summarized {
                1
            } else {
                u64::try_from(view.slots.len()).unwrap_or(u64::MAX)
            },
        };
        let changed = old != (view.hidden, view.summarized);
        drop(self.blocks.insert(order, block, measure));
        changed
    }

    pub(super) fn toggle_disclosure(&mut self, key: &str, task: bool) -> Result<()> {
        let (block, slot) = self
            .disclosure
            .position(key)
            .cloned()
            .ok_or("disclosure row is unavailable")?;
        let group = self
            .orders
            .get(&block)
            .and_then(|order| self.blocks.get(order))
            .and_then(|block| block.meta.task_group.clone())
            .filter(|group| self.disclosure.groups.contains_key(group));
        if task && (slot != 0 || group.is_none()) {
            return Err("row has no task path".into());
        }
        self.poisoned = true;
        if let Some(group) = group.filter(|_| task) {
            let collapsed = self
                .disclosure
                .groups
                .get_mut(&group)
                .ok_or("task path is unavailable")?;
            *collapsed = !*collapsed;
            if *collapsed {
                let _old = self.disclosure.explicitly_closed.insert(group.clone(), ());
            } else {
                let _old = self.disclosure.explicitly_closed.remove(&group);
            }
            let _removed = self.disclosure.automatic.remove(&group);
            for member in self.tasks.member_keys(&group) {
                let _removed = self.disclosure.transient.remove(&member);
                if let Some(view) = self.disclosure.views.get_mut(&member) {
                    view.exposed = false;
                }
                let _changed = self.remeasure(&member);
            }
        } else {
            let view = self
                .disclosure
                .views
                .get_mut(&block)
                .ok_or("disclosure block is unavailable")?;
            if !view.spans.iter().any(|span| span.row == slot) {
                return Err("row has no descendants".into());
            }
            if !view.open.remove(key) {
                let _inserted = view.open.insert(key.to_owned());
            }
            view.rebuild();
            let _changed = self.remeasure(&block);
        }
        self.publish(Vec::new(), Vec::new(), LiveWork::default())?;
        self.checkpoint()
    }
}
