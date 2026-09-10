//! Sparse rows staged by a response and bounded before the reducer publishes it.

use super::coordinates::ExpandedRow;
use serde_json::Value;
use std::collections::BTreeMap;

/// Four ordinary request pages. This bounds retained rows, independently of
/// the number of cache keys examined during eviction.
pub(super) const MAX_CACHED_ROWS: usize = 2000;

/// Requested visible rows precede spare visible rows and collapsed payloads;
/// distance only breaks ties within one retention class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RetentionPriority {
    Requested(u64),
    Visible(u64),
    Hidden(u64),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PageCache {
    rows: BTreeMap<ExpandedRow, Value>,
}

impl PageCache {
    pub(crate) fn get_by_index(&self, row: i64) -> Option<&Value> {
        self.get(ExpandedRow::new(row)?)
    }

    pub(crate) fn get(&self, row: ExpandedRow) -> Option<&Value> {
        self.rows.get(&row)
    }
    pub(crate) fn contains_key(&self, row: ExpandedRow) -> bool {
        self.rows.contains_key(&row)
    }
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.rows.clear();
    }

    /// Stage the arriving page, allowing a pending find target to resolve
    /// before the reducer prunes against the resulting viewport.
    pub(super) fn insert(&mut self, row: ExpandedRow, value: Value) -> Option<Value> {
        self.rows.insert(row, value)
    }

    pub(super) fn first_missing(
        &self,
        mut rows: impl Iterator<Item = ExpandedRow>,
    ) -> Option<ExpandedRow> {
        rows.find(|row| !self.rows.contains_key(row))
    }

    /// Remove every out-of-range row, then retain the best candidates up to
    /// the hard cap. The owner supplies visibility/distance priorities.
    pub(super) fn retain(
        &mut self,
        top: ExpandedRow,
        bottom: ExpandedRow,
        priority: impl Fn(ExpandedRow) -> RetentionPriority,
    ) {
        self.rows.retain(|row, _| *row >= top && *row <= bottom);
        if self.rows.len() <= MAX_CACHED_ROWS {
            return;
        }
        let mut ranked: Vec<_> = self
            .rows
            .keys()
            .copied()
            .map(|row| (priority(row), row))
            .collect();
        ranked.sort_unstable();
        for (_, row) in ranked.into_iter().skip(MAX_CACHED_ROWS) {
            drop(self.rows.remove(&row));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(row: i64) -> ExpandedRow {
        ExpandedRow::new(row).unwrap()
    }

    #[test]
    fn every_far_row_is_evicted_and_retained_size_is_a_hard_bound() {
        let mut cache = PageCache::default();
        for row in 0..10_000 {
            drop(cache.insert(abs(row), Value::Null));
        }
        cache.retain(abs(0), abs(1000), |row| {
            RetentionPriority::Requested(row.get().abs_diff(500))
        });
        assert_eq!(cache.len(), 1001);
        assert!(!cache.contains_key(abs(9999)));
        for row in 0..10_000 {
            drop(cache.insert(abs(row), Value::Null));
        }
        cache.retain(abs(0), abs(9999), |row| {
            RetentionPriority::Requested(row.get().abs_diff(5000))
        });
        assert_eq!(cache.len(), MAX_CACHED_ROWS);
        assert!(cache.contains_key(abs(5000)));
        assert!(!cache.contains_key(abs(0)));
        assert!(!cache.contains_key(abs(9999)));
    }
}
