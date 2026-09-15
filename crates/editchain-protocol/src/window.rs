//! Conditional native windows: coordinates and content belong to one revision.

use crate::{HistoryRow, RowLocation, SnapshotId};
use serde::{Deserialize, Serialize};

/// A retained presentation row and the service's opaque content fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedRow {
    /// Stable presentation identity.
    pub key: String,
    /// Fingerprint of the complete previously served row, including geometry.
    pub version: String,
}

/// Locate anchors and reconcile a bounded window in one native request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileRowsRequest {
    /// Revision whose coordinates and content are requested.
    pub snapshot_id: SnapshotId,
    /// Selection, focus and scroll identities to resolve.
    pub keys: Vec<String>,
    /// Preferred scroll anchors, in fallback order. Empty when following the head.
    pub anchors: Vec<String>,
    /// Window origin if no preferred anchor survives.
    pub offset: u64,
    /// Rows preceding the resolved anchor to include.
    pub before: u16,
    /// Maximum rows to return, including the preceding margin.
    pub limit: u16,
    /// Client-owned content eligible for validated reuse.
    pub known: Vec<CachedRow>,
}

/// One row in a contiguous reconciled window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciledRow {
    /// Identity and fingerprint of the current row.
    #[serde(flatten)]
    pub cached: CachedRow,
    /// Absent only when the request advertised exactly this identity/version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<HistoryRow>,
}

/// Atomic window response; no coordinates refer to a previous revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciledWindow {
    /// Revision owning every entry.
    pub snapshot_id: SnapshotId,
    /// Resolved selection, focus and scroll anchors.
    pub locations: Vec<RowLocation>,
    /// Visible coordinate of the first row.
    pub offset: u64,
    /// Contiguous current rows, with unchanged content omitted.
    pub rows: Vec<ReconciledRow>,
    /// Current visible row count.
    pub total: u64,
    /// Canonical operation generation.
    pub chain_generation: u64,
    /// Current global graph extent.
    pub max_lane: usize,
}
