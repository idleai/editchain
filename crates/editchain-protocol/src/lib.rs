//! Versioned request/response DTOs for the `EditChain` VS Code service.
//!
//! These types are serialized over stdio between the thin TypeScript
//! extension host and the native Rust service. Every response carries
//! generation counters so stale windows and results can be detected.

use serde::{Deserialize, Serialize};

// Crate-level dependency marker (used by Cargo for feature resolution; the
// types are exercised by the protocol round-trip tests).
use editchain_core as _;
use editchain_core::{GitAvailability, GitObjectFormat, GitSignature, Payload};
use editchain_query::search::{SearchMode, Source, TagFilter};

/// Protocol version for the framed stdio channel.
pub const PROTOCOL_VERSION: u32 = 1;

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
    /// Get a window of history rows.
    GetWindow(GetWindowRequest),
    /// Get the full graph layout (lanes + edge paths) for rendering.
    GetLayout(GetLayoutRequest),
    /// Get details for a specific node.
    GetNodeDetails(GetNodeDetailsRequest),
    /// Set search filters.
    SetFilters(SetFiltersRequest),
    /// Run a unified search.
    Search(SearchRequest),
    /// List discovered git repositories.
    GetRepositories,
    /// Resolve a git object by OID.
    ResolveObject(ResolveObjectRequest),
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
    /// An error message.
    Error(String),
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
/// Older clients ignore both keys; they are never required to open a chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRequest {
    /// Path to the workspace root.
    pub workspace_path: String,
    /// Path to the chain directory (may be empty if none).
    pub chain_dir: String,
}

/// Get a window of history rows (cursor-based paging).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetWindowRequest {
    /// Cursor offset into the history (0 = newest).
    pub offset: u64,
    /// Number of rows to return.
    pub limit: u64,
    /// Skip rows belonging to nested/submodule repositories.
    #[serde(default)]
    pub hide_submodules: bool,
    /// Optional chain filter to apply before windowing.
    #[serde(default)]
    pub filter: Option<ChainFilterDto>,
    /// Include globally stable lane/edge geometry in this response.
    ///
    /// The production viewer sends `false` for its first page so content rows
    /// can paint before O(V) layout, then repeats that window with `true`.
    /// Omitted by older clients/harnesses means `true` for compatibility.
    #[serde(default = "default_true")]
    pub include_layout: bool,
}

/// Serde default for backward-compatible opt-out flags.
const fn default_true() -> bool {
    true
}

/// Get the graph layout for a bounded window of rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetLayoutRequest {
    /// Skip rows belonging to nested/submodule repositories.
    #[serde(default)]
    pub hide_submodules: bool,
    /// Cursor offset into the history (0 = newest).
    #[serde(default)]
    pub offset: u64,
    /// Number of rows to emit edges for.
    #[serde(default)]
    pub limit: u64,
    /// Optional chain filter to apply before computing the layout.
    #[serde(default)]
    pub filter: Option<ChainFilterDto>,
}

/// A chain filter carried over the protocol.
///
/// Mirrors [`editchain_project::filter::ChainFilter`] as a plain serializable
/// DTO so the webview can request keyword/regex/undated filtering without
/// depending on the Rust projection crate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainFilterDto {
    /// Regex/literal pattern matched against each node's display summary.
    #[serde(default)]
    pub summary_pattern: String,
    /// Regex/literal pattern matched against each node's kind tag.
    #[serde(default)]
    pub kind_pattern: String,
    /// Inclusive kind constraint: when non-empty, ONLY nodes whose kind tag
    /// matches this regex/literal pattern are kept. Non-matching kinds are
    /// excluded without ordinary endpoint preservation; structural relationship
    /// anchors/targets remain so branch and reconnect edges stay visible. This
    /// lets the webview express "Show messages only" server-side without sparse
    /// client offsets or unsupported regex lookahead. Empty means no inclusion
    /// constraint.
    #[serde(default)]
    pub include_kind_pattern: String,
    /// Hide nodes with no real timestamp (`timestamp_ms() == 0`).
    #[serde(default)]
    pub hide_undated: bool,
    /// Hide trace rows (duplicate/echo/transport envelopes classified as
    /// `Visibility::Trace`) unconditionally, splicing causal edges across them.
    ///
    /// Backward compatible: older clients omit the field and deserialize it as
    /// `false` (the raw view), while the fixed pregenerated/default viewer
    /// sends `true` so Activity mode can be served from the render snapshot.
    #[serde(default)]
    pub hide_trace: bool,
    /// Reconnect causal edges across hidden intermediate nodes.
    #[serde(default)]
    pub splice: bool,
}

/// A grid point in the graph: a row index and a lane index.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LayoutPoint {
    /// Row index (0 = newest).
    pub row: usize,
    /// Lane index.
    pub lane: usize,
}

/// A single edge in the graph, from a child node down to one of its parents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutEdge {
    /// The child node key (the newer end of the edge).
    pub child: String,
    /// The parent node key (the older end of the edge).
    pub parent: String,
    /// Ordered grid points from child to parent.
    pub points: Vec<LayoutPoint>,
}

/// A single graph row in the layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutRow {
    /// The node key this row represents.
    pub node: String,
    /// The lane this node occupies.
    pub lane: usize,
}

/// The full graph layout for rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphLayout {
    /// Per-row assignment (row index → node key + lane).
    pub rows: Vec<LayoutRow>,
    /// All edges (child → parent), each with its ordered point path.
    pub edges: Vec<LayoutEdge>,
    /// The maximum lane index across ALL rows (not just this window). The
    /// webview uses this to size the graph column stably regardless of which
    /// window is loaded, so lanes don't jump as the user scrolls.
    #[serde(default)]
    pub max_lane: usize,
}

/// Get details for a specific node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetNodeDetailsRequest {
    /// The operation ID to inspect, in display form `"node:boot:seq"`.
    ///
    /// Stored as a string so it round-trips through JavaScript without precision
    /// loss on u64 node values that exceed 2^53.
    pub op_id: String,
}

/// Set search filters for subsequent queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetFiltersRequest {
    /// The filters to apply.
    pub filters: SearchFiltersDto,
}

/// Run a unified search across `EditChain` and `Git` history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    /// The query string.
    pub query: String,
    /// Search mode (lexical, vector, hybrid).
    pub mode: SearchMode,
    /// Number of results to return.
    pub top_k: usize,
    /// Optional filters.
    pub filters: SearchFiltersDto,
}

/// Search filters carried over the protocol.
///
/// Mirrors [`editchain_query::search::SearchFilters`] as a JSON-safe DTO.
/// Session and actor identifiers are exact decimal strings so u64 values above
/// 2^53 round-trip through JavaScript without precision loss; the service
/// parses and validates them, returning an `Error` response on invalid IDs.
/// Timestamps and counts stay numeric where safe.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchFiltersDto {
    /// Only include these operation kinds.
    pub kinds: Option<Vec<TagFilter>>,
    /// Only include these source domains (`EditChain` and/or `Git`).
    pub sources: Option<Vec<Source>>,
    /// Only include these sessions (exact decimal `SessionId` strings).
    pub sessions: Option<Vec<String>>,
    /// Only include these actors (exact decimal `ActorId` strings).
    pub actors: Option<Vec<String>>,
    /// Glob patterns for file paths.
    pub paths: Option<Vec<String>>,
    /// Earliest timestamp (Unix ms).
    pub after: Option<u64>,
    /// Latest timestamp (Unix ms).
    pub before: Option<u64>,
    /// Include raw import records in results.
    pub include_raw: bool,
    /// Include private/thinking content.
    pub include_private: bool,
}

/// Resolve a git object by OID in a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveObjectRequest {
    /// Repository identity as an exact decimal `RepositoryId` string (u64
    /// values above 2^53 must not be rounded by JavaScript).
    pub repository: String,
    /// Object OID to resolve, as lowercase hex (40 chars SHA-1 / 64 SHA-256).
    pub oid: String,
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
    reason = "flat versioned wire DTO: each boolean is an independent backward-compatible serde-defaulted flag the viewer toggles (submodule/system/subop/promoted); refactoring to enums would churn the wire contract"
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
    /// Stable node key for graph wiring (op id string or git oid hex).
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
    /// Bundled metadata sub-ops attached to this row (revealed on click).
    #[serde(default)]
    pub sub_ops: Vec<SubOpSummary>,
    /// Whether this row is a bundled sub-op expanded inline under its parent
    /// (rather than a top-level node). Sub-op rows carry no graph dot of their
    /// own; they inherit the parent's lane for a continuation line.
    #[serde(default)]
    pub is_subop: bool,
    /// Absolute row index of the parent's collapsed row, for sub-op rows.
    /// `None` on top-level rows.
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
    pub record_role: editchain_project::taxonomy::RecordRole,
    /// Provider-neutral activity kind (conversation/plan/execute/change/...).
    /// Serialized as a lowercase `snake_case` string; unknown values deserialize
    /// to `Unknown` for forward compatibility.
    #[serde(default)]
    pub activity_kind: editchain_project::taxonomy::ActivityKind,
    /// Render prominence (primary/supporting/trace). Trace rows are hidden by
    /// the `hide_trace` chain filter.
    #[serde(default)]
    pub visibility: editchain_project::taxonomy::Visibility,
    /// Concluded outcome (success/warning/failure/cancelled/unknown). Unknown
    /// is the default — success is never inferred without structured evidence.
    #[serde(default)]
    pub outcome: editchain_project::taxonomy::Outcome,
    /// Provider-neutral turn identity as an exact decimal string (u64 values
    /// above 2^53 round-trip through JavaScript without precision loss).
    /// `None` when the row is not turn-scoped.
    #[serde(default)]
    pub turn_id: Option<String>,
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
    /// ordinary row, expanded sub-op row, and raw (unbundled) row. Older
    /// services that predate the field omit it entirely, so it defaults to
    /// `None`.
    #[serde(default)]
    pub activity_bundle: Option<ActivityBundleDto>,
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
/// Serialized as kebab-case strings (`"execute-run"`). Unknown strings
/// deserialize to [`Self::Unknown`] so older clients tolerate new bundle kinds
/// from newer services (forward compatibility).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityBundleKind {
    /// A synthetic Activity-view summary node folding a maximal contiguous
    /// run of low-signal execute rows into one expandable run.
    ExecuteRun,
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
/// Serialized as lowercase strings (`"subagent"`, `"reconnect"`, `"fork"`).
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
    pub summary: String,
    /// Short type tag (e.g. "last-prompt", "mode", "permission-mode").
    pub kind: String,
    /// Timestamp in Unix ms (0 if unknown).
    pub timestamp_ms: u64,
}

/// A window of history rows with generation counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryWindow {
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
    /// Global per-top-level-node bundled sub-op counts for this filter state.
    /// Present on the offset-zero window that establishes a snapshot and omitted
    /// from subsequent pages so response size remains proportional to `limit`.
    /// The client retains these prefix sums for visible/absolute index mapping.
    #[serde(default)]
    pub sub_op_counts: Option<Vec<usize>>,
    /// Whether lane/connector fields contain the globally computed layout.
    /// `false` denotes a row-complete provisional first paint.
    #[serde(default)]
    pub layout_ready: bool,
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

/// A scored search result over the protocol.
///
/// Every identifier (`op_id`, `chunk_id`, `session_id`, `actor_id`) is an exact
/// string so u64 components above 2^53 round-trip through JavaScript without
/// precision loss. Git hits additionally carry the real commit identity
/// (`git_oid` lowercase hex, `repository` exact decimal, `kind` `"git"`,
/// `is_submodule`) so the renderer navigates by `ResolveObject` instead of the
/// synthetic index-only `op_id`. Scores, timestamps, and counts stay numeric
/// where safe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// The operation ID this chunk belongs to (`"node:boot:seq"`).
    ///
    /// For `Git` hits this is a synthetic id used only inside the search index
    /// (`0:0:generation`); it is NOT a projection node and must never be used
    /// for `GetNodeDetails` navigation.
    pub op_id: String,
    /// The chunk identifier (`"node:boot:seq:ordinal"`).
    pub chunk_id: String,
    /// Fused relevance score (higher = more relevant).
    pub score: f64,
    /// The text content of this chunk.
    pub text: String,
    /// The source domain (`EditChain` or `Git`).
    pub source: Source,
    /// The session this chunk belongs to, if any (exact decimal `SessionId`
    /// string; `None` for git commits, which have no session scope).
    pub session_id: Option<String>,
    /// The actor that produced this chunk (exact decimal `ActorId` string).
    pub actor_id: String,
    /// Bitmask of operation kind tags (numeric count of tag bits).
    pub kind_tags: u64,
    /// Timestamp in milliseconds since Unix epoch.
    pub timestamp_ms: u64,
    /// Generation counter for read-your-writes consistency.
    pub generation: u64,
    /// The git commit OID (for `Git` hits) as lowercase hex — `None` for
    /// `EditChain` hits. A string so it round-trips exactly through JavaScript.
    #[serde(default)]
    pub git_oid: Option<String>,
    /// The repository (for `Git` hits) as an exact decimal `RepositoryId`
    /// string — `None` for `EditChain` hits.
    #[serde(default)]
    pub repository: Option<String>,
    /// Discriminated identity tag: `"git"` for real git commits, or the
    /// `EditChain` op kind (`"message"`, `"tool"`, ...) when known.
    #[serde(default)]
    pub kind: String,
    /// Whether a `Git` hit belongs to a nested/submodule repository.
    #[serde(default)]
    pub is_submodule: bool,
}

/// A search response over the protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    /// The scored search result chunks.
    pub results: Vec<SearchHit>,
}

/// Information about a discovered git repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryInfo {
    /// Repository identity as an exact decimal `RepositoryId` string (u64
    /// values above 2^53 must not be rounded by JavaScript).
    pub id: String,
    /// Path to the repository root.
    pub path: String,
    /// Whether this is a linked worktree.
    pub is_worktree: bool,
    /// Whether this is a nested/submodule repository (not the workspace root).
    pub is_submodule: bool,
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
            sub_ops: Vec::new(),
            is_subop: false,
            parent_row: None,
            subop_kind: None,
            record_role: editchain_project::taxonomy::RecordRole::Artifact,
            activity_kind: editchain_project::taxonomy::ActivityKind::SourceControl,
            visibility: editchain_project::taxonomy::Visibility::Primary,
            outcome: editchain_project::taxonomy::Outcome::Success,
            turn_id: Some(OVER_2_53.to_string()),
            work_unit: None,
            promoted: false,
            activity_bundle: None,
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
        assert_eq!(round_trip["turn_id"], "9007199254740993");
        assert_eq!(
            back.record_role,
            editchain_project::taxonomy::RecordRole::Artifact
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
            editchain_project::taxonomy::RecordRole::Unknown
        );
        assert_eq!(
            sparse.activity_kind,
            editchain_project::taxonomy::ActivityKind::Unknown
        );
        assert_eq!(
            sparse.visibility,
            editchain_project::taxonomy::Visibility::Unknown
        );
        assert_eq!(
            sparse.outcome,
            editchain_project::taxonomy::Outcome::Unknown
        );
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
    fn repository_info_id_serializes_as_exact_string() {
        let info = RepositoryInfo {
            id: OVER_2_53.to_string(),
            path: "/tmp/repo".to_string(),
            is_worktree: false,
            is_submodule: true,
        };
        let json = serde_json::to_value(&info).expect("serialize");
        assert_eq!(json["id"], "9007199254740993");
        assert!(
            !json["id"].is_number(),
            "id must never serialize as a number"
        );
    }

    #[test]
    fn resolve_object_request_round_trips_exact_strings() {
        let req = ResolveObjectRequest {
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
    fn search_response_identifiers_serialize_as_exact_strings() {
        let hit = SearchHit {
            op_id: big_op_id().to_string(),
            chunk_id: format!("{}:3", big_op_id()),
            score: 0.5,
            text: "chunk text".to_string(),
            source: Source::EditChain,
            session_id: Some(OVER_2_53.to_string()),
            actor_id: OVER_2_53.to_string(),
            kind_tags: 3,
            timestamp_ms: 1_700_000_000_000,
            generation: 12,
            git_oid: None,
            repository: None,
            kind: String::new(),
            is_submodule: false,
        };
        let response = SearchResponse { results: vec![hit] };
        let json = serde_json::to_value(&response).expect("serialize");
        let hit_json = &json["results"][0];
        assert_eq!(hit_json["op_id"], "9007199254740993:7:42");
        assert_eq!(hit_json["chunk_id"], "9007199254740993:7:42:3");
        assert_eq!(hit_json["session_id"], "9007199254740993");
        assert_eq!(hit_json["actor_id"], "9007199254740993");
        assert_eq!(hit_json["timestamp_ms"], 1_700_000_000_000u64);
        assert_eq!(hit_json["kind_tags"], 3u64);
        assert_eq!(hit_json["generation"], 12u64);
        assert!(
            hit_json["git_oid"].is_null(),
            "EditChain hit has no git_oid"
        );
        assert!(
            hit_json["repository"].is_null(),
            "EditChain hit has no repository"
        );
        // All identifiers must be JSON strings, never numbers.
        for key in ["op_id", "chunk_id", "session_id", "actor_id"] {
            assert!(
                hit_json[key].is_string(),
                "{key} must serialize as a string: {hit_json}"
            );
        }
    }

    #[test]
    fn git_search_hit_serializes_real_identity_as_exact_strings() {
        let hit = SearchHit {
            // Synthetic index-only op id: present but never navigable.
            op_id: "0:0:42".to_string(),
            chunk_id: "0:0:42:0".to_string(),
            score: 0.75,
            text: "initial commit".to_string(),
            source: Source::Git,
            session_id: None,
            actor_id: "1".to_string(),
            kind_tags: 0,
            timestamp_ms: 1_700_000_000_000,
            generation: 42,
            git_oid: Some(big_oid_hex()),
            repository: Some(OVER_2_53.to_string()),
            kind: "git".to_string(),
            is_submodule: true,
        };
        let json = serde_json::to_value(&hit).expect("serialize");
        assert_eq!(json["git_oid"], big_oid_hex());
        assert_eq!(json["repository"], "9007199254740993");
        assert_eq!(json["kind"], "git");
        assert_eq!(json["is_submodule"], true);
        assert_eq!(json["op_id"], "0:0:42");
        // Git identity must round-trip as exact strings, never numbers.
        let back: SearchHit = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.git_oid.as_deref(), Some(big_oid_hex().as_str()));
        assert_eq!(back.repository.as_deref(), Some("9007199254740993"));
        assert_eq!(back.kind, "git");
        assert!(back.is_submodule);
        // Sparse payloads (older clients) default the new fields safely.
        let sparse: SearchHit = serde_json::from_value(serde_json::json!({
            "op_id": "1:0:1",
            "chunk_id": "1:0:1:0",
            "score": 1.0,
            "text": "row",
            "source": "EditChain",
            "session_id": null,
            "actor_id": "1",
            "kind_tags": 0,
            "timestamp_ms": 0,
            "generation": 0,
        }))
        .expect("deserialize sparse hit");
        assert!(sparse.git_oid.is_none());
        assert!(sparse.repository.is_none());
        assert_eq!(sparse.kind, "");
        assert!(!sparse.is_submodule);
    }

    #[test]
    fn search_filters_dto_round_trips_string_ids_and_numeric_ranges() {
        let filters = SearchFiltersDto {
            kinds: Some(vec![TagFilter::Message, TagFilter::Command]),
            sources: Some(vec![Source::EditChain]),
            sessions: Some(vec![OVER_2_53.to_string()]),
            actors: Some(vec![OVER_2_53.to_string()]),
            paths: Some(vec!["src/**".to_string()]),
            after: Some(1),
            before: Some(1_700_000_000_000),
            include_raw: false,
            include_private: false,
        };
        let json = serde_json::to_value(&filters).expect("serialize");
        assert_eq!(json["sessions"][0], "9007199254740993");
        assert_eq!(json["actors"][0], "9007199254740993");
        assert_eq!(json["kinds"][0], "Message");
        assert_eq!(json["after"], 1u64);
        assert_eq!(json["before"], 1_700_000_000_000u64);
        let back: SearchFiltersDto = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.sessions.as_deref(), Some(&[OVER_2_53.to_string()][..]));
        assert_eq!(back.actors.as_deref(), Some(&[OVER_2_53.to_string()][..]));
    }

    #[test]
    fn chain_filter_dto_round_trips_include_kind_pattern() {
        let dto = ChainFilterDto {
            summary_pattern: String::new(),
            kind_pattern: String::new(),
            include_kind_pattern: "^(message|command)$".to_string(),
            hide_undated: true,
            hide_trace: true,
            splice: true,
        };
        let json = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(json["include_kind_pattern"], "^(message|command)$");
        assert_eq!(json["hide_trace"], true);
        let back: ChainFilterDto = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.include_kind_pattern, "^(message|command)$");
        assert!(back.hide_trace);
        // Absent field defaults to empty (no inclusion constraint); the
        // backward-compatible hide_trace default is `false` (raw view).
        let sparse: ChainFilterDto =
            serde_json::from_value(serde_json::json!({ "splice": true })).expect("deserialize");
        assert_eq!(sparse.include_kind_pattern, "");
        assert!(sparse.splice);
        assert!(!sparse.hide_trace);
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
            "turn_id": "9007199254740993",
        }))
        .expect("unknown taxonomy tolerated");
        assert_eq!(
            row.record_role,
            editchain_project::taxonomy::RecordRole::Unknown
        );
        assert_eq!(
            row.activity_kind,
            editchain_project::taxonomy::ActivityKind::Unknown
        );
        assert_eq!(
            row.visibility,
            editchain_project::taxonomy::Visibility::Unknown
        );
        assert_eq!(row.outcome, editchain_project::taxonomy::Outcome::Unknown);
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
        assert_eq!(legacy.work_unit, None);
        assert!(!legacy.promoted);
        assert_eq!(legacy.activity_bundle, None);

        // Newer services emit the fields; partial WorkUnitDto members default.
        let row = HistoryRow {
            op_id: Some("1:0:1".to_string()),
            git_oid: None,
            repository: None,
            summary: "row".to_string(),
            timestamp_ms: 1,
            group: "session:1".to_string(),
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
            sub_ops: Vec::new(),
            is_subop: false,
            parent_row: None,
            subop_kind: None,
            record_role: editchain_project::taxonomy::RecordRole::Action,
            activity_kind: editchain_project::taxonomy::ActivityKind::Execute,
            visibility: editchain_project::taxonomy::Visibility::Primary,
            outcome: editchain_project::taxonomy::Outcome::Success,
            turn_id: Some(OVER_2_53.to_string()),
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
        };
        let json = serde_json::to_value(&row).expect("serialize row");
        assert_eq!(
            json["work_unit"]["id"],
            format!("session:1/turn:{OVER_2_53}")
        );
        assert_eq!(json["work_unit"]["is_start"], true);
        assert_eq!(json["work_unit"]["title"], "request");
        assert_eq!(json["work_unit"]["count"], 12u64);
        assert_eq!(json["promoted"], true);
        assert_eq!(json["activity_bundle"]["kind"], "execute-run");
        assert_eq!(json["activity_bundle"]["member_count"], 3u64);
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
    fn get_window_layout_defaults_on_for_older_clients() {
        let legacy: GetWindowRequest = serde_json::from_value(serde_json::json!({
            "offset": 0,
            "limit": 500
        }))
        .expect("deserialize legacy request");
        assert!(legacy.include_layout);

        let provisional: GetWindowRequest = serde_json::from_value(serde_json::json!({
            "offset": 0,
            "limit": 500,
            "include_layout": false
        }))
        .expect("deserialize provisional request");
        assert!(!provisional.include_layout);
    }
}
