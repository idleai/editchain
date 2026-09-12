//! Mutable block topology. Only a changed block rebuilds its local disclosure.

use super::{invalid, ExpandedRow, ExpansionIndex, VisibleRow, MAX_RENDER_ROWS};
use editchain_protocol::{
    rank::{Axis, Measure, RankTree},
    LiveBaseline, LiveBlock, LiveBlockMeta, LiveDelta, ServiceError, SnapshotId,
};
use std::collections::{BTreeSet, HashMap, HashSet};

mod tasks;
#[cfg(test)]
mod tests;

type Order = editchain_protocol::LiveOrder;

#[derive(Debug, Clone)]
struct Block {
    meta: LiveBlockMeta,
    index: ExpansionIndex,
    exposed: bool,
    hidden: bool,
}

impl Block {
    fn new(meta: &LiveBlockMeta) -> Result<Self, ServiceError> {
        let total = i64::try_from(meta.row_count)
            .map_err(|_overflow| invalid("Invalid live block size."))?;
        if total <= 0 || meta.key.is_empty() {
            return Err(invalid("Empty live block."));
        }
        Ok(Self {
            meta: meta.clone(),
            exposed: false,
            hidden: false,
            index: ExpansionIndex::from_metadata(total, None, Some(&meta.spans))?,
        })
    }

    fn measure(&self) -> Measure {
        Measure {
            expanded: self.meta.row_count,
            visible: if self.hidden {
                0
            } else {
                u64::try_from(self.index.visible_total()).unwrap_or(0)
            },
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::app) struct LiveIndex {
    pub(in crate::app) epoch: SnapshotId,
    pub(in crate::app) revision: u64,
    tree: RankTree<Order, Block>,
    orders: HashMap<String, Order>,
    groups: HashMap<String, tasks::Group>,
    pub(in crate::app) graph: editchain_protocol::live_graph::LiveGraph,
}

impl LiveIndex {
    pub(in crate::app) fn new(baseline: &LiveBaseline) -> Result<Self, ServiceError> {
        let mut index = Self {
            epoch: baseline.epoch.clone(),
            revision: baseline.revision,
            tree: RankTree::default(),
            orders: HashMap::new(),
            groups: HashMap::new(),
            graph: editchain_protocol::live_graph::LiveGraph::default(),
        };
        for meta in &baseline.blocks {
            if index.orders.contains_key(&meta.key) {
                return Err(invalid("Duplicate live block."));
            }
            index.insert(Block::new(meta)?);
        }
        index.graph.edit(&[], &baseline.blocks);
        index.bootstrap_groups(&baseline.blocks);
        if index.total() != baseline.total
            || baseline.total > u64::try_from(MAX_RENDER_ROWS).unwrap_or(0)
        {
            return Err(invalid("Live bootstrap topology does not cover its total."));
        }
        Ok(index)
    }

    pub(in crate::app) fn total(&self) -> u64 {
        self.tree.measure().expanded
    }
    pub(in crate::app) fn visible_total(&self) -> u64 {
        self.tree.measure().visible
    }

    pub(in crate::app) fn position(&self, abs: i64) -> Option<(String, u64)> {
        let abs = u64::try_from(abs).ok()?;
        let (_, block, start) = self.tree.select(abs, Axis::Expanded)?;
        Some((block.meta.key.clone(), abs.saturating_sub(start.expanded)))
    }

    pub(in crate::app) fn absolute(&self, position: &(String, u64)) -> Option<i64> {
        let order = self.orders.get(&position.0)?;
        let block = self.tree.get(order)?;
        (position.1 < block.meta.row_count)
            .then(|| self.tree.rank(order)?.expanded.checked_add(position.1))
            .flatten()
            .and_then(|value| i64::try_from(value).ok())
    }

    pub(in crate::app) fn expanded_for(&self, visible: VisibleRow) -> Option<ExpandedRow> {
        let (_, block, start) = self
            .tree
            .select(u64::try_from(visible.get()).ok()?, Axis::Visible)?;
        let local = VisibleRow::new(
            visible
                .get()
                .saturating_sub(i64::try_from(start.visible).ok()?),
        )?;
        ExpandedRow::new(
            i64::try_from(start.expanded)
                .ok()?
                .saturating_add(block.index.expanded_for(local)?.get()),
        )
    }

    pub(in crate::app) fn visible_for(&self, absolute: ExpandedRow) -> Option<VisibleRow> {
        let (_, block, start) = self
            .tree
            .select(u64::try_from(absolute.get()).ok()?, Axis::Expanded)?;
        if block.hidden {
            return None;
        }
        let local = ExpandedRow::new(
            absolute
                .get()
                .saturating_sub(i64::try_from(start.expanded).ok()?),
        )?;
        VisibleRow::new(
            i64::try_from(start.visible)
                .ok()?
                .saturating_add(block.index.visible_for(local)?.get()),
        )
    }

    pub(in crate::app) fn visible_between(
        &self,
        top: ExpandedRow,
        bottom: ExpandedRow,
    ) -> impl Iterator<Item = ExpandedRow> + '_ {
        let mut cursor = self.first_visible_rank(top);
        std::iter::from_fn(move || {
            let rank = VisibleRow::new(i64::try_from(cursor?).ok()?)?;
            let absolute = self.expanded_for(rank)?;
            cursor = cursor?.checked_add(1);
            (absolute <= bottom).then_some(absolute)
        })
    }

    fn first_visible_rank(&self, top: ExpandedRow) -> Option<u64> {
        let (_, block, start) = self
            .tree
            .select(u64::try_from(top.get()).ok()?, Axis::Expanded)?;
        let local = ExpandedRow::new(
            top.get()
                .saturating_sub(i64::try_from(start.expanded).ok()?),
        )?;
        let last = ExpandedRow::new(block.index.total().saturating_sub(1))?;
        let next = (!block.hidden)
            .then(|| block.index.visible_between(local, last).next())
            .flatten()
            .and_then(|row| block.index.visible_for(row))
            .and_then(|row| u64::try_from(row.get()).ok());
        Some(
            start
                .visible
                .saturating_add(next.unwrap_or_else(|| block.measure().visible)),
        )
    }

    pub(in crate::app) fn is_expanded(&self, row: ExpandedRow) -> bool {
        self.position(row.get())
            .and_then(|(key, relative)| {
                let block = self.tree.get(self.orders.get(&key)?)?;
                if block.meta.task_header.is_some() {
                    return Some(self.groups.get(&key).is_some_and(|group| !group.collapsed));
                }
                Some(
                    block
                        .index
                        .is_expanded(ExpandedRow::new(i64::try_from(relative).ok()?)?),
                )
            })
            .unwrap_or(false)
    }

    pub(in crate::app) fn toggle(&mut self, row: ExpandedRow) -> bool {
        let Some((key, relative)) = self.position(row.get()) else {
            return false;
        };
        if self
            .orders
            .get(&key)
            .and_then(|order| self.tree.get(order))
            .is_some_and(|block| block.meta.task_header.is_some())
        {
            return self.toggle_group(&key);
        }
        let Some(order) = self.orders.get(&key).cloned() else {
            return false;
        };
        let Some(mut block) = self.tree.remove(&order) else {
            return false;
        };
        let toggled = i64::try_from(relative)
            .ok()
            .and_then(ExpandedRow::new)
            .is_some_and(|row| block.index.toggle(row));
        self.insert(block);
        toggled
    }

    pub(in crate::app) fn apply(
        &mut self,
        delta: &LiveDelta,
        expanded: &BTreeSet<String>,
    ) -> Result<(), ServiceError> {
        if delta.base_revision != self.revision || delta.revision != self.revision.saturating_add(1)
        {
            return Err(invalid("Missing live revision; request replay."));
        }
        let mut touched: HashSet<_> = delta.removed.iter().cloned().collect();
        let mut prepared = Vec::new();
        for incoming in &delta.upserts {
            if !touched.insert(incoming.meta.key.clone()) {
                return Err(invalid("Duplicate live edit identity."));
            }
            prepared.push(prepare(incoming, expanded)?);
        }
        let removed_rows = touched
            .iter()
            .filter_map(|key| self.orders.get(key))
            .filter_map(|key| self.tree.get(key))
            .map(|block| block.meta.row_count)
            .fold(0u64, u64::saturating_add);
        let added_rows = prepared
            .iter()
            .map(|block| block.meta.row_count)
            .fold(0u64, u64::saturating_add);
        let total = self
            .total()
            .saturating_sub(removed_rows)
            .saturating_add(added_rows);
        if total != delta.total || total > u64::try_from(MAX_RENDER_ROWS).unwrap_or(0) {
            return Err(invalid("Live delta total mismatch."));
        }
        for key in &touched {
            if let Some(order) = self.orders.remove(key) {
                if let Some(old) = self.tree.remove(&order) {
                    self.forget_member(&old.meta);
                }
            }
        }
        for mut block in prepared {
            // A new or revised record remains visible, even inside a folded task.
            block.exposed = true;
            self.register_member(&block.meta);
            self.insert(block);
        }
        for key in &delta.removed {
            drop(self.groups.remove(key));
        }
        self.graph.edit(
            &delta.removed,
            &delta
                .upserts
                .iter()
                .map(|block| block.meta.clone())
                .collect::<Vec<_>>(),
        );
        touched.extend(self.graph.changed_boundaries().cloned());
        for key in touched {
            self.remeasure(&key);
        }
        self.revision = delta.revision;
        Ok(())
    }

    fn insert(&mut self, block: Block) {
        let order = block.meta.order();
        let measure = block.measure();
        drop(self.orders.insert(block.meta.key.clone(), order.clone()));
        drop(self.tree.insert(order, block, measure));
    }
}

fn prepare(incoming: &LiveBlock, expanded: &BTreeSet<String>) -> Result<Block, ServiceError> {
    if u64::try_from(incoming.rows.len()).unwrap_or(u64::MAX) != incoming.meta.row_count {
        return Err(invalid("Live block content/topology mismatch."));
    }
    let mut block = Block::new(&incoming.meta)?;
    for (slot, row) in incoming.rows.iter().enumerate() {
        if row.parent_row.is_some_and(|parent| parent >= slot) {
            return Err(invalid("Invalid live parent coordinate."));
        }
        if expanded.contains(&row.continuity_key) {
            if let Some(row) = i64::try_from(slot).ok().and_then(ExpandedRow::new) {
                let _toggled = block.index.toggle(row);
            }
        }
    }
    Ok(block)
}
