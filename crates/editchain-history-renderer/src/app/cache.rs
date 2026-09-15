//! Sparse rows staged by a response and bounded before the reducer publishes it.

use super::coordinates::ExpandedRow;
use super::row_input::RowInput;
use editchain_protocol::{CachedRow, ErrorCode, HistoryRow, ReconciledRow, ServiceError};
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
    /// Content from retired coordinates can only return through native validation.
    retired: BTreeMap<String, (ExpandedRow, CacheEntry)>,
    bytes: u64,
    /// Budget pressure disables speculative prefetch until this snapshot is
    /// retired. Clearing this after pruning would immediately refetch evictions.
    byte_limited: bool,
}

#[derive(Debug, Clone)]
pub(super) struct CacheEntry {
    row: RowInput,
    bytes: u64,
    version: String,
}

impl CacheEntry {
    pub(in crate::app) fn decorate(
        mut self,
        graph: &editchain_protocol::live_graph::LiveGraph,
        position: &(String, u64),
    ) -> Result<Self, ServiceError> {
        graph.decorate(&position.0, position.1, &mut self.row.source);
        Self::new(self.row)
    }
    pub(in crate::app) fn relocate(
        mut self,
        old: ExpandedRow,
        new: ExpandedRow,
    ) -> Result<Self, ServiceError> {
        self.row.source.parent_row = self.row.source.parent_row.and_then(|parent| {
            let distance = old.get().checked_sub(i64::try_from(parent).ok()?)?;
            usize::try_from(new.get().checked_sub(distance)?).ok()
        });
        Self::new(self.row)
    }
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
        Ok(Self {
            row,
            bytes,
            version: String::new(),
        })
    }
}

impl PageCache {
    pub(super) fn known_rows(&self) -> Vec<CachedRow> {
        self.rows
            .values()
            .chain(self.retired.values().map(|(_, entry)| entry))
            .filter(|entry| !entry.version.is_empty())
            .take(MAX_CACHED_ROWS)
            .map(|entry| CachedRow {
                key: entry.row.continuity_key().to_owned(),
                version: entry.version.clone(),
            })
            .collect()
    }

    pub(super) fn retire_coordinates(&mut self) {
        for (position, entry) in std::mem::take(&mut self.rows) {
            if entry.version.is_empty() {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
                continue;
            }
            if let Some((_, old)) = self
                .retired
                .insert(entry.row.continuity_key().to_owned(), (position, entry))
            {
                self.bytes = self.bytes.saturating_sub(old.bytes);
            }
        }
        self.byte_limited = false;
    }

    fn matching(&self, key: &str, version: &str) -> Option<&CacheEntry> {
        self.retired
            .get(key)
            .map(|(_, entry)| entry)
            .or_else(|| {
                self.rows
                    .values()
                    .find(|entry| entry.row.continuity_key() == key)
            })
            .filter(|entry| entry.version == version)
    }

    /// Validate the whole patch before moving any retained entries into new coordinates.
    pub(super) fn reconcile(
        &mut self,
        rows: Vec<ReconciledRow>,
    ) -> Result<Vec<CacheEntry>, ServiceError> {
        let prepared = rows
            .into_iter()
            .map(|row| {
                let mut entry = row
                    .content
                    .map(|content| CacheEntry::new(content.into()))
                    .transpose()?;
                if let Some(entry) = &mut entry {
                    entry.bytes = entry.bytes.saturating_add(
                        u64::try_from(row.cached.version.len()).unwrap_or(u64::MAX),
                    );
                    entry.version.clone_from(&row.cached.version);
                    if entry.bytes > MAX_CACHED_BYTES {
                        return Err(ServiceError::new(
                            ErrorCode::InvalidInput,
                            "A conditional row exceeds the retained content budget.",
                        ));
                    }
                }
                if entry
                    .as_ref()
                    .is_some_and(|entry| entry.row.continuity_key() != row.cached.key)
                    || (entry.is_none()
                        && self
                            .matching(&row.cached.key, &row.cached.version)
                            .is_none())
                {
                    return Err(ServiceError::new(
                        ErrorCode::InvalidInput,
                        "Conditional row has no matching retained content.",
                    ));
                }
                Ok((row.cached, entry))
            })
            .collect::<Result<Vec<_>, ServiceError>>()?;
        prepared
            .into_iter()
            .map(|(cached, fresh)| {
                let mut entry = if let Some(entry) = fresh {
                    entry
                } else if let Some((_, entry)) = self.retired.remove(&cached.key) {
                    self.bytes = self.bytes.saturating_sub(entry.bytes);
                    entry
                } else {
                    let position = self
                        .rows
                        .iter()
                        .find(|(_, entry)| entry.row.continuity_key() == cached.key)
                        .map(|(position, _)| *position);
                    let entry = position
                        .and_then(|position| self.rows.remove(&position))
                        .ok_or_else(|| {
                            ServiceError::new(ErrorCode::InvalidInput, "Retained row disappeared.")
                        })?;
                    self.bytes = self.bytes.saturating_sub(entry.bytes);
                    entry
                };
                entry.version = cached.version;
                Ok(entry)
            })
            .collect()
    }
    pub(in crate::app) fn indices(&self) -> impl Iterator<Item = ExpandedRow> + '_ {
        self.rows.keys().copied()
    }

    pub(in crate::app) fn take_entries(&mut self) -> BTreeMap<ExpandedRow, CacheEntry> {
        self.bytes = 0;
        std::mem::take(&mut self.rows)
    }
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
        self.retired.clear();
        self.bytes = 0;
        self.byte_limited = false;
    }

    /// Stage the arriving page, allowing a pending find target to resolve
    /// before the reducer prunes against the resulting viewport.
    pub(super) fn insert(&mut self, row: ExpandedRow, value: CacheEntry) -> Option<RowInput> {
        if let Some((_, old)) = self.retired.remove(value.row.continuity_key()) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
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
        self.retired.retain(|_, (row, entry)| {
            let keep = *row >= top && *row <= bottom;
            if !keep {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
            keep
        });
        self.rows.retain(|row, entry| {
            let keep = *row >= top && *row <= bottom;
            if !keep {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
            keep
        });
        while self.rows.len().saturating_add(self.retired.len()) > MAX_CACHED_ROWS
            || self.bytes > MAX_CACHED_BYTES
        {
            let Some((_, (_, entry))) = self.retired.pop_first() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
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

    fn conditional(key: &str, content: bool) -> ReconciledRow {
        ReconciledRow {
            cached: CachedRow {
                key: key.into(),
                version: "a".repeat(64),
            },
            content: content.then(|| {
                RowInput::from_legacy(&serde_json::json!({
                    "node_key": key, "continuity_key": key, "summary": key
                }))
                .source
            }),
        }
    }

    #[test]
    fn invalid_conditional_content_does_not_consume_retained_rows() {
        let mut cache = PageCache::default();
        let entry = cache
            .reconcile(vec![conditional("kept", true)])
            .unwrap()
            .pop()
            .unwrap();
        drop(cache.insert(abs(4), entry));
        let bytes = cache.retained_bytes();
        cache.retire_coordinates();
        assert!(cache
            .reconcile(vec![
                conditional("kept", false),
                conditional("missing", false)
            ])
            .is_err());
        assert!(cache.is_empty(), "retired coordinates stay inaccessible");
        assert_eq!(cache.retained_bytes(), bytes);
        assert_eq!(cache.known_rows().len(), 1);
        let entry = cache
            .reconcile(vec![conditional("kept", false)])
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(cache.retained_bytes(), 0);
        drop(cache.insert(abs(5), entry));
        assert_eq!(cache.get(abs(5)).unwrap().source.summary, "kept");
        assert_eq!(cache.retained_bytes(), bytes);
    }

    #[test]
    fn retired_and_current_content_share_one_budget_and_viewport_wins() {
        let mut cache = PageCache::default();
        for index in 0..MAX_CACHED_ROWS {
            let entry = cache
                .reconcile(vec![conditional(&index.to_string(), true)])
                .unwrap()
                .pop()
                .unwrap();
            drop(cache.insert(abs(i64::try_from(index).unwrap()), entry));
        }
        cache.retire_coordinates();
        let entry = cache
            .reconcile(vec![conditional("visible", true)])
            .unwrap()
            .pop()
            .unwrap();
        drop(cache.insert(abs(0), entry));
        assert!(cache.retain(abs(0), abs(2000), |_| RetentionPriority::Viewport(0)));
        assert_eq!(cache.known_rows().len(), MAX_CACHED_ROWS);
        assert_eq!(
            cache.rows.len().saturating_add(cache.retired.len()),
            MAX_CACHED_ROWS
        );
        assert_eq!(cache.get(abs(0)).unwrap().source.summary, "visible");
        assert!(cache.retained_bytes() <= MAX_CACHED_BYTES);
        cache.clear();
        assert_eq!(cache.retained_bytes(), 0);
        assert!(cache.known_rows().is_empty());
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
