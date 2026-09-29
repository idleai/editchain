//! Public transaction boundary for the filesystem chain index.

use std::{
    collections::BTreeSet,
    io,
    ops::Bound,
    path::{Path, PathBuf},
    rc::Rc,
};

use editchain_core::{Op, OpId};
use editchain_store::{read_encoded_at, BlobReader, ChainReadStats, OpRecordLocation, TailWork};

use crate::{
    boundary,
    references::references,
    state::{invalid, State, VERSION},
    ContentStatus, IndexKey, Storage,
};

/// IO performed by an incremental refresh, independent of accepted history size.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexWork {
    /// Append-frontier work reported by storage.
    pub records: TailWork,
    /// Distinct reference resolutions, including retries of unresolved content.
    pub content_reads: u64,
}

/// Changes since this handle's previous successful refresh, in operation-ID order.
///
/// This is an observation delta, not a durable subscription cursor. After a
/// crash/reopen consumers must query the current index again. Operation-ID
/// pagination is also not an append cursor: late imports can have older IDs.
#[derive(Debug, Default)]
pub struct IndexDelta {
    /// Newly accepted operations, excluding conflicts in the same refresh.
    pub added: BTreeSet<OpId>,
    /// Identities quarantined by conflicting records, including same-batch conflicts.
    pub removed: BTreeSet<OpId>,
    /// Previously accepted operations whose referenced content changed availability.
    /// Added and removed identities are excluded.
    pub content_changed: BTreeSet<OpId>,
    /// Record and content work performed by this refresh.
    pub work: IndexWork,
}

/// One distinct encoded operation record and its first persisted location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordVariant {
    /// Location of the first occurrence of these exact bytes.
    pub location: OpRecordLocation,
    /// Original encoded bytes; never reconstructed by serializing an operation.
    pub encoded: Vec<u8>,
}

/// Rebuildable filesystem index with an exclusively owned derived checkpoint.
///
/// Records and blobs remain authoritative in `editchain-store`. Queries read
/// the last successful refresh; call [`Self::refresh`] to observe new arrivals.
/// Sealed segments and available blobs are assumed immutable. Integrity checks
/// explicitly reread them to detect external damage. Mutations publish a new
/// checkpoint before replacing the handle's readable state.
#[derive(Debug)]
pub struct ChainIndex {
    pub(crate) root: PathBuf,
    pub(crate) storage: Rc<Storage>,
    pub(crate) state: State,
    blobs: BlobReader,
    needs_reopen: bool,
}

impl ChainIndex {
    /// Open an existing chain, creating its `index-v3` checkpoint if absent.
    ///
    /// A valid checkpoint resumes its append frontier and unresolved content.
    /// Incompatible or damaged checkpoints require [`Self::rebuild_at`]. Opening
    /// never changes chain records or blobs and does not take their writer lock.
    ///
    /// # Errors
    /// Returns source, checkpoint, schema, or competing-index-owner errors.
    pub fn open(chain_dir: &Path) -> io::Result<Self> {
        boundary(|| Self::open_inner(chain_dir, true))?
    }

    /// Replace a missing, stale, incompatible, or corrupt derived checkpoint.
    ///
    /// Reads only persisted records and their referenced blobs. Existing index
    /// handles must be dropped first; the same exclusive checkpoint lock applies.
    /// Unreferenced blobs do not participate in the index.
    ///
    /// # Errors
    /// Returns source replay, content IO, checkpoint publication, or lock errors.
    pub fn rebuild_at(chain_dir: &Path) -> io::Result<Self> {
        boundary(|| Self::open_inner(chain_dir, false))?
    }

    fn open_inner(chain_dir: &Path, resume: bool) -> io::Result<Self> {
        let root = std::fs::canonicalize(chain_dir)?;
        let storage = Storage::open(&root.join("index-v3"))?;
        let blobs = BlobReader::open(&root)?;
        let saved = if resume {
            match storage.load::<State>() {
                Ok(state) => Some(state),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let state = if let Some(state) = saved {
            if state.version != VERSION {
                return Err(invalid("unsupported chain index schema; rebuild required"));
            }
            state.tail.resume(&root)?;
            state
        } else {
            storage.commit(&State::build(&root, &blobs)?)?
        };
        let mut index = Self {
            root,
            storage,
            state,
            blobs,
            needs_reopen: false,
        };
        drop(index.refresh()?);
        Ok(index)
    }

    /// Absolute canonical storage directory underlying this index.
    #[must_use]
    pub fn chain_dir(&self) -> &Path {
        &self.root
    }

    /// Admission diagnostics at the last successful refresh.
    #[must_use]
    pub fn stats(&self) -> ChainReadStats {
        self.state.tail.chain().stats()
    }

    /// Read one accepted operation, retaining the exact recorded fields.
    ///
    /// # Errors
    /// Returns errors for damaged derived pages instead of treating them as absent.
    pub fn get(&self, id: OpId) -> io::Result<Option<Op>> {
        boundary(|| self.state.tail.chain().get(id).cloned())
    }

    /// Resolve an ID prefix against every retained identity, including conflicts.
    /// At most two candidates are read: two means ambiguous, never an arbitrary choice.
    /// # Errors
    /// Returns derived-page read errors.
    pub fn id_candidates(&self, query: &editchain_core::IdQuery) -> io::Result<Vec<OpId>> {
        let (lower, upper) = query.bounds();
        boundary(|| {
            let mut candidates: BTreeSet<_> = self
                .state
                .identities
                .range(lower..=upper)
                .take(2)
                .copied()
                .collect();
            candidates.extend(
                self.state
                    .aliases
                    .range(lower..=upper)
                    .take(2)
                    .map(|(id, _)| *id),
            );
            candidates.into_iter().take(2).collect()
        })
    }

    /// Resolve an old address without hiding multiple conversion targets.
    /// # Errors
    /// Returns derived-index read errors.
    pub fn alias_candidates(&self, id: OpId) -> io::Result<Vec<OpId>> {
        boundary(|| {
            self.state
                .aliases
                .get(&id)
                .map_or_else(Vec::new, |ids| ids.iter().take(2).copied().collect())
        })
    }

    /// Whether an accepted or quarantined physical identity is retained.
    /// # Errors
    /// Returns derived-index read errors.
    pub fn retains_id(&self, id: OpId) -> io::Result<bool> {
        boundary(|| self.state.identities.contains(&id))
    }

    /// Resolve a logical-item prefix independently of operation identities.
    /// # Errors
    /// Returns derived-index read errors.
    pub fn item_candidates(&self, query: &editchain_core::IdQuery) -> io::Result<Vec<OpId>> {
        let (lower, upper) = query.bounds();
        boundary(|| {
            self.state
                .items
                .range(lower..=upper)
                .take(2)
                .copied()
                .collect()
        })
    }

    /// Shortest unique display prefix of at least twelve digits in this chain.
    /// Missing IDs retain their full spelling.
    /// # Errors
    /// Returns derived-page read errors.
    pub fn short_id(&self, id: OpId) -> io::Result<String> {
        let full = id.to_string();
        for length in 12..64 {
            let Some(prefix) = full.get(..length) else {
                break;
            };
            let Some(query) = editchain_core::IdQuery::parse(prefix) else {
                break;
            };
            if self.id_candidates(&query)? == [id] {
                return Ok(prefix.to_owned());
            }
        }
        Ok(full)
    }

    /// Page through accepted IDs in deterministic identity order.
    ///
    /// `after` is exclusive; zero `limit` returns no results. This is a query
    /// cursor, not an append watermark. Use refresh deltas to discover late IDs.
    ///
    /// # Errors
    /// Returns errors reading derived pages.
    pub fn operations(&self, after: Option<OpId>, limit: usize) -> io::Result<Vec<OpId>> {
        boundary(|| {
            self.state
                .records
                .range((
                    after.map_or(Bound::Unbounded, Bound::Excluded),
                    Bound::Unbounded,
                ))
                .take(limit)
                .map(|(id, _)| *id)
                .collect()
        })
    }

    /// Page through IDs matching one recorded fact, in deterministic identity order.
    ///
    /// # Errors
    /// Returns errors reading derived pages.
    pub fn lookup(
        &self,
        key: IndexKey,
        after: Option<OpId>,
        limit: usize,
    ) -> io::Result<Vec<OpId>> {
        boundary(|| {
            self.state.postings.get(&key).map_or_else(Vec::new, |ids| {
                ids.range((
                    after.map_or(Bound::Unbounded, Bound::Excluded),
                    Bound::Unbounded,
                ))
                .take(limit)
                .copied()
                .collect()
            })
        })
    }

    /// Content availability for one accepted operation, in schema field order.
    ///
    /// Repeated identical references appear once. An absent/quarantined operation
    /// returns `None`; an operation without external references returns an empty list.
    ///
    /// # Errors
    /// Returns derived-page errors or inconsistent content-dependency errors.
    pub fn content(&self, id: OpId) -> io::Result<Option<Vec<ContentStatus>>> {
        boundary(|| {
            self.state
                .tail
                .chain()
                .get(id)
                .map(|op| {
                    references(op)
                        .into_iter()
                        .map(|reference| {
                            self.state
                                .content
                                .status(reference)
                                .ok_or_else(|| invalid("missing indexed content dependency"))
                        })
                        .collect()
                })
                .transpose()
        })?
    }

    /// Read distinct encoded records for an operation ID, including conflicting variants.
    ///
    /// # Errors
    /// Returns derived-page or canonical-record IO errors.
    pub fn record_variants(&self, id: OpId) -> io::Result<Vec<RecordVariant>> {
        boundary(|| {
            self.state
                .tail
                .chain()
                .record_locations(id)
                .map(|location| {
                    Ok(RecordVariant {
                        location,
                        encoded: read_encoded_at(&self.root, location)?,
                    })
                })
                .collect()
        })?
    }

    /// Drain newly appended records and retry only unresolved content references.
    ///
    /// No sealed history is decoded on idle polls or blob-only updates. Duplicates
    /// remain inert; conflicts retract all secondary postings for that identity.
    /// Errors leave the last successful state readable and do not consume changes.
    /// After a checkpoint write error, reopen before retrying publication.
    ///
    /// # Errors
    /// Returns framing, continuity, content IO, and checkpoint errors.
    pub fn refresh(&mut self) -> io::Result<IndexDelta> {
        self.check_writable()?;
        boundary(|| {
            let mut next = self.state.clone();
            let records = next.tail.drain()?;
            let mut reads = 0;
            let mut content_changed = next.apply(&self.root, &records, &self.blobs, &mut reads)?;
            content_changed
                .retain(|id| !records.added.contains_key(id) && !records.removed.contains(id));
            if records.work.bytes_read > 0 || !content_changed.is_empty() {
                self.publish(&next)?;
            }
            Ok(IndexDelta {
                added: records.added.into_keys().collect(),
                removed: records.removed,
                content_changed,
                work: IndexWork {
                    records: records.work,
                    content_reads: reads,
                },
            })
        })?
    }

    /// Reconstruct and atomically publish all derived state from storage.
    ///
    /// The existing checkpoint remains published if replay fails. Use a fresh
    /// query after rebuilding; this method returns canonical admission counts.
    ///
    /// # Errors
    /// Returns canonical-source or checkpoint-publication errors.
    pub fn rebuild(&mut self) -> io::Result<ChainReadStats> {
        self.check_writable()?;
        boundary(|| {
            let rebuilt = State::build(&self.root, &self.blobs)?;
            self.publish(&rebuilt)?;
            Ok(self.stats())
        })?
    }

    fn check_writable(&self) -> io::Result<()> {
        if self.needs_reopen {
            Err(io::Error::other(
                "reopen the chain index after a failed checkpoint publication",
            ))
        } else {
            Ok(())
        }
    }

    fn publish(&mut self, state: &State) -> io::Result<()> {
        // Partial page writes or an uncertain root publication require a fresh
        // storage end offset. Keep the old readable state, but fence further writes.
        self.needs_reopen = true;
        self.state = self.storage.commit(state)?;
        self.needs_reopen = false;
        Ok(())
    }
}
