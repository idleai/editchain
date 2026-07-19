use std::collections::HashMap;
use editchain_core::OpId;
use crate::data::header::{OpHeader, OpOrdinal};

/// Statistics about the loaded chain.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct ChainStatistics {
    pub total_ops: usize,
    pub total_segments: usize,
    pub total_bytes: u64,
    pub by_kind: HashMap<u8, usize>,
}

/// The in-memory snapshot of the chain — compact headers + indexes.
///
/// This is built once during loading and shared (behind Arc) with the UI thread.
/// Full operation payloads are NOT stored here; they are decoded lazily on demand.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TuiSnapshot {
    /// All operation headers in display order (causal/clock order).
    pub headers: Vec<OpHeader>,
    /// Map from OpId to ordinal index.
    pub by_id: HashMap<OpId, OpOrdinal>,
    /// Parent ordinals for each operation (0, 1, or 2 parents).
    pub parents: Vec<Vec<OpOrdinal>>,
    /// Child ordinals for each operation.
    pub children: Vec<Vec<OpOrdinal>>,
    /// Indexes for filtering: kind_code -> list of ordinals.
    pub by_kind: HashMap<u8, Vec<OpOrdinal>>,
    /// Indexes for filtering: actor -> list of ordinals.
    pub by_actor: HashMap<u64, Vec<OpOrdinal>>,
    /// Statistics.
    pub statistics: ChainStatistics,
}

impl TuiSnapshot {
    /// Create a new empty snapshot.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
            by_id: HashMap::new(),
            parents: Vec::new(),
            children: Vec::new(),
            by_kind: HashMap::new(),
            by_actor: HashMap::new(),
            statistics: ChainStatistics::default(),
        }
    }

    /// Look up the ordinal for an OpId.
    pub fn ordinal_of(&self, id: &OpId) -> Option<OpOrdinal> {
        self.by_id.get(id).copied()
    }

    /// Get the header at the given ordinal.
    pub fn header_at(&self, ordinal: OpOrdinal) -> Option<&OpHeader> {
        self.headers.get(ordinal as usize)
    }

    /// Total number of operations.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.headers.len()
    }

    /// Returns true if the snapshot is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }
}

impl Default for TuiSnapshot {
    fn default() -> Self {
        Self::new()
    }
}