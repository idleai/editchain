//! Deterministic queries over recorded facts in one caller-selected chain.
//!
//! Operations and availability follow the last successful index refresh.
//! Record references hash original encoded bytes, not their reserialization;
//! they are stable across rebuilds and replay into another chain directory.
//! Page cursors use operation-ID order, never inferred time or causality. They
//! are not subscriptions: use index refresh deltas for late, lower-ID arrivals.
//! No query reads a working tree, observes live Git refs, or interprets taxonomy.
//!
//! ```no_run
//! use editchain_engine::{OpId, queries::{ChainQueries, ContentField, ContentQuery, PageRequest}};
//!
//! # fn inspect(chain: &std::path::Path, revision: OpId) -> std::io::Result<()> {
//! let mut queries = ChainQueries::open(chain)?;
//! let _changes = queries.refresh()?;
//! let _history = queries.history(None, PageRequest::default())?;
//! let _search = queries.search("error", None, PageRequest::default())?;
//! let _content = queries.content(ContentQuery {
//!     operation: revision,
//!     field: ContentField::FileAfter,
//! })?;
//! let _diff = queries.diff(revision)?;
//! let _meta = queries.operation_meta(revision)?;
//! let _ancestors = queries.ancestors(revision, 100)?;
//! # Ok(())
//! # }
//! ```

mod content;
mod diff;
mod fields;
mod git;
mod operation_meta;
mod relationships;
mod search;

use std::{io, path::Path};

use editchain_store::BlobReader;
use serde::{Deserialize, Serialize};

use crate::{Op, OpId};

pub use content::{ContentField, ContentQuery, ContentResult, ContentValue};
pub use diff::{ByteComparison, ContentDiff, RevisionDiff};
pub use editchain_index::{
    ChainIndex, ContentReference, ContentState, ContentStatus, IndexDelta, IndexKey, IndexWork,
};
pub use git::GitQuery;
pub use operation_meta::{AncestorGraph, OperationLookup, OperationMeta};
pub use relationships::{EntityRef, RecordedRelationship, RelationshipKind};
pub use search::{FieldMatch, SearchHit, SearchPage};

/// Portable reference to one exact recorded operation representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRef {
    /// Recorded identity, which can have more than one conflicting representation.
    pub operation: OpId,
    /// Full BLAKE3 digest of the original encoded operation bytes.
    pub record_hash: [u8; 32],
}

/// Original encoded operation bytes and their stable reference, including conflicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedRecord {
    /// Stable reference to these exact bytes.
    pub reference: RecordRef,
    /// Retained encoding, without normalization or reconstruction.
    pub encoded: Vec<u8>,
}

/// One accepted operation with its record reference and external-content availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unmodified recorded envelope and payloads.
    pub operation: Op,
    /// Reference to the original encoding.
    pub record_ref: RecordRef,
    /// Referenced content in schema order, deduplicated by address and length.
    pub content: Vec<ContentStatus>,
}

/// Lookup outcomes keep missing and conflicted identities distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lookup<T> {
    /// No decodable record with this operation identity was recorded.
    Missing,
    /// Multiple representations quarantine the entire identity.
    Conflicted(Vec<RecordRef>),
    /// A result derived from the single accepted representation.
    Found(T),
}

impl<T> Lookup<T> {
    fn try_map<U>(self, map: impl FnOnce(T) -> io::Result<U>) -> io::Result<Lookup<U>> {
        match self {
            Self::Missing => Ok(Lookup::Missing),
            Self::Conflicted(variants) => Ok(Lookup::Conflicted(variants)),
            Self::Found(value) => map(value).map(Lookup::Found),
        }
    }
}

/// Bounded query page, ordered by operation ID with an exclusive cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    /// Resume strictly after this operation identity.
    pub after: Option<OpId>,
    /// Number of candidate operations to inspect, from 1 through 1000.
    pub limit: usize,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            after: None,
            limit: 100,
        }
    }
}

impl PageRequest {
    fn validate(self) -> io::Result<Self> {
        if (1..=1000).contains(&self.limit) {
            Ok(self)
        } else {
            Err(crate::invalid_input(
                "query page limit must be between 1 and 1000",
            ))
        }
    }
}

/// Results and continuation through the inspected operation candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPage<T> {
    /// Results in operation-ID order, then recorded field order.
    pub items: Vec<T>,
    /// Last inspected ID when additional candidates remain. Filtered pages can
    /// be empty and still have a continuation. `None` means the scan is exhausted.
    pub next_after: Option<OpId>,
}

/// Indexed engine queries independent of viewer, host, and controller state.
#[derive(Debug)]
pub struct ChainQueries {
    index: ChainIndex,
    blobs: BlobReader,
}

impl ChainQueries {
    /// Reconcile provider derivations, Codex items, exact copies and source gaps.
    /// This scans accepted records at the last refresh; canonical history is unchanged.
    /// Resolve the returned operation IDs through content queries for payload availability.
    ///
    /// # Errors
    /// Returns index or source-record read errors.
    pub fn import_state(&self) -> io::Result<crate::imports::ImportState> {
        let operations = self
            .all_history(None)?
            .into_iter()
            .map(|entry| entry.operation)
            .collect::<Vec<_>>();
        Ok(crate::imports::ImportState::from_ops(&operations))
    }

    /// Open an existing chain and exclusively own its rebuildable index checkpoint.
    ///
    /// # Errors
    /// Returns storage, index-schema, or checkpoint-lock errors.
    pub fn open(chain_dir: &Path) -> io::Result<Self> {
        Self::from_index(ChainIndex::open(chain_dir)?)
    }

    /// Transfer an already opened index, preserving its last refreshed state.
    ///
    /// # Errors
    /// Returns errors opening the chain's read-only blob adapter.
    pub fn from_index(index: ChainIndex) -> io::Result<Self> {
        let blobs = BlobReader::open(index.chain_dir())?;
        Ok(Self { index, blobs })
    }

    /// Index access for diagnostics and integrity verification.
    #[must_use]
    pub const fn index(&self) -> &ChainIndex {
        &self.index
    }

    /// Observe new operations, conflict retractions, and late content in this chain.
    ///
    /// # Errors
    /// Returns the storage and checkpoint errors from [`ChainIndex::refresh`].
    pub fn refresh(&mut self) -> io::Result<IndexDelta> {
        self.index.refresh()
    }

    /// Rebuild the derived index from persisted records and referenced blobs.
    ///
    /// # Errors
    /// Returns the source and checkpoint errors from [`ChainIndex::rebuild`].
    pub fn rebuild(&mut self) -> io::Result<crate::ChainReadStats> {
        self.index.rebuild()
    }

    /// Read all retained representations, sorted by encoded bytes, including conflicts.
    ///
    /// # Errors
    /// Returns index or source-record read errors.
    pub fn record_variants(&self, id: OpId) -> io::Result<Vec<EncodedRecord>> {
        let mut records = self.index.record_variants(id)?;
        records.sort_by(|a, b| a.encoded.cmp(&b.encoded));
        Ok(records
            .into_iter()
            .map(|record| EncodedRecord {
                reference: RecordRef {
                    operation: id,
                    record_hash: *blake3::hash(&record.encoded).as_bytes(),
                },
                encoded: record.encoded,
            })
            .collect())
    }

    /// Look up an operation without silently choosing a conflicted representation.
    ///
    /// # Errors
    /// Returns index or source-record read errors, including inconsistent index state.
    pub fn operation(&self, id: OpId) -> io::Result<Lookup<HistoryEntry>> {
        let variants = self.record_variants(id)?;
        let Some(operation) = self.index.get(id)? else {
            return Ok(if variants.is_empty() {
                Lookup::Missing
            } else {
                Lookup::Conflicted(
                    variants
                        .into_iter()
                        .map(|record| record.reference)
                        .collect(),
                )
            });
        };
        let [record] = variants.as_slice() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "accepted operation lacks a unique encoded record",
            ));
        };
        let content = self.index.content(id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "accepted operation lacks indexed content",
            )
        })?;
        Ok(Lookup::Found(HistoryEntry {
            operation,
            record_ref: record.reference,
            content,
        }))
    }

    /// Page through accepted history, optionally selecting one explicit indexed fact.
    ///
    /// Conflicts are excluded; inspect index statistics and [`Self::record_variants`] for
    /// quarantine and unsupported/incomplete-record diagnostics. Timestamps, tags,
    /// annotation payloads, and file stages are returned as recorded.
    ///
    /// # Errors
    /// Returns invalid page limits or index/source-record read errors.
    pub fn history(
        &self,
        key: Option<IndexKey>,
        page: PageRequest,
    ) -> io::Result<QueryPage<HistoryEntry>> {
        let page = page.validate()?;
        let mut ids = match key {
            Some(key) => self
                .index
                .lookup(key, page.after, page.limit.saturating_add(1))?,
            None => self
                .index
                .operations(page.after, page.limit.saturating_add(1))?,
        };
        let more = ids.len() > page.limit;
        ids.truncate(page.limit);
        let next_after = more.then(|| ids.last().copied()).flatten();
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            if let Lookup::Found(entry) = self.operation(id)? {
                items.push(entry);
            }
        }
        Ok(QueryPage { items, next_after })
    }

    fn all_history(&self, key: Option<IndexKey>) -> io::Result<Vec<HistoryEntry>> {
        let mut result = Vec::new();
        let mut request = PageRequest {
            after: None,
            limit: 1000,
        };
        loop {
            let page = self.history(key, request)?;
            result.extend(page.items);
            let Some(after) = page.next_after else {
                return Ok(result);
            };
            request.after = Some(after);
        }
    }
}
