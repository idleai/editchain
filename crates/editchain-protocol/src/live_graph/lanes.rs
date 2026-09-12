//! Stable lane identities and columns; new shared anchors use free columns.

use super::{events::Coverage, is_git, LiveBlockMeta, Order};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Lane {
    Git,
    Spine(usize),
    Operation(usize),
}

#[derive(Debug, Clone, Default)]
pub(super) struct Lanes {
    nodes: HashMap<String, Lane>,
    git: Coverage,
    git_present: bool,
    spines: BTreeMap<usize, Coverage>,
    operations: BTreeMap<usize, Coverage>,
    positions: BTreeMap<Lane, usize>,
    next_position: usize,
}

impl Lanes {
    pub(super) fn bootstrap(
        &mut self,
        plan: &editchain_project::layout::LanePlan,
        nodes: &HashMap<String, LiveBlockMeta>,
    ) {
        self.git_present = nodes.values().any(is_git);
        for index in 0..plan.spine_count {
            let _: &mut Coverage = self.spines.entry(index).or_default();
            let _: Option<usize> = self.positions.insert(Lane::Spine(index), index);
        }
        self.next_position = plan.spine_count;
        let base = usize::from(self.git_present).saturating_add(plan.spine_count);
        // Preserve the established bootstrap plan, independently of map iteration order.
        for index in plan.nodes.values().filter(|index| **index >= base) {
            let position = index.saturating_sub(usize::from(self.git_present));
            let lane = Lane::Operation(index.saturating_sub(base));
            let _: Option<usize> = self.positions.insert(lane, position);
            self.next_position = self.next_position.max(position.saturating_add(1));
        }
        for (key, index) in &plan.nodes {
            let Some(node) = nodes.get(key) else {
                continue;
            };
            let lane = if is_git(node) {
                Lane::Git
            } else {
                Lane::Operation(index.saturating_sub(base))
            };
            self.put(node, lane);
        }
    }
    pub(super) fn node(&self, key: &str) -> Option<Lane> {
        self.nodes.get(key).copied()
    }
    pub(super) fn put(&mut self, node: &LiveBlockMeta, lane: Lane) {
        self.git_present |= is_git(node);
        let _: Option<Lane> = self.nodes.insert(node.key.clone(), lane);
        let _: bool = self.coverage(lane).dots.insert(node.order());
    }
    pub(super) fn remove_dot(&mut self, node: &LiveBlockMeta, forget: bool) {
        if let Some(lane) = self.node(&node.key) {
            let _: bool = self.coverage(lane).dots.remove(&node.order());
            if forget {
                let _: Option<Lane> = self.nodes.remove(&node.key);
            }
        }
    }
    pub(super) fn coverage(&mut self, lane: Lane) -> &mut Coverage {
        if lane != Lane::Git && !self.positions.contains_key(&lane) {
            let _: Option<usize> = self.positions.insert(lane, self.next_position);
            self.next_position = self.next_position.saturating_add(1);
        }
        let (collection, index) = match lane {
            Lane::Git => return &mut self.git,
            Lane::Spine(index) => (&mut self.spines, index),
            Lane::Operation(index) => (&mut self.operations, index),
        };
        collection.entry(index).or_default()
    }
    pub(super) fn allocate(&mut self, spine: bool, start: &Order, end: &Order) -> Lane {
        let collection = if spine {
            &mut self.spines
        } else {
            &mut self.operations
        };
        let index = collection
            .iter()
            .find_map(|(index, lane)| lane.available(start, end).then_some(*index))
            .unwrap_or(collection.len());
        let _: &mut Coverage = collection.entry(index).or_default();
        if spine {
            Lane::Spine(index)
        } else {
            Lane::Operation(index)
        }
    }
    pub(super) fn display(&self, lane: Lane) -> usize {
        match lane {
            Lane::Git => 0,
            Lane::Spine(_) | Lane::Operation(_) => self
                .positions
                .get(&lane)
                .copied()
                .unwrap_or(0)
                .saturating_add(usize::from(self.git_present)),
        }
    }
    pub(super) fn max_lane(&self) -> usize {
        self.iter()
            .filter(|(_, coverage)| !coverage.empty())
            .map(|(lane, _)| self.display(lane))
            .max()
            .unwrap_or(0)
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (Lane, &Coverage)> {
        std::iter::once((Lane::Git, &self.git))
            .chain(
                self.spines
                    .iter()
                    .map(|(index, lane)| (Lane::Spine(*index), lane)),
            )
            .chain(
                self.operations
                    .iter()
                    .map(|(index, lane)| (Lane::Operation(*index), lane)),
            )
    }
}
