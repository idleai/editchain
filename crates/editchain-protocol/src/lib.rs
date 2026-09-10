//! Request/response DTOs for the `EditChain` VS Code service.
//!
//! These types are serialized over stdio between the thin TypeScript
//! extension host and the native Rust service.

mod error;
mod snapshot;
mod validation;
pub use error::{ErrorCode, ServiceError};
pub use snapshot::{OpenResponse, SnapshotId, SnapshotResult, PROTOCOL_VERSION};
pub use validation::{
    MAX_QUERY_BYTES, MAX_REQUEST_FRAME_BYTES, MAX_SEARCH_RESULTS, MAX_WINDOW_ROWS,
};

use serde::{Deserialize, Serialize};

use editchain_core::{GitAvailability, GitObjectFormat, GitSignature, Payload};

/// A request message from the extension host to the Rust service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Monotonic request ID for correlating responses.
    pub id: u64,
    /// The request body.
    pub body: RequestBody,
}

/// The body of a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestBody {
    /// Open a workspace and load its chain + git repositories.
    Open(OpenRequest),
    /// Reopen authoritative sources, bypassing derived caches after negotiation.
    Refresh(OpenRequest),
    /// Get a window of history rows.
    GetWindow(GetWindowRequest),
    /// Get details for a specific node.
    GetNodeDetails(GetNodeDetailsRequest),
    /// Find ranked lexical hits resolved to visible top-level history rows.
    ///
    /// Every match is resolved to the real visible top-level row and its
    /// absolute expanded-history parent-row offset, so the viewer can cycle
    /// matches without auto-expanding or scanning rows.
    FindInHistory(FindInHistoryRequest),
    /// Resolve a git object by OID.
    ResolveObject(ResolveObjectRequest),
    /// Materialize the immutable text sides for one advertised file change.
    GetFileDiff(GetFileDiffRequest),
}

/// A response message from the Rust service to the extension host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// The request ID this responds to.
    pub id: u64,
    /// The response body.
    pub body: ResponseBody,
}

/// The body of a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseBody {
    /// Successful result with a value.
    Ok(serde_json::Value),
    /// A structured error; readers also accept legacy string errors.
    Error(ServiceError),
}

/// Open a workspace and load its chain + git repositories.
///
/// The `Open` response is a JSON object with `workspace`, `chain`, `repos`,
/// `nodes`, and `chain_generation` keys, plus two backward-compatible
/// additions that describe what the open had to reconcile:
///
/// - `diagnostics` — `chain` (records decoded, accepted, exact `OpId` replays
///   ignored, and same-id conflicts quarantined through the core `OpSet`) and
///   `blobs` (bounded row previews read, full payloads deferred, explicit
///   hydrations/verified refs, and refs missing, corrupt, or not addressable by
///   the store).
/// - `warnings` — human-readable strings for any non-zero diagnostic count
///   (duplicates, quarantines, missing/corrupt blobs), so a client can surface
///   hydration gaps without silently treating preserved `BlobRef`s as content.
///
/// The request keeps its legacy shape. Clients validate [`OpenResponse`] before
/// issuing requests that require the negotiated snapshot identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    /// Path to the workspace root.
    pub workspace_path: String,
    /// Path to the chain directory (may be empty if none).
    pub chain_dir: String,
}

/// Get a window of history rows (cursor-based paging).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetWindowRequest {
    /// Snapshot returned by Open. Empty legacy tokens are rejected by the service.
    #[serde(default)]
    pub snapshot_id: SnapshotId,
    /// Cursor offset into the history (0 = newest).
    pub offset: u64,
    /// Number of rows to return.
    pub limit: u64,
    /// Include globally stable lane/edge geometry in this response.
    ///
    /// The production viewer sends `false` for its first page so content rows
    /// can paint before O(V) layout, then repeats that window with `true`.
    pub include_layout: bool,
}

/// Get details for a specific node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetNodeDetailsRequest {
    /// Snapshot that advertised this operation identity.
    #[serde(default)]
    pub snapshot_id: SnapshotId,
    /// The operation ID to inspect, in display form `"node:boot:seq"`.
    ///
    /// Stored as a string so it round-trips through JavaScript without precision
    /// loss on u64 node values that exceed 2^53.
    pub op_id: String,
}

/// Find ranked lexical hits resolved to visible top-level history rows.
///
/// The service runs a BM25 lexical search, resolves every scored chunk to the
/// top-level history row that actually renders it in the fixed Activity view,
/// deduplicates multiple chunks/children
/// that map to the same row (keeping the best score), and filters out hits with
/// no row in that view. It never auto-expands or changes expansion state: each
/// match carries the stable real `node_key` of its top-level row plus the
/// absolute expanded-history parent-row offset (0 = newest) that matches
/// `GetWindow` offsets and the fixed expanded-row coordinates the viewer
/// already retains.
///
/// Resolution runs against the same fixed Activity snapshot as `GetWindow`,
/// never a rebuilt O(V) view per arrow press.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindInHistoryRequest {
    /// Snapshot whose row coordinates must be used for every match.
    #[serde(default)]
    pub snapshot_id: SnapshotId,
    /// The query string (BM25 lexical search only).
    pub query: String,
    /// Number of candidate chunks to retrieve from the index before row
    /// resolution and deduplication. When the candidate list hits this cap the
    /// response reports `more: true` — it never claims an exact total.
    pub top_k: usize,
}

/// Resolve a git object by OID in a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveObjectRequest {
    /// Snapshot whose repository catalog advertised this identity.
    #[serde(default)]
    pub snapshot_id: SnapshotId,
    /// Repository identity as an exact decimal `RepositoryId` string (u64
    /// values above 2^53 must not be rounded by JavaScript).
    pub repository: String,
    /// Object OID to resolve, as lowercase hex (40 chars SHA-1 / 64 SHA-256).
    pub oid: String,
}

/// Request the before/after text for one file row previously advertised by a
/// history window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetFileDiffRequest {
    /// Snapshot that advertised this change identity.
    #[serde(default)]
    pub snapshot_id: SnapshotId,
    /// Complete identity of the advertised change. The service revalidates it
    /// against Git objects or the canonical source operation before returning
    /// any content.
    pub change: FileChangeDto,
}

/// Provenance domain for one file change row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileChangeSource {
    /// Immutable Git commit/tree objects.
    Git,
    /// An imported Claude/Codex operation.
    Agent,
    /// Forward-compatible unknown source.
    #[serde(other)]
    Unknown,
}

/// Source-control-style status for one changed path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeStatus {
    /// A new path.
    Added,
    /// An existing path whose content or executable bit changed.
    Modified,
    /// A removed path.
    Deleted,
    /// A path moved from [`FileChangeDto::old_path`].
    Renamed,
    /// A path copied from [`FileChangeDto::old_path`].
    Copied,
    /// The Git entry kind changed (for example file to symlink).
    TypeChanged,
    /// Forward-compatible unknown status.
    #[serde(other)]
    Unknown,
}

/// Display and immutable-content identity for one expandable file row.
///
/// Git identities use full object IDs. Agent identities use an exact `OpId`;
/// their retained content may be a snippet or hunk rather than a whole-file
/// snapshot, which is surfaced by [`Self::partial`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeDto {
    /// Provenance domain.
    pub source: FileChangeSource,
    /// Current/destination path, normalized for workspace display.
    pub path: String,
    /// Previous/source path for renames and copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Source-control-style status.
    pub status: FileChangeStatus,
    /// Whether either side is not representable as a text document.
    #[serde(default)]
    pub binary: bool,
    /// Whether the retained agent evidence is a snippet/hunk rather than two
    /// complete file snapshots. Always `false` for resolved Git blobs.
    #[serde(default)]
    pub partial: bool,
    /// Exact imported operation identity for agent changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
    /// Exact repository identity for Git changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Path relative to the anchored repository tree. This can differ from
    /// [`Self::path`] when the workspace contains a nested repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_path: Option<String>,
    /// Commit whose first-parent diff advertised this row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_oid: Option<String>,
    /// First-parent blob/gitlink object, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_oid: Option<String>,
    /// Commit-side blob/gitlink object, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_oid: Option<String>,
    /// First-parent Git entry mode (`blob`, `exe`, `link`, or `commit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_mode: Option<String>,
    /// Commit-side Git entry mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_mode: Option<String>,
}

/// One independently recorded hunk from a unified diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiffHunkDto {
    /// Original unified-diff header, including source ranges and any section
    /// heading.
    pub header: String,
    /// Recorded source-side lines for this hunk.
    pub before: String,
    /// Recorded destination-side lines for this hunk.
    pub after: String,
}

/// Materialized text shown by VS Code's native diff editors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiffDto {
    /// Current/destination path.
    pub path: String,
    /// Previous/source path for renames and copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Source-control-style status.
    pub status: FileChangeStatus,
    /// Whether this change must not be opened through a text provider.
    #[serde(default)]
    pub binary: bool,
    /// Whether these sides are recorded snippets/hunks rather than complete
    /// files.
    #[serde(default)]
    pub partial: bool,
    /// Complete source text or a lone recorded hunk. Empty for an exact
    /// addition or when `hunks` contains disconnected regions.
    pub before: String,
    /// Complete destination text or a lone recorded hunk. Empty for an exact
    /// deletion or when `hunks` contains disconnected regions.
    pub after: String,
    /// Disconnected unified-diff regions that VS Code should render as
    /// independent entries in its changes editor.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<FileDiffHunkDto>,
    /// Optional fidelity/availability explanation for the editor title or UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The full resolved git commit, JSON-safe for the read-only JSON editor.
///
/// This mirrors [`editchain_core::GitCommitEntity`] with every identity
/// carried as an exact string so u64 values above 2^53 round-trip through
/// JavaScript without precision loss:
///
/// - `repository` is an exact decimal `RepositoryId` string;
/// - `oid`, `tree`, and `parents` are lowercase hex `GitOid` strings;
/// - `imported_record` is the `"node:boot:seq"` display form, when present;
/// - `changed_paths` are exact decimal `PathId` strings.
///
/// Safe enums (`object_format`, `availability`), timestamps, signatures, and
/// payloads retain their native types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedObject {
    /// Repository identity as an exact decimal `RepositoryId` string.
    pub repository: String,
    /// Object format of the repository.
    pub object_format: GitObjectFormat,
    /// Full commit OID as lowercase hex.
    pub oid: String,
    /// `EditChain` operation that imported this commit, if any, in display
    /// form `"node:boot:seq"`.
    pub imported_record: Option<String>,
    /// Availability of the underlying object data.
    pub availability: GitAvailability,
    /// Tree OID referenced by this commit, as lowercase hex.
    pub tree: String,
    /// Parent commit OIDs (ancestry), as lowercase hex.
    pub parents: Vec<String>,
    /// Author signature.
    pub author: GitSignature,
    /// Committer signature.
    pub committer: GitSignature,
    /// Author timestamp (Unix seconds).
    pub authored_at: i64,
    /// Commit timestamp (Unix seconds).
    pub committed_at: i64,
    /// Commit message (subject + body).
    pub message: Payload,
    /// Refs observed at import time (snapshot).
    pub imported_refs: Vec<Payload>,
    /// Refs observed live (snapshot; may change).
    pub live_refs: Vec<Payload>,
    /// Paths changed by this commit, as exact decimal `PathId` strings.
    pub changed_paths: Vec<String>,
}

/// A history row in the unified projection (`EditChain` op or `Git` commit).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "flat versioned wire DTO: each boolean is an independent backward-compatible serde-defaulted flag the viewer toggles (group boundary/submodule/system/subop/promoted); refactoring to enums would churn the wire contract"
)]
pub struct HistoryRow {
    /// The operation ID (for `EditChain` ops) in display form `"node:boot:seq"`,
    /// or `None` for git commits. Stored as a string to avoid JS precision loss.
    pub op_id: Option<String>,
    /// The git commit OID (for git commits) as lowercase hex — a string so it
    /// round-trips exactly through JavaScript.
    pub git_oid: Option<String>,
    /// The repository (for git commits) as an exact decimal `RepositoryId`
    /// string — a string so u64 values above 2^53 round-trip exactly.
    pub repository: Option<String>,
    /// Display summary text.
    pub summary: String,
    /// Timestamp in Unix ms (0 if unknown).
    pub timestamp_ms: u64,
    /// Grouping key for block separation (session id for ops, repo id for git).
    pub group: String,
    /// Whether this is the final top-level graph node in its contiguous group
    /// run. The service computes this against the complete filtered snapshot,
    /// so clients never infer a false boundary at a virtual-window edge.
    #[serde(default)]
    pub group_end: bool,
    /// Stable graph key: operation ID or repository-qualified Git commit key.
    pub node_key: String,
    /// Parent node keys (for drawing graph edges).
    pub parents: Vec<String>,
    /// Provider-neutral relationship kinds for the edges in [`Self::parents`].
    ///
    /// Each entry pairs one drawn parent with the semantic kind of the edge,
    /// so the viewer can annotate compact branch/start and return/completion
    /// semantics without parsing provider-specific raw JSON:
    ///
    /// - `"subagent"` — the parent edge is a `SubagentOf` structural note:
    ///   this row starts a subagent branch spawned by the target row.
    /// - `"reconnect"` — the parent edge is a `ReconnectsTo` structural note:
    ///   this row is the parent thread's completion result returning into the
    ///   target row (the subagent's last op).
    /// - `"fork"` — the parent edge is a `ForkOf` structural note: this row
    ///   branches off the target row at a fork divergence boundary.
    /// - `"produced_commit"` — this Git row was produced by the parent command
    ///   operation.
    ///
    /// One entry is listed per parent key in [`Self::parents`] whose edge is
    /// structural (the row's final lifted parents after filtering/splicing),
    /// so the client can match relations to the parent keys it renders and
    /// annotate the row itself — a `"subagent"` relation marks this row as a
    /// branch start, `"reconnect"` as a return/completion row. Absent on older
    /// services or plain edges — the list is empty then, never `null`.
    #[serde(default)]
    pub parent_relations: Vec<ParentRelationDto>,
    /// Whether this row belongs to a nested/submodule repository.
    pub is_submodule: bool,
    /// Whether this is a system-generated node (tool results, raw import
    /// records) rather than user-facing text. The viewer uses this to dim or
    /// hide such rows.
    #[serde(default)]
    pub is_system: bool,
    /// Author display name (git commits only; empty for ops).
    #[serde(default)]
    pub author: String,
    /// Commit/ID display value (abbreviated git OID or op id).
    #[serde(default)]
    pub commit_id: String,
    /// Short type tag for styling (e.g. "tool", "message", "command", "git").
    #[serde(default)]
    pub kind: String,
    /// The graph lane this row's node occupies (for per-row graph rendering).
    #[serde(default)]
    pub lane: usize,
    /// Lanes with a vertical segment in the TOP half of this row's cell (lines
    /// entering from above). Tips (newest nodes) have none here — no line above
    /// their dot.
    #[serde(default)]
    pub above: Vec<usize>,
    /// Lanes with a vertical segment in the BOTTOM half of this row's cell (lines
    /// leaving downward). Roots (no parents) have none here — no line below.
    #[serde(default)]
    pub below: Vec<usize>,
    /// Horizontal lane-jog segments at this row: `(from_lane, to_lane)` merge
    /// connectors (per-row graph cells).
    #[serde(default)]
    pub transitions: Vec<(usize, usize)>,
    /// Subset of [`Self::above`] whose edge ownership is exclusively muted.
    /// Omitted when empty for compact snapshot rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub muted_above: Vec<usize>,
    /// Subset of [`Self::below`] whose edge ownership is exclusively muted.
    /// Omitted when empty for compact snapshot rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub muted_below: Vec<usize>,
    /// Subset of [`Self::transitions`] whose edge ownership is exclusively
    /// muted. Omitted when empty for compact snapshot rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub muted_transitions: Vec<(usize, usize)>,
    /// Bundled metadata sub-ops attached to this row (revealed on click).
    #[serde(default)]
    pub sub_ops: Vec<SubOpSummary>,
    /// Whether this row is a bundled sub-op expanded inline under its parent
    /// (rather than a top-level node). Sub-op rows carry no graph dot of their
    /// own; they inherit the parent's lane for a continuation line.
    #[serde(default)]
    pub is_subop: bool,
    /// Nesting depth in the expandable presentation tree. Top-level graph rows
    /// are `0`; direct work-group members are `1`; children of an existing
    /// bundle/member are `2`. Older one-level services omit this and default to
    /// `0` (the `is_subop` flag remains the compatibility signal).
    #[serde(default)]
    pub hierarchy_depth: u8,
    /// Absolute row index of the direct parent row for nested rows. `None` on
    /// top-level graph rows.
    #[serde(default)]
    pub parent_row: Option<usize>,
    /// Semantic class for the sub-op's icon (e.g. `"meta"`, `"edit"`, `"msg"`,
    /// `"tool_result"`). `None` on top-level rows.
    #[serde(default)]
    pub subop_kind: Option<String>,
    /// Provider-neutral record role (narrative/action/result/artifact/
    /// lifecycle/echo/unknown). Serialized as a lowercase `snake_case` string;
    /// unknown values deserialize to `Unknown` for forward compatibility.
    #[serde(default)]
    pub record_role: editchain_core::taxonomy::RecordRole,
    /// Provider-neutral activity kind (conversation/plan/execute/change/...).
    /// Serialized as a lowercase `snake_case` string; unknown values deserialize
    /// to `Unknown` for forward compatibility.
    #[serde(default)]
    pub activity_kind: editchain_core::taxonomy::ActivityKind,
    /// Render prominence (primary/supporting/trace). Trace rows are omitted
    /// from the fixed Activity view.
    #[serde(default)]
    pub visibility: editchain_core::taxonomy::Visibility,
    /// Concluded outcome (success/warning/failure/cancelled/unknown). Unknown
    /// is the default — success is never inferred without structured evidence.
    #[serde(default)]
    pub outcome: editchain_core::taxonomy::Outcome,
    /// Reusable presentation state for this row and its child-owned graph edge.
    /// Active is the backward-compatible default and is omitted on the wire.
    #[serde(
        default,
        skip_serializing_if = "editchain_core::taxonomy::ChainState::is_active"
    )]
    pub chain_state: editchain_core::taxonomy::ChainState,
    /// Provider-neutral turn identity as an exact decimal string (u64 values
    /// above 2^53 round-trip through JavaScript without precision loss).
    /// `None` when the row is not turn-scoped.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// Small display-safe subset of the owning Codex session metadata.
    ///
    /// The service attaches this to session-scoped rows so clients can label a
    /// session boundary without parsing raw import JSON. Both members are
    /// optional because older providers and older imports may omit either one.
    #[serde(default)]
    pub session_meta: Option<SessionMetaDto>,
    /// Whole-session summary metadata on the session's newest visible row.
    ///
    /// This is independent of [`Self::work_unit`]: providers such as Codex mix
    /// turn-scoped and session-scoped rows, so a work-unit boundary is not
    /// necessarily a boundary for the complete session. `None` on every other
    /// row and on services that predate this additive field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_summary: Option<SessionSummaryDto>,
    /// Stable, additive work-unit metadata for boundary/header rendering.
    ///
    /// Every row in a window carries its opaque work-unit id plus view-stable
    /// boundary flags, so the client can render a unit header (at
    /// [`WorkUnitDto::is_start`]) without inferring boundaries across paged
    /// windows. `None` only on older services that predate the field.
    #[serde(default)]
    pub work_unit: Option<WorkUnitDto>,
    /// Conservative promotion marker: `true` when this row is significant
    /// enough that the Activity projection must never fold it into a bundled
    /// execute run (warning/failure/cancelled outcome, change/verify activity,
    /// or a deterministically known unit-final narrative/final decision).
    /// Bundling-eligible execute rows are never promoted.
    #[serde(default)]
    pub promoted: bool,
    /// Typed metadata for a synthetic Activity-view execute-run bundle row.
    ///
    /// `Some` only for top-level execute bundle rows (the synthetic Activity
    /// projection node folding a contiguous execute run): the client can then
    /// distinguish a bundled execute run from an ordinary execute row with
    /// `sub_ops` without parsing the display summary string. `None` on every
    /// ordinary row and expanded sub-op row. Older
    /// services that predate the field omit it entirely, so it defaults to
    /// `None`.
    #[serde(default)]
    pub activity_bundle: Option<ActivityBundleDto>,
    /// Source-control-style metadata for a nested file row. `None` on every
    /// graph node and non-file descendant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_change: Option<FileChangeDto>,
}

/// Display-safe provenance copied from a session's `session_meta` record.
///
/// This deliberately remains a tiny subset of the provider payload: model and
/// agent labels are useful history chrome, while instructions, environment,
/// and other large or sensitive session fields stay in the raw record only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetaDto {
    /// Human-friendly session title captured from the provider's durable
    /// rename metadata (for example a Claude `custom-title` or Codex thread
    /// index entry).
    #[serde(default)]
    pub session_title: Option<String>,
    /// Model/provider label recorded by the session (for example
    /// `sglang_dsv4`).
    #[serde(default)]
    pub model_provider: Option<String>,
    /// Human-friendly agent nickname, when the provider assigned one.
    #[serde(default)]
    pub agent_nickname: Option<String>,
}

/// View-wide metadata attached to the true newest row of one session group.
///
/// Presence identifies the significant session-summary row without asking the
/// client to infer it from provider-specific scope or adjacent paged rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummaryDto {
    /// Total top-level rows in this session for the current projected view.
    #[serde(default)]
    pub count: u64,
}

/// Stable metadata for the work unit one history row belongs to.
///
/// Serialized inline on [`HistoryRow::work_unit`]. The id is opaque and
/// provider-neutral; the client compares ids only for equality and renders a
/// unit header when [`Self::is_start`] is `true`. All fields except `id` are
/// serde-defaulted so older payloads and hand-written JSON tolerate missing
/// members (forward/backward compatibility).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkUnitDto {
    /// Opaque work-unit id (stable within a view snapshot; e.g. a turn
    /// identity for turn-scoped rows, else the row's group key). Rows that
    /// share an id form one unit, and ids never repeat across unrelated units
    /// in a view.
    pub id: String,
    /// Whether this row is the FIRST row of its unit in the view's display
    /// order (newest-first). The client renders the unit boundary/header here.
    #[serde(default)]
    pub is_start: bool,
    /// Whether this row is the LAST row of its unit in display order. Absent
    /// on older clients/services it is simply ignored; present rows can close
    /// the unit's rendered section.
    #[serde(default)]
    pub is_end: bool,
    /// Deterministic unit title when evidence supports one: the display
    /// summary of the unit's oldest primary narrative row (the initiating
    /// request for a turn). `None` when the unit has no narrative evidence.
    #[serde(default)]
    pub title: Option<String>,
    /// Total number of top-level rows in this unit for the current view
    /// (computed over the full view, so stable across paged windows).
    #[serde(default)]
    pub count: u64,
}

/// Typed metadata for a synthetic Activity-view execute-run bundle row.
///
/// Serialized inline on [`HistoryRow::activity_bundle`]. `member_count` is the
/// ORIGINAL top-level run member count (`member_nodes.len()`), never the
/// flattened metadata-subop count, so the client can render faithful
/// "N steps" labels from structured data. The bundle kind is an enum so new
/// bundle kinds stay forward-compatible with older viewers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityBundleDto {
    /// Provider-neutral kind of this activity bundle row.
    pub kind: ActivityBundleKind,
    /// Number of original top-level member rows folded into the bundle
    /// (exact, as a u64).
    pub member_count: u64,
}

/// Provider-neutral kinds for an activity bundle row.
///
/// Serialized as kebab-case strings (`"work-group"`, `"execute-run"`,
/// `"plan-repeat"`). Unknown strings deserialize to [`Self::Unknown`] so
/// older clients tolerate new bundle kinds from newer services.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityBundleKind {
    /// All linear non-chat activity between conversational boundaries. Existing
    /// execute/plan bundles remain expandable children of this outer group.
    WorkGroup,
    /// A synthetic Activity-view summary node folding a maximal contiguous
    /// run of low-signal execute rows into one expandable run.
    ExecuteRun,
    /// Adjacent primary Plan narratives that repeat the same normalized
    /// heading, retained as expandable original reasoning records.
    PlanRepeat,
    /// A bundle kind this client does not recognize (forward compatibility).
    #[serde(other)]
    Unknown,
}

/// One typed parent edge on a history row.
///
/// `parent` is a node key from [`HistoryRow::parents`]; `kind` is a
/// provider-neutral relationship kind (see [`ParentRelationKind`]). Unknown
/// kinds deserialize to [`ParentRelationKind::Unknown`], so a newer service
/// never breaks an older viewer (forward compatibility with new structural
/// relationships).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentRelationDto {
    /// The parent node key this relation applies to (matches a key in
    /// [`HistoryRow::parents`]).
    pub parent: String,
    /// Provider-neutral relationship kind of this parent edge.
    pub kind: ParentRelationKind,
}

/// Provider-neutral relationship kinds for a structural parent edge.
///
/// Serialized as lowercase strings (`"subagent"`, `"reconnect"`, `"fork"`,
/// `"produced_commit"`).
/// Unknown strings deserialize to [`Self::Unknown`] so clients tolerate new
/// structural relationships from newer services; the viewer ignores unknown
/// kinds instead of breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParentRelationKind {
    /// The row starts a subagent branch spawned by the target row.
    Subagent,
    /// The row is the parent thread's completion result returning into the
    /// subagent branch (the target row is the subagent's last op).
    Reconnect,
    /// The row branches off the target row at a fork divergence boundary.
    Fork,
    /// A Git commit row was produced by the parent command operation.
    #[serde(rename = "produced_commit")]
    ProducedCommit,
    /// A relationship kind this client does not recognize (forward
    /// compatibility).
    #[serde(other)]
    Unknown,
}

/// A bundled metadata sub-op attached to a history row.
///
/// Metadata-only records (e.g. `last-prompt`, `permission-mode`, `custom-title`,
/// `mode`) carry no user-facing content and are bundled as sub-ops of a real
/// turn/tool node rather than occupying their own graph row/lane. The viewer
/// reveals them on click, like git-graph commit detail expansion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubOpSummary {
    /// The operation ID (display form `"node:boot:seq"`).
    pub op_id: String,
    /// Display summary (e.g. the record type or a short label).
    #[serde(default)]
    pub summary: String,
    /// Short type tag (e.g. "last-prompt", "mode", "permission-mode").
    #[serde(default)]
    pub kind: String,
    /// Timestamp in Unix ms (0 if unknown).
    #[serde(default)]
    pub timestamp_ms: u64,
}

/// A window of history rows with generation counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryWindow {
    /// Exact opened source/view identity shared by rows, expansion, and layout.
    pub snapshot_id: SnapshotId,
    /// The rows in this window (newest-first).
    pub rows: Vec<HistoryRow>,
    /// Total number of rows available.
    pub total: u64,
    /// Chain generation at snapshot time.
    pub chain_generation: u64,
    /// The maximum graph lane across ALL rows (global), so the client can size
    /// the graph column stably regardless of which window is loaded.
    #[serde(default)]
    pub max_lane: usize,
    /// Global per-top-level-node bundled sub-op counts for this snapshot.
    /// Present on the offset-zero window that establishes a snapshot and omitted
    /// from subsequent pages so response size remains proportional to `limit`.
    /// Retained for top-level block lookup and compatibility with one-level
    /// clients; current clients use `expansion_spans` for nested visibility.
    #[serde(default)]
    pub sub_op_counts: Option<Vec<usize>>,
    /// Global expandable-row spans for this fixed snapshot. Each entry names an
    /// absolute row and the number of contiguous descendant slots immediately
    /// following it. Present only on the offset-zero window, like
    /// `sub_op_counts`. This additive index generalizes one-level sub-op
    /// collapse to the bounded two-level work-group hierarchy.
    #[serde(default)]
    pub expansion_spans: Option<Vec<ExpansionSpanDto>>,
    /// Whether lane/connector fields contain the globally computed layout.
    /// `false` denotes a row-complete provisional first paint.
    #[serde(default)]
    pub layout_ready: bool,
}

/// One collapsible row's contiguous descendant interval in expanded history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpansionSpanDto {
    /// Absolute expanded-history row occupied by the expandable parent.
    pub row: u64,
    /// Number of descendant slots following `row` in depth-first order.
    pub descendant_count: u64,
}

/// Details for a single history node (for the inspector).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDetails {
    /// The operation ID (display form `"node:boot:seq"`), if this is an
    /// `EditChain` op. Stored as a string to avoid JS precision loss.
    pub op_id: Option<String>,
    /// The git commit OID (for git commits) as lowercase hex — a string so it
    /// round-trips exactly through JavaScript.
    pub git_oid: Option<String>,
    /// The repository (for git commits) as an exact decimal `RepositoryId`
    /// string — a string so u64 values above 2^53 round-trip exactly.
    pub repository: Option<String>,
    /// Display summary.
    pub summary: String,
    /// Full payload text (message/content), if available.
    pub body: String,
    /// Parent operation IDs (for `EditChain` ops) as exact `"node:boot:seq"`
    /// strings.
    pub parents: Vec<String>,
    /// Parent commit OIDs (for git commits) as lowercase hex strings.
    pub git_parents: Vec<String>,
    /// Refs pointing at this commit (for git commits).
    pub refs: Vec<String>,
    /// Changed paths (for git commits).
    pub changed_paths: Vec<String>,
}

/// A Find-in-Chain match: one distinct visible top-level history row.
///
/// Multiple scored chunks and folded children that resolve to the same visible
/// row are deduplicated into one match, keeping the best (highest) BM25 score.
/// `row` is the absolute expanded-history parent-row offset of the containing
/// top-level row — the same coordinate the viewer derives from its
/// fixed expanded coordinates and the `parent_row` values in `GetWindow`
/// responses, so arrow-key navigation never needs to auto-expand anything.
///
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindInHistoryMatch {
    /// The stable real node key of the visible top-level row that renders this
    /// hit (`"node:boot:seq"` for `EditChain` rows, lowercase OID hex for git).
    pub node_key: String,
    /// Absolute expanded-history parent-row offset (0 = newest) of the
    /// containing top-level row, compatible with `GetWindow` offsets and
    /// `ViewSnapshot.starts`.
    pub row: u64,
}

/// A Find-in-Chain response.
///
/// `returned` is the exact number of distinct visible matches in `matches`;
/// `more` reports whether additional matches **may** exist because the
/// `candidate/top_k` limit truncated retrieval. When `more` is `true` the
/// response never claims an exact total.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindInHistoryResponse {
    /// Snapshot whose fixed expanded coordinates are returned below.
    pub snapshot_id: SnapshotId,
    /// Distinct visible matches, one per top-level history row, ranked by best
    /// BM25 score (highest first; ties broken by newest row first).
    pub matches: Vec<FindInHistoryMatch>,
    /// Whether more matches may exist due to `candidate/top_k` truncation.
    pub more: bool,
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "Tests index into freshly constructed serde_json::Value trees"
)]
mod tests {
    use super::*;
    use editchain_core::{GitOid, NodeId, OpId};

    /// 2^53 + 1 — the first integer JavaScript's IEEE-754 doubles round.
    const OVER_2_53: u64 = 9_007_199_254_740_993;

    #[test]
    fn request_limits_reject_zero_oversize_and_inexact_coordinates() {
        for body in [
            serde_json::json!({"GetWindow": {"offset": 0, "limit": 0, "include_layout": false}}),
            serde_json::json!({"GetWindow": {"offset": 0, "limit": 10001, "include_layout": false}}),
            serde_json::json!({"GetWindow": {"offset": OVER_2_53, "limit": 1, "include_layout": false}}),
            serde_json::json!({"GetWindow": {"offset": u64::MAX, "limit": 1, "include_layout": false}}),
            serde_json::json!({"FindInHistory": {"query": "needle", "top_k": 0}}),
            serde_json::json!({"FindInHistory": {"query": "needle", "top_k": 1001}}),
            serde_json::json!({"FindInHistory": {"query": "x".repeat(MAX_QUERY_BYTES.saturating_add(1)), "top_k": 1}}),
        ] {
            let request: RequestBody = serde_json::from_value(body).unwrap();
            assert_eq!(
                request.validate().unwrap_err().code,
                ErrorCode::InvalidInput
            );
        }
        for body in [
            serde_json::json!({"GetWindow": {"offset": 0, "limit": MAX_WINDOW_ROWS, "include_layout": true}}),
            serde_json::json!({"FindInHistory": {"query": "x".repeat(MAX_QUERY_BYTES), "top_k": MAX_SEARCH_RESULTS}}),
        ] {
            let request: RequestBody = serde_json::from_value(body).unwrap();
            assert!(request.validate().is_ok());
        }
    }

    #[test]
    fn structured_errors_preserve_codes_and_accept_legacy_messages() {
        let body = ResponseBody::Error(ServiceError::new(
            ErrorCode::StaleSnapshot,
            "reopen history",
        ));
        let value = serde_json::to_value(body).unwrap();
        assert_eq!(value["Error"]["code"], "stale_snapshot");
        assert_eq!(value["Error"]["message"], "reopen history");
        let legacy: ResponseBody =
            serde_json::from_value(serde_json::json!({"Error": "old service"})).unwrap();
        assert!(matches!(legacy, ResponseBody::Error(error)
            if error.code == ErrorCode::Internal && error.message == "old service"));
    }

    fn big_op_id() -> OpId {
        OpId::new(NodeId(OVER_2_53), 7, 42)
    }

    fn big_oid() -> GitOid {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xde;
        bytes[1] = 0xad;
        GitOid::new(GitObjectFormat::Sha1, bytes)
    }

    /// The 40-char SHA-1 hex form of [`big_oid`].
    fn big_oid_hex() -> String {
        format!("dead{}", "0".repeat(36))
    }

    #[test]
    fn history_row_identifiers_serialize_as_exact_strings() {
        let row = HistoryRow {
            op_id: Some(big_op_id().to_string()),
            git_oid: Some(big_oid().to_hex()),
            repository: Some(OVER_2_53.to_string()),
            summary: "row".to_string(),
            timestamp_ms: 1_700_000_000_000,
            group: "repo:big".to_string(),
            group_end: true,
            node_key: big_op_id().to_string(),
            parents: vec![big_op_id().to_string()],
            parent_relations: vec![ParentRelationDto {
                parent: big_op_id().to_string(),
                kind: ParentRelationKind::Subagent,
            }],
            is_submodule: false,
            is_system: false,
            author: String::new(),
            commit_id: String::new(),
            kind: "git".to_string(),
            lane: 0,
            above: Vec::new(),
            below: Vec::new(),
            transitions: Vec::new(),
            muted_above: Vec::new(),
            muted_below: Vec::new(),
            muted_transitions: Vec::new(),
            sub_ops: Vec::new(),
            is_subop: false,
            hierarchy_depth: 0,
            parent_row: None,
            subop_kind: None,
            record_role: editchain_core::taxonomy::RecordRole::Artifact,
            activity_kind: editchain_core::taxonomy::ActivityKind::SourceControl,
            visibility: editchain_core::taxonomy::Visibility::Primary,
            outcome: editchain_core::taxonomy::Outcome::Success,
            chain_state: editchain_core::taxonomy::ChainState::Active,
            turn_id: Some(OVER_2_53.to_string()),
            session_meta: None,
            session_summary: None,
            work_unit: None,
            promoted: false,
            activity_bundle: None,
            file_change: None,
        };
        let json = serde_json::to_value(&row).expect("serialize");
        assert_eq!(json["op_id"], "9007199254740993:7:42");
        assert_eq!(json["git_oid"], big_oid_hex());
        assert_eq!(json["repository"], "9007199254740993");
        assert_eq!(json["parents"][0], "9007199254740993:7:42");
        assert_eq!(
            json["parent_relations"][0]["parent"],
            "9007199254740993:7:42"
        );
        assert_eq!(json["parent_relations"][0]["kind"], "subagent");
        assert_eq!(json["timestamp_ms"], 1_700_000_000_000u64);
        // Exact round-trip through deserialization.
        let back: HistoryRow = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.repository.as_deref(), Some("9007199254740993"));
        assert_eq!(back.git_oid.as_deref(), Some(big_oid_hex().as_str()));
        assert_eq!(back.parent_relations[0].kind, ParentRelationKind::Subagent);
        // Taxonomy values serialize as stable lowercase `snake_case` strings and
        // turn identity round-trips as an exact decimal string above 2^53.
        let round_trip = serde_json::to_value(&back).expect("re-serialize");
        assert_eq!(round_trip["record_role"], "artifact");
        assert_eq!(round_trip["activity_kind"], "source_control");
        assert_eq!(round_trip["visibility"], "primary");
        assert_eq!(round_trip["outcome"], "success");
        assert!(
            round_trip.get("chain_state").is_none(),
            "the default active state stays compact on the wire"
        );
        assert_eq!(round_trip["turn_id"], "9007199254740993");
        assert_eq!(
            back.record_role,
            editchain_core::taxonomy::RecordRole::Artifact
        );
        assert_eq!(back.turn_id.as_deref(), Some("9007199254740993"));
    }

    #[test]
    fn history_row_parent_relations_default_to_empty_for_sparse_payloads() {
        // Older services / fixture rows omit `parent_relations` entirely; it
        // must deserialize to an empty list (never `null` or an error), so the
        // viewer can iterate it unconditionally.
        let sparse: HistoryRow = serde_json::from_value(serde_json::json!({
            "op_id": null,
            "git_oid": null,
            "repository": null,
            "summary": "row",
            "timestamp_ms": 0,
            "group": "session:1",
            "node_key": "1:0:1",
            "parents": ["1:0:0"],
            "is_submodule": false,
        }))
        .expect("sparse HistoryRow without parent_relations");
        assert!(sparse.parent_relations.is_empty());
        // Newer provider-neutral fields default safely on sparse payloads.
        assert_eq!(
            sparse.record_role,
            editchain_core::taxonomy::RecordRole::Unknown
        );
        assert_eq!(
            sparse.activity_kind,
            editchain_core::taxonomy::ActivityKind::Unknown
        );
        assert_eq!(
            sparse.visibility,
            editchain_core::taxonomy::Visibility::Unknown
        );
        assert_eq!(sparse.outcome, editchain_core::taxonomy::Outcome::Unknown);
        assert_eq!(
            sparse.chain_state,
            editchain_core::taxonomy::ChainState::Active
        );
        assert!(sparse.muted_above.is_empty());
        assert!(sparse.muted_below.is_empty());
        assert!(sparse.muted_transitions.is_empty());
        assert!(sparse.turn_id.is_none());
        // Unknown relationship kinds deserialize to the forward-compatible
        // Unknown variant (and re-serialize as a string), so a newer service
        // never breaks an older viewer.
        let unknown: ParentRelationDto = serde_json::from_value(serde_json::json!({
            "parent": "1:0:1",
            "kind": "supercedes",
        }))
        .expect("unknown kind tolerated");
        assert_eq!(unknown.kind, ParentRelationKind::Unknown);
        let reserialized = serde_json::to_string(&unknown).expect("serialize unknown kind");
        assert!(reserialized.contains("\"unknown\""), "got {reserialized}");
    }

    #[test]
    fn produced_commit_relation_has_a_stable_protocol_name() {
        let relation = ParentRelationDto {
            parent: "1:0:2".to_string(),
            kind: ParentRelationKind::ProducedCommit,
        };
        let json = serde_json::to_value(&relation).expect("serialize produced-commit relation");
        assert_eq!(json["kind"], "produced_commit");
        let round_trip: ParentRelationDto =
            serde_json::from_value(json).expect("deserialize produced-commit relation");
        assert_eq!(round_trip, relation);
    }

    #[test]
    fn node_details_identifiers_serialize_as_exact_strings() {
        let details = NodeDetails {
            op_id: Some(big_op_id().to_string()),
            git_oid: Some(big_oid().to_hex()),
            repository: Some(OVER_2_53.to_string()),
            summary: "details".to_string(),
            body: String::new(),
            parents: vec![big_op_id().to_string()],
            git_parents: vec![big_oid().to_hex()],
            refs: Vec::new(),
            changed_paths: Vec::new(),
        };
        let json = serde_json::to_value(&details).expect("serialize");
        assert_eq!(json["git_oid"], big_oid_hex());
        assert_eq!(json["repository"], "9007199254740993");
        assert_eq!(json["parents"][0], "9007199254740993:7:42");
        assert_eq!(json["git_parents"][0], big_oid_hex());
    }

    #[test]
    fn resolve_object_request_round_trips_exact_strings() {
        let req = ResolveObjectRequest {
            snapshot_id: SnapshotId::new("fixture"),
            repository: OVER_2_53.to_string(),
            oid: big_oid().to_hex(),
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["repository"], "9007199254740993");
        assert_eq!(json["oid"], big_oid_hex());
        let back: ResolveObjectRequest = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.repository, OVER_2_53.to_string());
        assert_eq!(back.oid, big_oid_hex());
    }

    #[test]
    fn resolved_object_serializes_identities_as_exact_strings() {
        let signature = GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 1_700_000_000,
        };
        let resolved = ResolvedObject {
            repository: OVER_2_53.to_string(),
            object_format: GitObjectFormat::Sha1,
            oid: big_oid_hex(),
            imported_record: Some(big_op_id().to_string()),
            availability: GitAvailability::Resolved,
            tree: big_oid_hex(),
            parents: vec![big_oid_hex()],
            author: signature.clone(),
            committer: signature,
            authored_at: 1_700_000_000,
            committed_at: 1_700_000_001,
            message: Payload::Inline(b"initial commit".to_vec()),
            imported_refs: vec![Payload::Inline(b"refs/heads/main".to_vec())],
            live_refs: vec![Payload::Inline(b"refs/heads/main".to_vec())],
            changed_paths: vec![OVER_2_53.to_string()],
        };
        let json = serde_json::to_value(&resolved).expect("serialize");
        assert_eq!(json["repository"], "9007199254740993");
        assert_eq!(json["oid"], big_oid_hex());
        assert_eq!(json["tree"], big_oid_hex());
        assert_eq!(json["parents"][0], big_oid_hex());
        assert_eq!(json["imported_record"], "9007199254740993:7:42");
        assert_eq!(json["changed_paths"][0], "9007199254740993");
        assert_eq!(json["object_format"], "Sha1");
        assert_eq!(json["availability"], "Resolved");
        assert_eq!(json["authored_at"], 1_700_000_000i64);
        // Every identity must be an exact string, never a number or a raw
        // structural ID object (the pre-DTO wire form leaked repository u64,
        // GitOid bytes arrays, and OpId node/boot/seq numbers).
        for key in ["repository", "oid", "tree", "imported_record"] {
            assert!(
                json[key].is_string(),
                "{key} must serialize as a string: {json}"
            );
        }
        assert!(json["parents"][0].is_string());
        assert!(json["changed_paths"][0].is_string());
        assert!(!json["repository"].is_number());
        assert!(json["oid"]["bytes"].is_null(), "oid must not leak bytes");
        assert!(
            json["imported_record"]["node"].is_null(),
            "imported_record must not leak node"
        );
        // Exact round-trip through deserialization.
        let back: ResolvedObject = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.repository, OVER_2_53.to_string());
        assert_eq!(
            back.imported_record.as_deref(),
            Some("9007199254740993:7:42")
        );
        assert_eq!(back.changed_paths, vec![OVER_2_53.to_string()]);
        assert_eq!(back.parents, vec![big_oid_hex()]);
        assert_eq!(back.tree, big_oid_hex());
        assert_eq!(back.author.when, 1_700_000_000);
    }

    #[test]
    fn find_in_history_wire_shape_is_minimal_and_exact() {
        let request = RequestBody::FindInHistory(FindInHistoryRequest {
            snapshot_id: SnapshotId::new("fixture"),
            query: "needle".to_string(),
            top_k: 25,
        });
        let request_json = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(request_json["FindInHistory"]["query"], "needle");
        assert_eq!(request_json["FindInHistory"]["top_k"], 25usize);

        let response = FindInHistoryResponse {
            snapshot_id: SnapshotId::new("fixture"),
            matches: vec![FindInHistoryMatch {
                node_key: big_op_id().to_string(),
                row: 900_719_925_474_099,
            }],
            more: false,
        };
        let json = serde_json::to_value(&response).expect("serialize response");
        assert_eq!(json["matches"][0]["node_key"], big_op_id().to_string());
        assert_eq!(json["matches"][0]["row"], 900_719_925_474_099u64);
        assert_eq!(json["more"], false);
        assert!(json.get("returned").is_none());
        let back: FindInHistoryResponse = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.matches[0].node_key, big_op_id().to_string());
        assert_eq!(back.matches[0].row, 900_719_925_474_099);
    }

    #[test]
    fn file_change_and_diff_request_round_trip_exact_identities() {
        let change = FileChangeDto {
            source: FileChangeSource::Git,
            path: "src/new.rs".to_string(),
            old_path: Some("src/old.rs".to_string()),
            status: FileChangeStatus::Renamed,
            binary: false,
            partial: false,
            op_id: None,
            repository: Some(OVER_2_53.to_string()),
            repository_path: Some("src/new.rs".to_string()),
            commit_oid: Some(big_oid_hex()),
            old_oid: Some(format!("beef{}", "0".repeat(36))),
            new_oid: Some(big_oid_hex()),
            old_mode: Some("blob".to_string()),
            new_mode: Some("blob".to_string()),
        };
        let body = RequestBody::GetFileDiff(GetFileDiffRequest {
            snapshot_id: SnapshotId::new("fixture"),
            change: change.clone(),
        });
        let json = serde_json::to_value(&body).expect("serialize file diff request");
        assert_eq!(
            json["GetFileDiff"]["change"]["repository"],
            OVER_2_53.to_string()
        );
        assert_eq!(json["GetFileDiff"]["change"]["status"], "renamed");
        assert_eq!(json["GetFileDiff"]["change"]["source"], "git");
        let back: RequestBody =
            serde_json::from_value(json).expect("deserialize file diff request");
        assert!(matches!(
            back,
            RequestBody::GetFileDiff(GetFileDiffRequest { change: parsed, .. }) if parsed == change
        ));

        let diff = FileDiffDto {
            path: change.path,
            old_path: change.old_path,
            status: change.status,
            binary: false,
            partial: true,
            before: "old\n".to_string(),
            after: "new\n".to_string(),
            hunks: vec![FileDiffHunkDto {
                header: "@@ -1 +1 @@".to_string(),
                before: "old\n".to_string(),
                after: "new\n".to_string(),
            }],
            note: None,
        };
        let diff_json = serde_json::to_value(&diff).expect("serialize materialized diff");
        assert_eq!(diff_json["before"], "old\n");
        assert_eq!(diff_json["after"], "new\n");
        assert_eq!(diff_json["hunks"][0]["header"], "@@ -1 +1 @@");
        assert_eq!(diff_json["hunks"][0]["before"], "old\n");
        assert_eq!(diff_json["hunks"][0]["after"], "new\n");

        let legacy: FileDiffDto = serde_json::from_value(serde_json::json!({
            "path": "src/lib.rs",
            "status": "modified",
            "before": "old",
            "after": "new"
        }))
        .expect("deserialize diff without structured hunks");
        assert!(legacy.hunks.is_empty());
    }

    #[test]
    fn history_row_taxonomy_unknowns_round_trip_and_unknown_strings_fall_back() {
        // Unknown taxonomy strings from a newer service deserialize to the
        // forward-compatible Unknown variants and re-serialize as "unknown".
        let row: HistoryRow = serde_json::from_value(serde_json::json!({
            "op_id": null,
            "git_oid": null,
            "repository": null,
            "summary": "row",
            "timestamp_ms": 0,
            "group": "session:1",
            "node_key": "1:0:1",
            "parents": [],
            "is_submodule": false,
            "record_role": "curated_note",
            "activity_kind": "gardening",
            "visibility": "spotlight",
            "outcome": "heroic",
            "chain_state": "retired",
            "turn_id": "9007199254740993",
        }))
        .expect("unknown taxonomy tolerated");
        assert_eq!(
            row.record_role,
            editchain_core::taxonomy::RecordRole::Unknown
        );
        assert_eq!(
            row.activity_kind,
            editchain_core::taxonomy::ActivityKind::Unknown
        );
        assert_eq!(
            row.visibility,
            editchain_core::taxonomy::Visibility::Unknown
        );
        assert_eq!(row.outcome, editchain_core::taxonomy::Outcome::Unknown);
        assert_eq!(
            row.chain_state,
            editchain_core::taxonomy::ChainState::Active
        );
        assert_eq!(row.turn_id.as_deref(), Some("9007199254740993"));
        let reserialized = serde_json::to_string(&row).expect("serialize row");
        assert!(reserialized.contains("\"record_role\":\"unknown\""));
        assert!(reserialized.contains("\"activity_kind\":\"unknown\""));
        assert!(reserialized.contains("\"visibility\":\"unknown\""));
        assert!(reserialized.contains("\"outcome\":\"unknown\""));
    }

    #[test]
    fn history_row_work_unit_and_promotion_default_and_round_trip() {
        // Older services omit the new fields entirely: each must serde-default
        // (work_unit -> None, promoted -> false, activity_bundle -> None) so
        // old payloads keep loading.
        let legacy: HistoryRow = serde_json::from_value(serde_json::json!({
            "op_id": null,
            "git_oid": null,
            "repository": null,
            "summary": "row",
            "timestamp_ms": 1,
            "group": "session:1",
            "node_key": "1:0:1",
            "parents": [],
            "is_submodule": false,
        }))
        .expect("legacy row deserializes");
        assert!(!legacy.group_end);
        assert_eq!(legacy.work_unit, None);
        assert_eq!(legacy.session_meta, None);
        assert_eq!(legacy.session_summary, None);
        assert!(!legacy.promoted);
        assert_eq!(legacy.activity_bundle, None);
        assert_eq!(legacy.file_change, None);

        // Newer services emit the fields; partial WorkUnitDto members default.
        let row = HistoryRow {
            op_id: Some("1:0:1".to_string()),
            git_oid: None,
            repository: None,
            summary: "row".to_string(),
            timestamp_ms: 1,
            group: "session:1".to_string(),
            group_end: true,
            node_key: "1:0:1".to_string(),
            parents: Vec::new(),
            parent_relations: Vec::new(),
            is_submodule: false,
            is_system: false,
            author: String::new(),
            commit_id: String::new(),
            kind: String::new(),
            lane: 0,
            above: Vec::new(),
            below: Vec::new(),
            transitions: Vec::new(),
            muted_above: Vec::new(),
            muted_below: Vec::new(),
            muted_transitions: Vec::new(),
            sub_ops: Vec::new(),
            is_subop: false,
            hierarchy_depth: 0,
            parent_row: None,
            subop_kind: None,
            record_role: editchain_core::taxonomy::RecordRole::Action,
            activity_kind: editchain_core::taxonomy::ActivityKind::Execute,
            visibility: editchain_core::taxonomy::Visibility::Primary,
            outcome: editchain_core::taxonomy::Outcome::Success,
            chain_state: editchain_core::taxonomy::ChainState::Muted,
            turn_id: Some(OVER_2_53.to_string()),
            session_meta: Some(SessionMetaDto {
                session_title: Some("r8".to_string()),
                model_provider: Some("sglang_dsv4".to_string()),
                agent_nickname: Some("Harvey".to_string()),
            }),
            session_summary: Some(SessionSummaryDto { count: 87 }),
            work_unit: Some(WorkUnitDto {
                id: format!("session:1/turn:{OVER_2_53}"),
                is_start: true,
                is_end: false,
                title: Some("request".to_string()),
                count: 12,
            }),
            promoted: true,
            activity_bundle: Some(ActivityBundleDto {
                kind: ActivityBundleKind::ExecuteRun,
                member_count: 3,
            }),
            file_change: None,
        };
        let json = serde_json::to_value(&row).expect("serialize row");
        assert_eq!(
            json["work_unit"]["id"],
            format!("session:1/turn:{OVER_2_53}")
        );
        assert_eq!(json["work_unit"]["is_start"], true);
        assert_eq!(json["work_unit"]["title"], "request");
        assert_eq!(json["work_unit"]["count"], 12u64);
        assert_eq!(json["group_end"], true);
        assert_eq!(json["promoted"], true);
        assert_eq!(json["session_meta"]["model_provider"], "sglang_dsv4");
        assert_eq!(json["session_meta"]["agent_nickname"], "Harvey");
        assert_eq!(json["session_meta"]["session_title"], "r8");
        assert_eq!(json["session_summary"]["count"], 87u64);
        assert_eq!(json["activity_bundle"]["kind"], "execute-run");
        assert_eq!(json["activity_bundle"]["member_count"], 3u64);
        assert_eq!(json["chain_state"], "muted");
        let back: HistoryRow = serde_json::from_value(json).expect("deserialize row");
        assert_eq!(
            back.work_unit.as_ref().map(|w| w.id.as_str()),
            Some("session:1/turn:9007199254740993")
        );
        assert!(back
            .work_unit
            .as_ref()
            .is_some_and(|w| w.is_start && !w.is_end));
        assert!(back.promoted);
        assert_eq!(back.session_summary, Some(SessionSummaryDto { count: 87 }));
        assert_eq!(
            back.chain_state,
            editchain_core::taxonomy::ChainState::Muted
        );
        assert_eq!(
            back.session_meta
                .as_ref()
                .and_then(|meta| meta.model_provider.as_deref()),
            Some("sglang_dsv4")
        );
        assert_eq!(
            back.activity_bundle.as_ref().map(|bundle| bundle.kind),
            Some(ActivityBundleKind::ExecuteRun)
        );
        assert_eq!(
            back.activity_bundle
                .as_ref()
                .map(|bundle| bundle.member_count),
            Some(3u64)
        );

        let plan_repeat = ActivityBundleDto {
            kind: ActivityBundleKind::PlanRepeat,
            member_count: 3,
        };
        let plan_json = serde_json::to_value(&plan_repeat).expect("serialize Plan repeat");
        assert_eq!(plan_json["kind"], "plan-repeat");
        let plan_back: ActivityBundleDto =
            serde_json::from_value(plan_json).expect("deserialize Plan repeat");
        assert_eq!(plan_back.kind, ActivityBundleKind::PlanRepeat);
        assert_eq!(plan_back.member_count, 3);

        let work_group = ActivityBundleDto {
            kind: ActivityBundleKind::WorkGroup,
            member_count: 5,
        };
        let work_json = serde_json::to_value(&work_group).expect("serialize work group");
        assert_eq!(work_json["kind"], "work-group");
        let work_back: ActivityBundleDto =
            serde_json::from_value(work_json).expect("deserialize work group");
        assert_eq!(work_back.kind, ActivityBundleKind::WorkGroup);
        assert_eq!(work_back.member_count, 5);

        // A missing `activity_bundle` member defaults to None, keeping older
        // payloads additive-compatible with the new field.
        let without_bundle: HistoryRow = serde_json::from_value(serde_json::json!({
            "op_id": null,
            "git_oid": null,
            "repository": null,
            "summary": "row",
            "timestamp_ms": 1,
            "group": "session:1",
            "node_key": "1:0:1",
            "parents": [],
            "is_submodule": false,
            "work_unit": { "id": "ops" }
        }))
        .expect("row without activity_bundle deserializes");
        assert_eq!(without_bundle.activity_bundle, None);
        assert_eq!(without_bundle.hierarchy_depth, 0);

        // Unknown bundle kinds from a newer service deserialize to the
        // forward-compatible Unknown variant (and re-serialize as a string),
        // so a newer service never breaks an older viewer.
        let unknown: ActivityBundleDto = serde_json::from_value(serde_json::json!({
            "kind": "super-run",
            "member_count": 2,
        }))
        .expect("unknown bundle kind tolerated");
        assert_eq!(unknown.kind, ActivityBundleKind::Unknown);
        assert_eq!(unknown.member_count, 2);
        let reserialized = serde_json::to_string(&unknown).expect("serialize unknown kind");
        assert!(reserialized.contains("\"unknown\""), "got {reserialized}");

        // A partial WorkUnitDto (only the required id) fills the rest with
        // defaults, keeping the wire additive for older viewers.
        let sparse: HistoryRow = serde_json::from_value(serde_json::json!({
            "op_id": null,
            "git_oid": null,
            "repository": null,
            "summary": "row",
            "timestamp_ms": 1,
            "group": "ops",
            "node_key": "1:0:1",
            "parents": [],
            "is_submodule": false,
            "work_unit": { "id": "ops" }
        }))
        .expect("sparse work unit deserializes");
        let unit = sparse.work_unit.expect("work unit present");
        assert_eq!(unit.id, "ops");
        assert!(!unit.is_start);
        assert!(!unit.is_end);
        assert_eq!(unit.title, None);
        assert_eq!(unit.count, 0);
    }

    #[test]
    fn get_window_layout_flag_round_trips() {
        let provisional: GetWindowRequest = serde_json::from_value(serde_json::json!({
            "offset": 0,
            "limit": 500,
            "include_layout": false
        }))
        .expect("deserialize provisional request");
        assert!(!provisional.include_layout);
    }
}
