//! Sparse rows staged by a response and bounded before the reducer publishes it.

use super::coordinates::ExpandedRow;
use super::row_input::RowInput;
use editchain_protocol::{ErrorCode, HistoryRow, ServiceError};
use std::collections::BTreeMap;

/// Four ordinary request pages. This bounds retained rows, independently of
/// the number of cache keys examined during eviction.
pub(super) const MAX_CACHED_ROWS: usize = 2000;
/// Encoded source DTOs plus once-resolved presentation text. Temporary response
/// decoding/staging and allocator overhead are outside this publication budget.
pub(super) const MAX_CACHED_BYTES: u64 = 16 * 1024 * 1024;

/// Requested visible rows precede spare visible rows and collapsed payloads;
/// distance only breaks ties within one retention class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RetentionPriority {
    Viewport(u64),
    Requested(u64),
    Visible(u64),
    Hidden(u64),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PageCache {
    rows: BTreeMap<ExpandedRow, CacheEntry>,
    bytes: u64,
    /// Budget pressure disables speculative prefetch until this snapshot is
    /// retired. Clearing this after pruning would immediately refetch evictions.
    byte_limited: bool,
}

#[derive(Debug, Clone)]
pub(super) struct CacheEntry {
    row: RowInput,
    bytes: u64,
}

impl CacheEntry {
    fn new(row: RowInput) -> Result<Self, ServiceError> {
        let bytes = row.cache_bytes().map_err(|error| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                format!("Invalid history row: {error}"),
            )
        })?;
        if bytes > MAX_CACHED_BYTES {
            return Err(ServiceError::new(
                ErrorCode::InvalidInput,
                "A history row exceeds the retained content budget.",
            ));
        }
        Ok(Self { row, bytes })
    }
}

impl PageCache {
    /// Resolve and validate the entire response before any row or snapshot
    /// metadata is published. Serialization counts bytes without allocating JSON.
    pub(super) fn prepare(rows: Vec<HistoryRow>) -> Result<Vec<CacheEntry>, ServiceError> {
        rows.into_iter()
            .map(|row| CacheEntry::new(row.into()))
            .collect()
    }

    pub(crate) fn get_by_index(&self, row: i64) -> Option<&RowInput> {
        self.get(ExpandedRow::new(row)?)
    }

    pub(crate) fn get(&self, row: ExpandedRow) -> Option<&RowInput> {
        self.rows.get(&row).map(|entry| &entry.row)
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
    pub(crate) fn retained_bytes(&self) -> u64 {
        self.bytes
    }
    pub(super) fn byte_limited(&self) -> bool {
        self.byte_limited
    }

    pub(super) fn clear(&mut self) {
        self.rows.clear();
        self.bytes = 0;
        self.byte_limited = false;
    }

    /// Stage the arriving page, allowing a pending find target to resolve
    /// before the reducer prunes against the resulting viewport.
    pub(super) fn insert(&mut self, row: ExpandedRow, value: CacheEntry) -> Option<RowInput> {
        self.bytes = self.bytes.saturating_add(value.bytes);
        self.rows.insert(row, value).map(|previous| {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
            previous.row
        })
    }

    #[cfg(test)]
    pub(crate) fn insert_legacy(
        &mut self,
        row: ExpandedRow,
        value: &serde_json::Value,
    ) -> Option<RowInput> {
        self.insert(row, CacheEntry::new(RowInput::from_legacy(value)).unwrap())
    }

    pub(super) fn first_missing(
        &self,
        mut rows: impl Iterator<Item = ExpandedRow>,
    ) -> Option<ExpandedRow> {
        rows.find(|row| !self.rows.contains_key(row))
    }

    /// Remove every out-of-range row, then retain the best candidates up to
    /// both hard caps. Return whether all encountered viewport rows fit.
    /// The owner supplies visibility/distance priorities.
    pub(super) fn retain(
        &mut self,
        top: ExpandedRow,
        bottom: ExpandedRow,
        priority: impl Fn(ExpandedRow) -> RetentionPriority,
    ) -> bool {
        self.rows.retain(|row, entry| {
            let keep = *row >= top && *row <= bottom;
            if !keep {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
            keep
        });
        self.byte_limited |= self.bytes > MAX_CACHED_BYTES;
        if self.rows.len() <= MAX_CACHED_ROWS && self.bytes <= MAX_CACHED_BYTES {
            return true;
        }
        let mut ranked: Vec<_> = self
            .rows
            .keys()
            .copied()
            .map(|row| (priority(row), row))
            .collect();
        ranked.sort_unstable();
        let mut viewport_fits = true;
        for (priority, row) in ranked.into_iter().rev() {
            if self.rows.len() <= MAX_CACHED_ROWS && self.bytes <= MAX_CACHED_BYTES {
                break;
            }
            if let Some(entry) = self.rows.remove(&row) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
                viewport_fits &= !matches!(priority, RetentionPriority::Viewport(_));
            }
        }
        viewport_fits
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
            drop(cache.insert_legacy(abs(row), &serde_json::Value::Null));
        }
        assert!(cache.retain(abs(0), abs(1000), |row| {
            RetentionPriority::Requested(row.get().abs_diff(500))
        }));
        assert_eq!(cache.len(), 1001);
        assert!(!cache.contains_key(abs(9999)));
        for row in 0..10_000 {
            drop(cache.insert_legacy(abs(row), &serde_json::Value::Null));
        }
        assert!(cache.retain(abs(0), abs(9999), |row| {
            RetentionPriority::Requested(row.get().abs_diff(5000))
        }));
        assert_eq!(cache.len(), MAX_CACHED_ROWS);
        assert!(cache.contains_key(abs(5000)));
        assert!(!cache.contains_key(abs(0)));
        assert!(!cache.contains_key(abs(9999)));
    }

    #[test]
    fn heterogeneous_rows_are_charged_on_replacement_eviction_and_clear() {
        let mut cache = PageCache::default();
        for row in 0..80 {
            let bytes = if row % 2 == 0 { 32 * 1024 } else { 512 * 1024 };
            drop(cache.insert_legacy(
                abs(row),
                &serde_json::json!({
                    "group": "x".repeat(bytes), "summary": "short preview"
                }),
            ));
        }
        assert!(cache.retained_bytes() > MAX_CACHED_BYTES);
        assert!(cache.retain(abs(0), abs(79), |row| {
            if row.get() == 0 {
                RetentionPriority::Viewport(0)
            } else {
                RetentionPriority::Requested(row.get().abs_diff(0))
            }
        }));
        assert!(
            cache.len() < 80,
            "bytes constrain even a small number of rows"
        );
        assert!(cache.retained_bytes() <= MAX_CACHED_BYTES);
        assert!(cache.byte_limited());
        let retained = cache.retained_bytes();
        let previous = cache
            .insert_legacy(abs(0), &serde_json::Value::Null)
            .unwrap();
        let replaced = cache.get(abs(0)).unwrap();
        assert_eq!(
            cache.retained_bytes(),
            retained - previous.cache_bytes().unwrap() + replaced.cache_bytes().unwrap()
        );
        assert!(cache.retain(abs(0), abs(0), |_| RetentionPriority::Viewport(0)));
        assert_eq!(
            cache.retained_bytes(),
            cache.get(abs(0)).unwrap().cache_bytes().unwrap()
        );
        assert!(
            cache.byte_limited(),
            "pruning does not restart speculative requests"
        );
        cache.clear();
        assert_eq!(cache.retained_bytes(), 0);
        assert!(cache.is_empty());
        assert!(!cache.byte_limited());
    }
}
