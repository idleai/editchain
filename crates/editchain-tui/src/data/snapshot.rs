use crate::data::header::{OpHeader, OpOrdinal};
use editchain_core::OpId;
use std::collections::HashMap;

/// Statistics about the loaded chain.
#[expect(dead_code, reason = "WIP TUI — statistics display")]
#[derive(Debug, Clone, Default)]
pub(crate) struct ChainStatistics {
    pub total_ops: usize,
    pub total_segments: usize,
    pub total_bytes: u64,
    pub by_kind: HashMap<u8, usize>,
}

/// The in-memory snapshot of the chain — compact headers + indexes.
///
/// This is built once during loading and shared (behind Arc) with the UI thread.
/// Full operation payloads are NOT stored here; they are decoded lazily on demand.
#[derive(Debug, Clone)]
pub(crate) struct TuiSnapshot {
    /// All operation headers in display order (causal/clock order).
    pub headers: Vec<OpHeader>,
    /// Map from `OpId` to ordinal index.
    pub by_id: HashMap<OpId, OpOrdinal>,
    /// Parent ordinals for each operation (0, 1, or 2 parents).
    pub parents: Vec<Vec<OpOrdinal>>,
    /// Child ordinals for each operation.
    pub children: Vec<Vec<OpOrdinal>>,
    /// Indexes for filtering: `kind_code` -> list of ordinals.
    #[expect(dead_code, reason = "WIP TUI — filter functionality")]
    pub by_kind: HashMap<u8, Vec<OpOrdinal>>,
    /// Indexes for filtering: actor -> list of ordinals.
    #[expect(dead_code, reason = "WIP TUI — filter functionality")]
    pub by_actor: HashMap<u64, Vec<OpOrdinal>>,
    /// Statistics.
    pub statistics: ChainStatistics,
}

impl TuiSnapshot {
    /// Look up the ordinal for an `OpId`.
    pub(crate) fn ordinal_of(&self, id: &OpId) -> Option<OpOrdinal> {
        self.by_id.get(id).copied()
    }

    /// Get the header at the given ordinal.
    #[expect(
        clippy::as_conversions,
        reason = "OpOrdinal to usize cast is safe on all targets"
    )]
    pub(crate) fn header_at(&self, ordinal: OpOrdinal) -> Option<&OpHeader> {
        self.headers.get(ordinal as usize)
    }

    /// Total number of operations.
    #[expect(dead_code, reason = "WIP TUI — used for status display")]
    pub(crate) const fn len(&self) -> usize {
        self.headers.len()
    }

    /// Returns true if the snapshot is empty.
    #[expect(dead_code, reason = "WIP TUI — used for status display")]
    pub(crate) const fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }
}
