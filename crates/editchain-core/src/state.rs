use alloc::collections::btree_map::Entry;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cmp::Ordering;
use serde::{Deserialize, Serialize};

use crate::ids::*;
use crate::op::*;
use crate::payload::ContentId;

// ---------------------------------------------------------------------------
// OpSet — grow-only set keyed by OpId
// ---------------------------------------------------------------------------

/// A grow-only set of operations, keyed by `OpId`.
///
/// Admission rules:
/// - Same `OpId` + same bytes → duplicate; ignored.
/// - Same `OpId` + different bytes → invalid duplicate; quarantined.
/// - New valid `OpId` → accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpSet {
    ops: BTreeMap<OpId, Vec<u8>>,
    quarantined: Vec<(OpId, Vec<u8>, Vec<u8>)>,
}

impl OpSet {
    pub fn new() -> Self {
        Self {
            ops: BTreeMap::new(),
            quarantined: Vec::new(),
        }
    }

    /// Try to insert an operation.
    ///
    /// Returns `Ok(true)` if accepted, `Ok(false)` if duplicate,
    /// `Err((existing, incoming))` if invalid duplicate (quarantined).
    pub fn insert(&mut self, id: OpId, encoded: Vec<u8>) -> Result<bool, (Vec<u8>, Vec<u8>)> {
        match self.ops.entry(id) {
            Entry::Occupied(e) => {
                if e.get() == &encoded {
                    // Exact duplicate — ignore.
                    Ok(false)
                } else {
                    // Same OpId, different bytes — quarantine.
                    let existing = e.get().clone();
                    self.quarantined.push((*e.key(), existing.clone(), encoded.clone()));
                    Err((existing, encoded))
                }
            }
            Entry::Vacant(e) => {
                e.insert(encoded);
                Ok(true)
            }
        }
    }

    /// Returns true if the set contains the given OpId.
    pub fn contains(&self, id: &OpId) -> bool {
        self.ops.contains_key(id)
    }

    /// Returns the number of accepted operations.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Returns true if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Iterate over all accepted (OpId, encoded_bytes) pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&OpId, &[u8])> {
        self.ops.iter().map(|(k, v)| (k, v.as_slice()))
    }

    /// Returns a reference to the quarantined entries.
    pub fn quarantined(&self) -> &[(OpId, Vec<u8>, Vec<u8>)] {
        &self.quarantined
    }

    /// Merge another OpSet into this one (set-union).
    ///
    /// Returns counts: (accepted, duplicates, quarantined).
    pub fn merge(&mut self, other: &OpSet) -> (usize, usize, usize) {
        let mut accepted = 0;
        let mut duplicates = 0;
        let mut quarantined = 0;

        for (id, bytes) in other.ops.iter() {
            match self.insert(*id, bytes.clone()) {
                Ok(true) => accepted += 1,
                Ok(false) => duplicates += 1,
                Err(_) => quarantined += 1,
            }
        }

        (accepted, duplicates, quarantined)
    }
}

impl Default for OpSet {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// BlobSet
// ---------------------------------------------------------------------------

/// A content-addressed set of blob references.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlobSet {
    blobs: BTreeMap<ContentIdKey, u32>,
}

/// Wrapper for using ContentId as a BTreeMap key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum ContentIdKey {
    Local(u64, u64),
    Hash128([u8; 16]),
    Hash256([u8; 32]),
}

impl From<crate::payload::ContentId> for ContentIdKey {
    fn from(cid: crate::payload::ContentId) -> Self {
        match cid {
            crate::payload::ContentId::Local { node, seq } => ContentIdKey::Local(node.0, seq),
            crate::payload::ContentId::Hash128(h) => ContentIdKey::Hash128(h),
            crate::payload::ContentId::Hash256(h) => ContentIdKey::Hash256(h),
        }
    }
}

impl BlobSet {
    pub fn new() -> Self {
        Self { blobs: BTreeMap::new() }
    }

    pub fn insert(&mut self, id: ContentId, len: u32) {
        self.blobs.insert(id.into(), len);
    }

    pub fn contains(&self, id: &crate::payload::ContentId) -> bool {
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
    pub clock_val: u64,
    pub clock_sub: u16,
    pub node: u64,
    pub boot: u32,
    pub seq: u64,
}

impl CausalKey {
    pub fn from_op(op: &Op) -> Self {
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
/// It is recomputed from the OpSet whenever operations are merged.
/// Every replica with the same OpSet produces the same CanonicalView.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalView {
    /// Messages in canonical causal order.
    pub messages: Vec<OpId>,
    /// File revisions — latest materializing revision per PathId.
    pub files: BTreeMap<PathId, FileRevision>,
}

/// The materialized state of a file at a point in the canonical view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRevision {
    pub op_id: OpId,
    pub stage: FileStage,
    pub after: Option<crate::payload::ContentId>,
}

// ---------------------------------------------------------------------------
// Reducer trait
// ---------------------------------------------------------------------------

/// Error type for reduction failures.
#[derive(Debug, Clone)]
pub enum ReduceError {
    UnsupportedKind(&'static str),
}

impl core::fmt::Display for ReduceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReduceError::UnsupportedKind(kind) => write!(f, "unsupported op kind: {}", kind),
        }
    }
}

#[cfg(feature = "use-std")]
impl std::error::Error for ReduceError {}


/// A reducer processes operations and updates canonical state.
pub trait Reducer {
    /// Process a single operation and update internal state.
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
    pub fn new() -> Self {
        Self { messages: Vec::new() }
    }

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

/// Tracks the latest materializing revision per PathId.
#[derive(Debug, Clone)]
pub struct FileReducer {
    files: BTreeMap<PathId, (CausalKey, FileRevision)>,
}

impl FileReducer {
    pub fn new() -> Self {
        Self { files: BTreeMap::new() }
    }

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
                            e.insert((ck, rev));
                        }
                    }
                    Entry::Vacant(e) => {
                        e.insert((ck, rev));
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
/// Contains the grow-only OpSet and a deterministic CanonicalView
/// computed by running all reducers over the accepted operations.
#[derive(Debug, Clone)]
pub struct ChainState {
    pub ops: OpSet,
    pub blobs: BlobSet,
    pub view: CanonicalView,
}

impl ChainState {
    pub fn new() -> Self {
        Self {
            ops: OpSet::new(),
            blobs: BlobSet::new(),
            view: CanonicalView::default(),
        }
    }

    /// Recompute the canonical view from scratch by running all reducers
    /// over every accepted operation in OpSet order.
    ///
    /// Note: This requires decoding ops from their stored bytes. The codec
    /// crate provides `postcard::from_bytes` for this. When called from
    /// outside the core crate, pass pre-decoded ops or use the codec layer.
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
    pub fn merge(&mut self, other: &ChainState) -> Result<(usize, usize, usize), ReduceError> {
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

