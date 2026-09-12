//! Additive capability for a retained, revisioned live view.

use crate::{ExpansionSpanDto, HistoryRow, SnapshotId};
use serde::{Deserialize, Serialize};

/// Causally scheduled physical item order. The final slot is retained for disk compatibility.
pub type LiveOrder = (std::cmp::Reverse<u64>, String, u8);

/// Host-authorized provider capture configuration, never accepted from a webview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexLiveRequest {
    /// Original provider root, preserving portable cursor identities.
    pub sessions_root: String,
    /// Explicit helper executable supporting the persistent stream protocol.
    pub helper: String,
    /// Changed rollouts below the provider root.
    pub paths: Vec<String>,
}

/// Ordered replay cursor and optional provider capture request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncLiveRequest {
    /// Epoch supplied by the live bootstrap.
    pub epoch: SnapshotId,
    /// Last atomically applied revision.
    pub after_revision: u64,
    /// Only the trusted extension host can request file capture.
    pub codex: Option<CodexLiveRequest>,
}

/// Small topology entry used by both native and WASM rank/select indexes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveBlockMeta {
    /// Stable item/block identity.
    pub key: String,
    /// Monotone ordering clock, raised only to keep present parents below children.
    /// Physical row timestamps remain the original provider timestamps.
    pub sort_time: u64,
    /// Number of fully expanded rows in this block.
    pub row_count: u64,
    /// Expansion spans relative to the start of this block.
    pub spans: Vec<ExpansionSpanDto>,
    /// Current physical identity of the top-level row.
    #[serde(default)]
    pub node_key: String,
    /// Exact visible ancestors, expressed as stable block identities.
    #[serde(default)]
    pub parents: Vec<String>,
    /// Child-owned graph paths retain the Activity view's muted state.
    #[serde(default)]
    pub chain_state: editchain_core::taxonomy::ChainState,
    /// Task section owning this independent item; absent for ungrouped rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_group: Option<String>,
    /// Task path annotation on an existing physical item; never a separate row.
    #[serde(
        default,
        alias = "task_header",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_summary: Option<TaskGroupDto>,
    /// Failure, warning, cancellation or unresolved work that must stay exposed.
    #[serde(default)]
    pub task_protected: bool,
}

/// Native task summary for one connected, non-branching path of its activity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGroupDto {
    /// Full stable task incarnation identity shared by continued sections.
    pub task_id: String,
    /// Full provider thread identity.
    pub thread_id: String,
    /// Full provider task/turn identity.
    pub turn_id: String,
    /// Provider-observed lifecycle status; unknown is never treated as completion.
    pub status: TaskStatus,
    /// Earliest available user prompt, clipped for display; never a grouping boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Number of independently retained items in this section.
    pub member_count: u64,
    /// Existing physical item carrying the path summary.
    pub anchor: String,
    /// Native disclosure state, supplied on paged rows and absent in stored metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    /// This settled anchor currently represents its folded path. Fresh anchors keep their own content.
    #[serde(default)]
    pub summarized: bool,
}

/// Task lifecycle supplied by the provider's persisted turn metadata.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    /// No supported lifecycle observation is available.
    #[default]
    Unknown,
    /// Provider reports ongoing work.
    InProgress,
    /// Provider explicitly completed the task.
    Completed,
    /// Provider reports a failed task.
    Failed,
    /// Provider explicitly interrupted the task.
    Interrupted,
}

impl TaskStatus {
    /// Compact user-facing state, without inferring success from inactivity.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Status unknown",
            Self::InProgress => "In progress",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Interrupted => "Interrupted",
        }
    }
}

impl LiveBlockMeta {
    /// Stable order key; rank is derived rather than stored in each later row.
    #[must_use]
    pub fn order(&self) -> LiveOrder {
        (std::cmp::Reverse(self.sort_time), self.key.clone(), 1)
    }
}

/// One changed presentation block. Parent coordinates are block-relative.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveBlock {
    /// Ordering and disclosure metadata.
    pub meta: LiveBlockMeta,
    /// Only rows in this changed block.
    pub rows: Vec<HistoryRow>,
}

/// One-time live topology bootstrap; row content remains paged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveBaseline {
    /// Native visible coordinates; the baseline carries no global topology.
    #[serde(default)]
    pub paged: bool,
    /// Stable runtime epoch, replaced only by an explicit new bootstrap.
    pub epoch: SnapshotId,
    /// Revision represented by the bootstrap.
    pub revision: u64,
    /// Expanded row total.
    pub total: u64,
    /// Shared ordering and expansion metadata for retained blocks.
    pub blocks: Vec<LiveBlockMeta>,
}

/// Stage work and duration counters for one transaction, excluding bootstrap.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct LiveWork {
    /// Bytes read from changed provider sources.
    pub source_bytes: u64,
    /// Provider reducer records, excluding duplicate replies.
    pub provider_records: u64,
    /// Explicit provider cold starts.
    pub provider_bootstraps: u64,
    /// Source capture has a bounded backlog, independent of a filesystem stamp.
    #[serde(default)]
    pub provider_pending: bool,
    /// New chain bytes read.
    pub chain_bytes: u64,
    /// Encoded chain records decoded.
    pub chain_records: u64,
    /// Input operations in changed presentation blocks.
    pub presentation_ops: usize,
    /// Logical items reduced through dependency indexes.
    pub items: usize,
    /// Occurrence proofs revalidated.
    pub occurrences: usize,
    /// Blocks changed in the published view.
    pub blocks: usize,
    /// Capture and durable transaction duration.
    pub capture_ms: u64,
    /// Canonical tail and projection duration.
    pub projection_ms: u64,
}

/// Atomic view edit relative to a known revision in one epoch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveDelta {
    /// Visible total for clients using native disclosure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_total: Option<u64>,
    /// Revision required before applying this edit.
    pub base_revision: u64,
    /// Revision after applying this edit.
    pub revision: u64,
    /// Identity required by paged/detail requests after applying this edit.
    pub snapshot_id: SnapshotId,
    /// Retired presentation identities.
    pub removed: Vec<String>,
    /// Inserted, moved or revised blocks.
    pub upserts: Vec<LiveBlock>,
    /// New expanded row total.
    pub total: u64,
    /// Accepted operation count.
    pub chain_generation: u64,
    /// Retained global lane extent.
    pub max_lane: usize,
    /// Observable incremental work.
    pub work: LiveWork,
}

/// A bounded, ordered replay from the native runtime journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveUpdate {
    /// Runtime epoch to which every enclosed delta belongs.
    pub epoch: SnapshotId,
    /// Current revision, including when no new operations were admitted.
    pub revision: u64,
    /// Consecutive edits after the requested cursor.
    pub deltas: Vec<LiveDelta>,
    /// Work for this synchronization call, including an idle poll.
    pub work: LiveWork,
}
