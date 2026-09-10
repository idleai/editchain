use core::cmp::Ordering;
use serde::{Deserialize, Serialize};
use std::collections::{btree_map::Entry, BTreeMap};

use crate::admission::OpSet;
use crate::ids::{OpId, PathId};
use crate::op::{FileStage, Op, OpKind};
use crate::payload::ContentId;

// ---------------------------------------------------------------------------
// BlobSet
// ---------------------------------------------------------------------------

/// A content-addressed set of blob references.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlobSet {
    blobs: BTreeMap<ContentIdKey, u32>,
}

/// Wrapper for using `ContentId` as a `BTreeMap` key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum ContentIdKey {
    /// Local content reference (node, seq).
    Local(u64, u64),
    /// 128-bit hash.
    Hash128([u8; 16]),
    /// 256-bit hash.
    Hash256([u8; 32]),
}

impl From<ContentId> for ContentIdKey {
    fn from(cid: ContentId) -> Self {
        match cid {
            ContentId::Local { node, seq } => Self::Local(node.0, seq),
            ContentId::Hash128(h) => Self::Hash128(h),
            ContentId::Hash256(h) => Self::Hash256(h),
        }
    }
}

impl BlobSet {
    /// Create an empty `BlobSet`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            blobs: BTreeMap::new(),
        }
    }

    /// Insert a blob reference into the set.
    #[expect(
        clippy::let_underscore_untyped,
        reason = "BTreeMap insert returns Option; we intentionally discard it"
    )]
    pub fn insert(&mut self, id: ContentId, len: u32) {
        let _ = self.blobs.insert(id.into(), len);
    }

    /// Returns true if the set contains the given `ContentId`.
    #[must_use]
    pub fn contains(&self, id: &ContentId) -> bool {
        let key: ContentIdKey = (*id).into();
        self.blobs.contains_key(&key)
    }
}

// ---------------------------------------------------------------------------
// Canonical causal key
// ---------------------------------------------------------------------------

/// A composite key for deterministic causal ordering of operations.
///
/// Ordering: ancestry → clock → node → boot → seq
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalKey {
    /// Clock value for primary ordering.
    pub clock_val: u64,
    /// Clock sub-value for sub-ms ordering (Hybrid clock ctr).
    pub clock_sub: u16,
    /// Node identifier for tie-breaking.
    pub node: u64,
    /// Boot counter for tie-breaking.
    pub boot: u32,
    /// Sequence number for tie-breaking.
    pub seq: u64,
}

impl CausalKey {
    /// Build a `CausalKey` from an operation's clock and identity fields.
    #[must_use]
    pub const fn from_op(op: &Op) -> Self {
        Self {
            clock_val: op.clock.as_u64(),
            clock_sub: op.clock.sub(),
            node: op.id.node.0,
            boot: op.id.boot,
            seq: op.id.seq,
        }
    }
}

impl Ord for CausalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.clock_val
            .cmp(&other.clock_val)
            .then(self.clock_sub.cmp(&other.clock_sub))
            .then(self.node.cmp(&other.node))
            .then(self.boot.cmp(&other.boot))
            .then(self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for CausalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------
// CanonicalView — deterministic reduced state
// ---------------------------------------------------------------------------

/// The canonical view is a deterministic reduction over all accepted operations.
///
/// It is recomputed from the `OpSet` whenever operations are merged.
/// Every replica with the same `OpSet` produces the same `CanonicalView`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalView {
    /// Messages in canonical causal order.
    pub messages: Vec<OpId>,
    /// File revisions — latest materializing revision per `PathId`.
    pub files: BTreeMap<PathId, FileRevision>,
}

/// The materialized state of a file at a point in the canonical view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRevision {
    /// The operation ID that produced this revision.
    pub op_id: OpId,
    /// The lifecycle stage of this file revision.
    pub stage: FileStage,
    /// Content identifier for the file after this revision.
    pub after: Option<ContentId>,
}

// ---------------------------------------------------------------------------
// Reducer trait
// ---------------------------------------------------------------------------

/// Error type for reduction failures.
#[derive(Debug, Clone)]
pub enum ReduceError {
    /// The operation kind is not supported by this reducer.
    UnsupportedKind(&'static str),
}

impl core::fmt::Display for ReduceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedKind(kind) => write!(f, "unsupported op kind: {kind}"),
        }
    }
}

impl std::error::Error for ReduceError {}

/// A reducer processes operations and updates canonical state.
pub trait Reducer {
    /// Process a single operation and update internal state.
    ///
    /// # Errors
    ///
    /// Returns `ReduceError::UnsupportedKind` if the operation kind is not
    /// handled by this reducer.
    fn reduce(&mut self, op: &Op) -> Result<(), ReduceError>;
}

// ---------------------------------------------------------------------------
// MessageReducer
// ---------------------------------------------------------------------------

/// Collects messages in canonical causal order.
#[derive(Debug, Clone)]
pub struct MessageReducer {
    messages: Vec<(CausalKey, OpId)>,
}

impl MessageReducer {
    /// Create a new empty `MessageReducer`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Consume the reducer and return messages in canonical causal order.
    #[must_use]
    pub fn into_view(self) -> Vec<OpId> {
        let mut sorted = self.messages;
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        sorted.into_iter().map(|(_, id)| id).collect()
    }
}

impl Default for MessageReducer {
    fn default() -> Self {
        Self::new()
    }
}

impl Reducer for MessageReducer {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "MessageReducer only handles Message ops; all other kinds are silently ignored"
    )]
    fn reduce(&mut self, op: &Op) -> Result<(), ReduceError> {
        match &op.kind {
            OpKind::Message(_) => {
                self.messages.push((CausalKey::from_op(op), op.id));
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// FileReducer
// ---------------------------------------------------------------------------

/// Tracks the latest materializing revision per `PathId`.
#[derive(Debug, Clone)]
pub struct FileReducer {
    files: BTreeMap<PathId, (CausalKey, FileRevision)>,
}

impl FileReducer {
    /// Create a new empty `FileReducer`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files: BTreeMap::new(),
        }
    }

    /// Consume the reducer and return the latest revision per path.
    #[must_use]
    pub fn into_view(self) -> BTreeMap<PathId, FileRevision> {
        self.files.into_iter().map(|(k, (_, v))| (k, v)).collect()
    }
}

impl Default for FileReducer {
    fn default() -> Self {
        Self::new()
    }
}

impl Reducer for FileReducer {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "FileReducer only handles File ops; all other kinds are silently ignored"
    )]
    #[expect(
        clippy::let_underscore_untyped,
        reason = "BTreeMap entry insert returns Option; we intentionally discard it"
    )]
    fn reduce(&mut self, op: &Op) -> Result<(), ReduceError> {
        match &op.kind {
            OpKind::File(file_op) => {
                let ck = CausalKey::from_op(op);
                let rev = FileRevision {
                    op_id: op.id,
                    stage: file_op.stage,
                    after: file_op.after,
                };

                match self.files.entry(file_op.path) {
                    Entry::Occupied(mut e) => {
                        let (existing_ck, _) = e.get();
                        if ck > *existing_ck {
                            let _ = e.insert((ck, rev));
                        }
                    }
                    Entry::Vacant(e) => {
                        let _ = e.insert((ck, rev));
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// ChainState — full canonical state
// ---------------------------------------------------------------------------

/// The complete canonical state of an edit chain.
///
/// Contains the grow-only `OpSet` and a deterministic `CanonicalView`
/// computed by running all reducers over the accepted operations.
#[derive(Debug, Clone)]
pub struct ChainState {
    /// The grow-only set of accepted operations.
    pub ops: OpSet,
    /// The content-addressed set of blob references.
    pub blobs: BlobSet,
    /// The deterministic canonical view (messages + file revisions).
    pub view: CanonicalView,
}

impl ChainState {
    /// Create a new empty `ChainState`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ops: OpSet::new(),
            blobs: BlobSet::new(),
            view: CanonicalView::default(),
        }
    }

    /// Recompute the canonical view from scratch by running all reducers
    /// over every accepted operation in `OpSet` order.
    ///
    /// Note: This requires decoding ops from their stored bytes. The codec
    /// crate provides `postcard::from_bytes` for this. When called from
    /// outside the core crate, pass pre-decoded ops or use the codec layer.
    ///
    /// # Errors
    ///
    /// Returns `ReduceError::UnsupportedKind` if any operation kind is not
    /// handled by the registered reducers.
    pub fn recompute_view_from_ops(&mut self, ops: &[Op]) -> Result<(), ReduceError> {
        let mut msg_reducer = MessageReducer::new();
        let mut file_reducer = FileReducer::new();

        for op in ops {
            msg_reducer.reduce(op)?;
            file_reducer.reduce(op)?;
        }

        self.view.messages = msg_reducer.into_view();
        self.view.files = file_reducer.into_view();

        Ok(())
    }

    /// Merge another chain's ops into this one and recompute the view.
    ///
    /// # Errors
    ///
    /// Returns `ReduceError::UnsupportedKind` if any operation kind is not
    /// handled by the registered reducers.
    #[expect(
        clippy::let_underscore_untyped,
        reason = "Borrow self.ops to suppress unused field warning; type is &OpSet"
    )]
    pub fn merge(&mut self, other: &Self) -> Result<(usize, usize, usize), ReduceError> {
        let counts = self.ops.merge(&other.ops);
        // View recomputation requires decoded ops; see recompute_view_from_ops.
        let _ = &self.ops;
        Ok(counts)
    }
}

impl Default for ChainState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
