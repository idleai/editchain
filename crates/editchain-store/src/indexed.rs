//! Canonical admission with exact evidence retained in its durable records.

use crate::{reader::read_encoded_at, ChainReadStats, OpRecordLocation};
use editchain_core::{Admission, Op, OpId};
use editchain_index::Map;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Entry {
    location: OpRecordLocation,
    accepted: Option<Arc<Op>>,
}

/// Resident identities and shared decoded operations, backed by immutable
/// segment records for exact duplicate/conflict evidence. Unlike a standalone
/// evidence set, this index cannot outlive or be merged without its source files.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct IndexedChain {
    root: PathBuf,
    entries: Map<OpId, Entry>,
    conflicts: Map<OpId, Vec<OpRecordLocation>>,
    stats: ChainReadStats,
}

impl IndexedChain {
    /// Classify against exact durable bytes, including every quarantined variant.
    ///
    /// # Errors
    /// Returns IO errors if previously admitted evidence is unavailable.
    pub fn classify(&self, id: OpId, encoded: &[u8]) -> io::Result<Admission> {
        let Some(entry) = self.entries.get(&id) else {
            return Ok(Admission::Accepted);
        };
        for location in
            std::iter::once(&entry.location).chain(self.conflicts.get(&id).into_iter().flatten())
        {
            if usize::try_from(location.data_len).ok() == Some(encoded.len())
                && read_encoded_at(&self.root, *location)? == encoded
            {
                return Ok(Admission::Duplicate);
            }
        }
        Ok(Admission::Conflict)
    }

    /// Final admission and integrity counts for the retained frontier.
    #[must_use]
    pub const fn stats(&self) -> ChainReadStats {
        self.stats
    }

    /// Read one accepted operation without copying its payload.
    #[must_use]
    pub fn get(&self, id: OpId) -> Option<&Op> {
        self.entries.get(&id)?.accepted.as_deref()
    }

    /// Share accepted immutable operations with a dependency index.
    pub fn shared_ops(&self) -> impl Iterator<Item = Arc<Op>> + '_ {
        self.entries
            .values()
            .filter_map(|entry| entry.accepted.clone())
    }
}

impl crate::tail::TailCorpus for IndexedChain {
    fn empty(root: &Path) -> Self {
        Self {
            root: root.into(),
            entries: Map::new(),
            conflicts: Map::new(),
            stats: ChainReadStats::default(),
        }
    }

    fn admit_record(
        &mut self,
        op: Arc<Op>,
        encoded: &[u8],
        location: OpRecordLocation,
    ) -> io::Result<Admission> {
        let admission = self.classify(op.id, encoded)?;
        self.stats.records = self.stats.records.saturating_add(1);
        match admission {
            Admission::Accepted => {
                self.stats.accepted = self.stats.accepted.saturating_add(1);
                drop(self.entries.insert(
                    op.id,
                    Entry {
                        location,
                        accepted: Some(op),
                    },
                ));
            }
            Admission::Duplicate => self.stats.duplicates = self.stats.duplicates.saturating_add(1),
            Admission::Conflict => {
                let entry = self
                    .entries
                    .get_mut(&op.id)
                    .ok_or_else(|| io::Error::other("missing conflicted identity"))?;
                let was_accepted = entry.accepted.take().is_some();
                if was_accepted {
                    self.stats.accepted = self.stats.accepted.saturating_sub(1);
                }
                self.stats.quarantined =
                    self.stats
                        .quarantined
                        .saturating_add(if was_accepted { 2 } else { 1 });
                self.conflicts.entry(op.id).or_default().push(location);
            }
        }
        Ok(admission)
    }

    fn record_undecodable(&mut self) {
        self.stats.undecodable = self.stats.undecodable.saturating_add(1);
    }

    fn record_tail_change(&mut self, incomplete: bool) {
        self.stats.incomplete_tails = if incomplete {
            self.stats.incomplete_tails.saturating_add(1)
        } else {
            self.stats.incomplete_tails.saturating_sub(1)
        };
    }
}
