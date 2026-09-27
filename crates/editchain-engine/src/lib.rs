//! A reusable entry point for immutable chains, operations, and exact evidence.
//!
//! [`Engine`] takes an explicit chain directory. Producers supply operation,
//! actor, session, and chain identities; opening a directory never derives or
//! replaces those identities. No viewer, runtime, or product workspace is
//! required. The shared schema is re-exported, including [`records`].
//!
//! Use [`Engine::append`] for newly authored operations and
//! [`Engine::append_encoded`] to preserve imported or replicated bytes. Exact
//! replays return [`Admission::Duplicate`]. Different bytes under one operation
//! ID return [`Admission::Conflict`]; all variants remain available as evidence
//! and that ID is excluded from accepted history, including after reopen.
//!
//! Writes use the existing durable segment and blob stores. Each append takes
//! the writer lock and reads current admission state before writing, so handles
//! do not retain stale state after another writer or a failed append. This
//! initial facade favors a simple full read per append; storage adapters and
//! incremental query interfaces can build on the same record contract.

use std::io;
use std::path::{Path, PathBuf};

use editchain_store::format::{encoded_op_len, Page, MAX_RECORD_BYTES};
use editchain_store::{BlobStore, CanonicalChain, SegmentStore};

pub use editchain_core::{
    admission::*, clock::*, git::*, ids::*, op::*, parents::*, payload::*, provider, records,
    scope::*, tags::*,
};
pub use editchain_store::format::{decode_op, encode_op};
pub use editchain_store::{BlobResolution, ChainReadStats};

#[cfg(test)]
use tempfile as _;

/// Filesystem engine handle for one caller-selected chain.
///
/// Handles do not hold a writer lock between calls. Concurrent writers receive
/// `WouldBlock` and can retry the same immutable operation. The engine does not
/// allocate identities, infer chain membership from paths, or rewrite records.
#[derive(Debug, Clone)]
pub struct Engine {
    root: PathBuf,
}

impl Engine {
    /// Open or create a chain directory without generating any history records.
    ///
    /// Use [`ChainSnapshot::read`] for reads that must not create a directory.
    ///
    /// # Errors
    /// Returns filesystem, segment-sequence, or competing-writer errors.
    pub fn open(chain_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let store = SegmentStore::open(chain_dir)?;
        let root = std::fs::canonicalize(store.chain_dir())?;
        Ok(Self { root })
    }

    /// Absolute storage location, independent of logical recorded identities.
    #[must_use]
    pub fn chain_dir(&self) -> &Path {
        &self.root
    }

    /// Read accepted history and all decodable conflict evidence.
    ///
    /// # Errors
    /// Returns the framing and filesystem errors from [`ChainSnapshot::read`].
    pub fn snapshot(&self) -> io::Result<ChainSnapshot> {
        ChainSnapshot::read(&self.root)
    }

    /// Encode and append a newly authored operation using the existing codec.
    ///
    /// Use [`Self::append_encoded`] when original bytes already exist. Success
    /// acknowledges durable evidence, not necessarily an accepted operation:
    /// [`Admission::Conflict`] retains evidence while quarantining the identity.
    ///
    /// # Errors
    /// Returns encoding, size-limit, framing, lock, or persistence errors.
    pub fn append(&self, operation: &Op) -> io::Result<Admission> {
        validate_length(encoded_op_len(operation).map_err(invalid_input)?)?;
        self.append_encoded(&encode_op(operation).map_err(invalid_input)?)
    }

    /// Append exact operation bytes, without normalizing their representation.
    ///
    /// The entire buffer must decode as one supported operation. Duplicates
    /// do not add another physical record. New conflicting variants are synced
    /// before returning and never overwrite earlier evidence. A retry after an
    /// error re-reads durable state; incomplete tails remain in their segment
    /// and subsequent writes begin a fresh segment.
    ///
    /// # Errors
    /// Rejects oversized or undecodable input (including trailing bytes),
    /// corrupt existing framing, competing writers, and failed persistence.
    /// A persistence error can leave retained bytes; retry with the same bytes.
    pub fn append_encoded(&self, encoded: &[u8]) -> io::Result<Admission> {
        validate_length(encoded.len())?;
        let operation = decode_op(encoded).map_err(invalid_input)?;
        let mut store = SegmentStore::open(&self.root)?;
        let chain = CanonicalChain::read(&self.root)?;
        let admission = chain.evidence().classify(operation.id, encoded);
        if admission == Admission::Duplicate {
            // A prior append may have written all bytes but failed at sync.
            // Re-establish durability before acknowledging a retry.
            store.sync_all()?;
        } else {
            let mut page = Page::new(store.segment_sequence());
            page.add_record(0, encoded.to_vec());
            store.append_page(&page)?;
        }
        Ok(admission)
    }

    /// Persist exact bytes and return their full BLAKE3 content reference.
    ///
    /// Records and blobs may arrive in either order. Success is returned only
    /// after the existing blob store's durable publication contract is met.
    ///
    /// # Errors
    /// Returns an error for content exceeding the reference's `u32` length,
    /// inconsistent existing content, or failed persistence.
    pub fn store_blob(&self, bytes: &[u8]) -> io::Result<BlobRef> {
        let len = u32::try_from(bytes.len()).map_err(invalid_input)?;
        let id = ContentId::Hash256(*blake3::hash(bytes).as_bytes());
        let _writer = SegmentStore::open(&self.root)?;
        BlobStore::new(self.root.join("blobs"))?.write(bytes)?;
        Ok(BlobRef { id, len })
    }

    /// Resolve a full content ID, distinguishing missing and invalid content.
    ///
    /// Local and truncated identities are explicitly unresolvable by this
    /// filesystem adapter. Each call observes blobs that arrived since earlier
    /// reads, even if the blob directory did not exist then.
    ///
    /// # Errors
    /// Returns filesystem errors reading the blob directory or content.
    pub fn resolve_content(&self, id: ContentId) -> io::Result<BlobResolution> {
        let ContentId::Hash256(hash) = id else {
            return Ok(BlobResolution::Unresolvable);
        };
        let Some(store) = BlobStore::open_read_only(self.root.join("blobs"))? else {
            return Ok(BlobResolution::Missing);
        };
        let Some(bytes) = store.get(&hash)? else {
            return Ok(BlobResolution::Missing);
        };
        if blake3::hash(&bytes).as_bytes() == &hash {
            Ok(BlobResolution::Found(bytes))
        } else {
            Ok(BlobResolution::Corrupt)
        }
    }

    /// Resolve a blob, verifying both its full content address and length.
    ///
    /// # Errors
    /// Returns the filesystem errors from [`Self::resolve_content`].
    pub fn resolve_blob(&self, reference: &BlobRef) -> io::Result<BlobResolution> {
        match self.resolve_content(reference.id)? {
            BlobResolution::Found(bytes)
                if u32::try_from(bytes.len()).ok() != Some(reference.len) =>
            {
                Ok(BlobResolution::Corrupt)
            }
            resolution @ (BlobResolution::Found(_)
            | BlobResolution::Missing
            | BlobResolution::Corrupt
            | BlobResolution::Unresolvable) => Ok(resolution),
        }
    }

    /// Resolve payload bytes without decoding text or changing the record.
    ///
    /// # Errors
    /// Returns filesystem errors for a referenced blob.
    pub fn resolve_payload(&self, payload: &Payload) -> io::Result<BlobResolution> {
        match payload {
            Payload::Empty => Ok(BlobResolution::Found(Vec::new())),
            Payload::Inline(bytes) => Ok(BlobResolution::Found(bytes.clone())),
            Payload::Blob(reference) => self.resolve_blob(reference),
        }
    }
}

/// An immutable read of accepted operations and retained byte evidence.
///
/// Ordering is deterministic operation-ID order, not inferred chronology.
/// Concurrent appends can appear in a subsequent snapshot. Incomplete tails and
/// unsupported encodings are counted by [`Self::stats`]; their original bytes
/// remain in the segment files. They are not exposed as decoded operations.
#[derive(Debug)]
pub struct ChainSnapshot {
    chain: CanonicalChain,
}

impl ChainSnapshot {
    /// Read a chain without creating files; a missing chain is empty.
    ///
    /// # Errors
    /// Rejects unreadable segments, sequence gaps, corrupt framing, unsupported
    /// page formats, and records beyond the storage size bound.
    pub fn read(chain_dir: &Path) -> io::Result<Self> {
        Ok(Self {
            chain: CanonicalChain::read(chain_dir)?,
        })
    }

    /// Look up one unconflicted operation identity.
    #[must_use]
    pub fn get(&self, id: OpId) -> Option<&Op> {
        self.chain.get(id)
    }

    /// Iterate accepted operations in deterministic operation-ID order.
    pub fn operations(&self) -> impl Iterator<Item = &Op> {
        self.chain.located_ops().map(|(operation, _)| operation)
    }

    /// Every distinct decodable representation, including conflicted variants.
    ///
    /// Use [`OpSet::evidence`] for exact bytes or [`OpSet::conflicts`] to inspect
    /// quarantined identities. These bytes must be used for replay or export;
    /// serializing [`Self::operations`] would discard conflicted evidence.
    #[must_use]
    pub const fn evidence(&self) -> &OpSet {
        self.chain.evidence()
    }

    /// Admission counts and explicit incomplete/undecodable record diagnostics.
    #[must_use]
    pub fn stats(&self) -> ChainReadStats {
        self.chain.stats()
    }
}

fn validate_length(length: usize) -> io::Result<()> {
    if u32::try_from(length).is_ok_and(|length| length <= MAX_RECORD_BYTES) {
        Ok(())
    } else {
        Err(invalid_input("operation exceeds the storage record limit"))
    }
}

fn invalid_input(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error)
}
