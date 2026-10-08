//! Durable, bounded changes between accepted index revisions.

use std::{
    io,
    ops::Bound,
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use editchain_core::OpId;
use serde::{Deserialize, Serialize};

use crate::{boundary, OrderedMap};

const RETAINED_CHANGES: usize = 100_000;
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Opaque index generation and position. Rebuilding starts a new generation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRevision {
    /// Generation identity; consumers compare it only for equality.
    pub generation: String,
    /// Last recorded change in this generation, independent of operation IDs.
    pub position: u64,
}

/// Effect of one accepted-state change on a dependent index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexChangeKind {
    /// An operation was admitted to the accepted set.
    Added,
    /// Conflicting representations removed an operation from the accepted set.
    Removed,
    /// Referenced content changed availability without changing operation bytes.
    Content,
}

/// One identity requiring reconciliation against the declared index snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexChange {
    /// Complete recorded operation identity.
    pub operation: OpId,
    /// Recorded index change, not a replacement operation.
    pub kind: IndexChangeKind,
}

/// Bounded changes through one fixed accepted-state revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexChanges {
    /// Revision against which the consumer must resolve the returned identities.
    pub revision: IndexRevision,
    /// Resume here after applying this batch.
    pub next: IndexRevision,
    /// Changes in journal order; repeated identities are intentional.
    pub changes: Vec<IndexChange>,
    /// This batch reaches the declared revision.
    pub complete: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ChangeLog {
    pub(crate) revision: IndexRevision,
    entries: OrderedMap<u64, IndexChange>,
}

impl ChangeLog {
    pub(crate) fn initialize(&mut self) -> io::Result<bool> {
        if !self.revision.generation.is_empty() {
            return Ok(false);
        }
        let time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(io::Error::other)?;
        self.revision.generation = format!(
            "{:x}-{:x}-{:x}",
            time.as_nanos(),
            std::process::id(),
            GENERATION.fetch_add(1, Ordering::Relaxed)
        );
        Ok(true)
    }

    pub(crate) fn append(&mut self, operation: OpId, kind: IndexChangeKind) -> io::Result<()> {
        self.revision.position = self
            .revision
            .position
            .checked_add(1)
            .ok_or_else(|| io::Error::other("index revision exhausted"))?;
        let _old = self
            .entries
            .insert(self.revision.position, IndexChange { operation, kind });
        while self.entries.len() > RETAINED_CHANGES {
            let Some(key) = self.entries.first_key_value().map(|(key, _)| *key) else {
                break;
            };
            let _removed = self.entries.remove(&key);
        }
        Ok(())
    }

    pub(crate) fn since(
        &self,
        after: &IndexRevision,
        limit: usize,
    ) -> io::Result<Option<IndexChanges>> {
        if !(1..=10_000).contains(&limit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "change limit must be between 1 and 10000",
            ));
        }
        boundary(|| {
            if after.generation != self.revision.generation
                || after.position > self.revision.position
                || self
                    .entries
                    .first_key_value()
                    .is_some_and(|(first, _)| after.position.saturating_add(1) < *first)
            {
                return None;
            }
            let mut next = after.clone();
            let changes = self
                .entries
                .range((Bound::Excluded(after.position), Bound::Unbounded))
                .take(limit)
                .map(|(position, change)| {
                    next.position = *position;
                    *change
                })
                .collect();
            Some(IndexChanges {
                complete: next == self.revision,
                revision: self.revision.clone(),
                next,
                changes,
            })
        })
    }
}
