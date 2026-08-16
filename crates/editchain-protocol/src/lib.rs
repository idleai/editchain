//! Versioned request/response DTOs for the `EditChain` VS Code service.
//!
//! These types are serialized over stdio between the thin TypeScript
//! extension host and the native Rust service. Every response carries
//! generation counters so stale windows and results can be detected.

use serde::{Deserialize, Serialize};

use editchain_core::{GitOid, OpId, RepositoryId};
use editchain_query::search::{SearchFilters, SearchMode};

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
    /// Hide nodes with no real timestamp (`timestamp_ms() == 0`).
    #[serde(default)]
    pub hide_undated: bool,
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
    pub filters: SearchFilters,
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
    pub filters: SearchFilters,
}

/// Resolve a git object by OID in a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveObjectRequest {
    /// Repository identity.
    pub repository: RepositoryId,
    /// Object OID to resolve.
    pub oid: GitOid,
}

/// A history row in the unified projection (`EditChain` op or `Git` commit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRow {
    /// The operation ID (for `EditChain` ops) in display form `"node:boot:seq"`,
    /// or `None` for git commits. Stored as a string to avoid JS precision loss.
    pub op_id: Option<String>,
    /// The git commit OID (for git commits).
    pub git_oid: Option<GitOid>,
    /// The repository (for git commits).
    pub repository: Option<RepositoryId>,
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
}

/// Details for a single history node (for the inspector).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDetails {
    /// The operation ID (display form `"node:boot:seq"`), if this is an
    /// `EditChain` op. Stored as a string to avoid JS precision loss.
    pub op_id: Option<String>,
    /// The git commit OID, if this is a git commit.
    pub git_oid: Option<GitOid>,
    /// The repository, if this is a git commit.
    pub repository: Option<RepositoryId>,
    /// Display summary.
    pub summary: String,
    /// Full payload text (message/content), if available.
    pub body: String,
    /// Parent operation IDs (for `EditChain` ops).
    pub parents: Vec<OpId>,
    /// Parent commit OIDs (for git commits).
    pub git_parents: Vec<GitOid>,
    /// Refs pointing at this commit (for git commits).
    pub refs: Vec<String>,
    /// Changed paths (for git commits).
    pub changed_paths: Vec<String>,
}

/// Information about a discovered git repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryInfo {
    /// Repository identity.
    pub id: RepositoryId,
    /// Path to the repository root.
    pub path: String,
    /// Whether this is a linked worktree.
    pub is_worktree: bool,
    /// Whether this is a nested/submodule repository (not the workspace root).
    pub is_submodule: bool,
}
