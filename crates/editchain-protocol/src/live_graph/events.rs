//! Reference-counted edge boundaries. Prefix sums replace per-row edge walks.

use super::{Change, Order, Owners, Point};
use crate::rank::{Measure, RankTree};
use editchain_index::OrderedSet;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Events {
    weights: RankTree<Point, Measure>,
    keys: OrderedSet<Point>,
}

impl Events {
    fn change(&mut self, key: &Point, start: bool, change: Change) {
        let mut weight = self.weights.get(key).copied().unwrap_or_default();
        let count = if start {
            &mut weight.expanded
        } else {
            &mut weight.visible
        };
        change.apply(count);
        if weight == Measure::default() {
            let _: bool = self.keys.remove(key);
            let _: Option<Measure> = self.weights.remove(key);
        } else {
            let _: bool = self.keys.insert(key.clone());
            let _: Option<Measure> = self.weights.insert(key.clone(), weight, weight);
        }
    }
    fn at(&self, point: &Point) -> u64 {
        let sum = self.weights.prefix(point);
        sum.expanded.saturating_sub(sum.visible)
    }
    fn intersects(&self, start: &Point, end: &Point) -> bool {
        self.at(start) > 0 || self.keys.range(start.clone()..end.clone()).next().is_some()
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Coverage {
    active: Events,
    muted: Events,
    pub(super) dots: OrderedSet<Order>,
}

impl Coverage {
    pub(super) fn change(&mut self, start: &Point, end: &Point, muted: bool, change: Change) {
        let events = if muted {
            &mut self.muted
        } else {
            &mut self.active
        };
        events.change(start, true, change);
        events.change(end, false, change);
    }
    pub(super) fn at(&self, point: &Point) -> Owners {
        Owners {
            active: self.active.at(point),
            muted: self.muted.at(point),
        }
    }
    pub(super) fn empty(&self) -> bool {
        self.dots.is_empty() && self.active.keys.is_empty() && self.muted.keys.is_empty()
    }
    pub(super) fn available(&self, start: &Order, end: &Order) -> bool {
        let start_point = (start.clone(), 1);
        let end_point = (end.clone(), 1);
        !self.at(&(start.clone(), 2)).present()
            && !self.active.intersects(&start_point, &end_point)
            && !self.muted.intersects(&start_point, &end_point)
            && self.dots.range(start.clone()..end.clone()).next().is_none()
    }
}
