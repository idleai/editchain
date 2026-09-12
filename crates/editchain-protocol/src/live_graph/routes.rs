//! Activity's straight trunks, parent-anchored forks, merge jogs and Git spines.

use super::{is_git, Lane, LiveGraph, Order, Point};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Path {
    pub(super) runs: Vec<(Lane, Point, Point)>,
    pub(super) bends: Vec<(Order, Lane, Lane)>,
    pub(super) muted: bool,
}

impl Path {
    fn run(&mut self, lane: Lane, start: &Order, end: &Order) {
        if start < end {
            self.runs.push((lane, (start.clone(), 1), (end.clone(), 1)));
        }
    }
    fn bend(&mut self, at: &Order, from: Lane, to: Lane) {
        if from != to {
            self.bends.push((at.clone(), from, to));
        }
    }
}

impl LiveGraph {
    pub(super) fn route(&self, key: &str, parent: &str) -> Option<Path> {
        let child = self.nodes.get(key)?;
        let target = self.nodes.get(parent)?;
        let start = child.order();
        let end = target.order();
        // Activity never invents an upward edge for inverted source timestamps.
        if start >= end {
            return None;
        }
        let from = self.lanes.node(key)?;
        let to = self.lanes.node(parent)?;
        let mut path = Path {
            muted: !child.chain_state.is_active(),
            ..Path::default()
        };
        if let Some(spine) = self
            .spines
            .get(parent)
            .map(|spine| spine.lane)
            .filter(|_| !is_git(child))
        {
            path.run(spine, &start, &end);
            path.bend(&start, from, spine);
            path.bend(&end, spine, to);
        } else if from == to {
            path.run(from, &start, &end);
        } else if self
            .incoming
            .get(parent)
            .is_some_and(|children| children.len() > 1)
        {
            path.run(from, &start, &end);
            path.bend(&end, from, to);
        } else {
            let jog = self.order.range(..end.clone()).next_back()?;
            path.run(from, &start, jog);
            path.bend(jog, from, to);
            path.run(to, jog, &end);
        }
        Some(path)
    }
}
