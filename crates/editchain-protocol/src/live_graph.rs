//! Retained Activity graph: the established lane planner and Git-style routes.
//! Geometry is indexed by stable row boundaries, so inserting a row never
//! rewrites every later coordinate. Native paging and WASM use this same state.

mod edit;
mod events;
mod lanes;
mod routes;
#[cfg(test)]
mod tests;

use crate::{HistoryRow, LiveBlockMeta};
use lanes::{Lane, Lanes};
use routes::Path;
use std::collections::{BTreeMap, BTreeSet, HashMap};

type Order = crate::LiveOrder;
type Edge = (String, String);
type Point = (Order, u8);

#[derive(Debug, Clone, Copy)]
enum Change {
    Add,
    Remove,
}

impl Change {
    fn apply(self, count: &mut u64) {
        *count = match self {
            Self::Add => count.saturating_add(1),
            Self::Remove => count.saturating_sub(1),
        };
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Owners {
    active: u64,
    muted: u64,
}

#[derive(Debug, Clone)]
struct Spine {
    lane: Lane,
    start: Order,
    end: Order,
}

impl Owners {
    fn change(&mut self, muted: bool, change: Change) {
        let count = if muted {
            &mut self.muted
        } else {
            &mut self.active
        };
        change.apply(count);
    }
    fn present(self) -> bool {
        self.active > 0 || self.muted > 0
    }
    fn muted(self) -> bool {
        self.active == 0 && self.muted > 0
    }
}

/// Causal lanes and edge events retained across operation deltas.
#[derive(Debug, Clone, Default)]
pub struct LiveGraph {
    nodes: HashMap<String, LiveBlockMeta>,
    order: BTreeSet<Order>,
    incoming: HashMap<String, BTreeSet<String>>,
    lanes: Lanes,
    paths: HashMap<Edge, Path>,
    bends: BTreeMap<Order, BTreeMap<(Lane, Lane), Owners>>,
    spines: HashMap<String, Spine>,
    headers: HashMap<String, LiveBlockMeta>,
    changed: BTreeSet<String>,
}

impl LiveGraph {
    /// Presentation headers participate in paging but never in causal topology.
    pub fn set_headers(&mut self, removed: &[String], upserts: &[LiveBlockMeta]) {
        for key in removed {
            drop(self.headers.remove(key));
        }
        for meta in upserts.iter().filter(|meta| meta.task_header.is_some()) {
            drop(self.headers.insert(meta.key.clone(), meta.clone()));
        }
    }

    /// Endpoints whose disclosure safety may have changed in the last edit.
    pub fn changed_boundaries(&self) -> impl Iterator<Item = &String> {
        self.changed.iter()
    }

    /// Only straight interiors can disappear; preserve every real attachment,
    /// every routing bend (including passing lanes), and protected outcomes.
    #[must_use]
    pub fn foldable(&self, key: &str) -> bool {
        let Some(node) = self.nodes.get(key) else {
            return false;
        };
        let children = self.incoming.get(key);
        !is_git(node)
            && !node.task_protected
            && node.parents.len() == 1
            && children.is_some_and(|children| children.len() == 1)
            && !self.bends.contains_key(&node.order())
            && node
                .parents
                .iter()
                .chain(children.into_iter().flatten())
                .all(|key| self.nodes.get(key).is_some_and(|node| !is_git(node)))
    }
    /// Highest occupied lane, including shared session-to-Git routing spines.
    #[must_use]
    pub fn max_lane(&self) -> usize {
        self.lanes.max_lane()
    }

    /// Decorate a root or detail row with the same graph contract as Activity.
    pub fn decorate(&self, key: &str, slot: u64, row: &mut HistoryRow) {
        let Some(meta) = self.nodes.get(key).or_else(|| self.headers.get(key)) else {
            return;
        };
        let order = meta.order();
        let anchor = meta
            .task_header
            .as_ref()
            .map_or(key, |task| task.anchor.as_str());
        row.lane = self
            .lanes
            .node(anchor)
            .map_or(0, |lane| self.lanes.display(lane));
        row.above.clear();
        row.below.clear();
        row.transitions.clear();
        row.muted_above.clear();
        row.muted_below.clear();
        row.muted_transitions.clear();
        for (lane, coverage) in self.lanes.iter() {
            let lane = self.lanes.display(lane);
            let header = meta.task_header.is_some();
            let above = coverage.at(&(
                order.clone(),
                if header {
                    1
                } else if slot == 0 {
                    0
                } else {
                    2
                },
            ));
            let below = coverage.at(&(order.clone(), if header { 1 } else { 2 }));
            if above.present() {
                row.above.push(lane);
            }
            if below.present() {
                row.below.push(lane);
            }
            if above.muted() {
                row.muted_above.push(lane);
            }
            if below.muted() {
                row.muted_below.push(lane);
            }
        }
        if slot == 0 && meta.task_header.is_none() {
            row.parents = meta
                .parents
                .iter()
                .filter_map(|parent| self.nodes.get(parent).map(|node| node.node_key.clone()))
                .collect();
            for (lanes, owners) in self.bends.get(&order).into_iter().flatten() {
                let lanes = (self.lanes.display(lanes.0), self.lanes.display(lanes.1));
                row.transitions.push(lanes);
                if owners.muted() {
                    row.muted_transitions.push(lanes);
                }
            }
        }
        row.transitions.sort_unstable();
        row.muted_transitions.sort_unstable();
    }

    fn bootstrap(&mut self) {
        self.lanes = Lanes::default();
        let keys: Vec<_> = self.order.iter().map(|order| order.1.clone()).collect();
        let parents: HashMap<_, _> = self
            .nodes
            .iter()
            .map(|(key, node)| (key.clone(), node.parents.clone()))
            .collect();
        let plan = editchain_project::layout::plan_lanes(&keys, &parents, &|key| {
            self.nodes.get(key).is_some_and(is_git)
        });
        self.lanes.bootstrap(&plan, &self.nodes);
        self.spines.clear();
        for ((child, parent), lane) in plan.spines {
            if let (Some(child), Some(target)) = (self.nodes.get(&child), self.nodes.get(&parent)) {
                let start = child.order();
                let _: &mut Spine = self
                    .spines
                    .entry(parent)
                    .and_modify(|spine| {
                        spine.start = spine.start.clone().min(start.clone());
                    })
                    .or_insert(Spine {
                        lane: Lane::Spine(lane.saturating_sub(1)),
                        start,
                        end: target.order(),
                    });
            }
        }
        for key in keys {
            self.add_paths(&key);
        }
    }

    fn add_paths(&mut self, key: &str) {
        let parents = self
            .nodes
            .get(key)
            .map(|node| node.parents.clone())
            .unwrap_or_default();
        for parent in parents {
            if let Some(old) = self.paths.remove(&(key.to_owned(), parent.clone())) {
                self.paint(&old, Change::Remove);
            }
            if let Some(path) = self.route(key, &parent) {
                self.paint(&path, Change::Add);
                drop(self.paths.insert((key.to_owned(), parent), path));
            }
        }
    }

    fn remove_paths(&mut self, key: &str) {
        let parents = self
            .nodes
            .get(key)
            .map(|node| node.parents.clone())
            .unwrap_or_default();
        for parent in parents {
            if let Some(path) = self.paths.remove(&(key.to_owned(), parent)) {
                self.paint(&path, Change::Remove);
            }
        }
    }

    fn paint(&mut self, path: &Path, change: Change) {
        for (lane, start, end) in &path.runs {
            self.lanes
                .coverage(*lane)
                .change(start, end, path.muted, change);
        }
        for (at, from, to) in &path.bends {
            let _: bool = self.changed.insert(at.1.clone());
            let bends = self.bends.entry(at.clone()).or_default();
            let owners = bends.entry((*from, *to)).or_default();
            owners.change(path.muted, change);
            if !owners.present() {
                let _: Option<Owners> = bends.remove(&(*from, *to));
            }
            if bends.is_empty() {
                drop(self.bends.remove(at));
            }
        }
    }
}

fn is_git(node: &LiveBlockMeta) -> bool {
    node.node_key.starts_with("git:")
}
