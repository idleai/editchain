//! Native Rust stdio service for the `EditChain` VS Code extension.
//!
//! Reads framed requests from stdin, dispatches them against the chain reader,
//! git resolver, and unified search, and writes framed responses to stdout.

#[cfg(test)]
use tempfile as _;

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_query as _;

use std::path::PathBuf;

use editchain_codec::frame::decode_op;
use editchain_core::{
    ActorId, Clock, GitOid, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
};
use editchain_git::{discover_repositories, resolve_commit, walk_history, RepositoryHandle};
use editchain_index::LexicalIndex;
use editchain_node::segment::SegmentStore;
use editchain_project::filter::ChainFilter;
use editchain_project::HistoryProjection;
use editchain_protocol::{
    ChainFilterDto, GraphLayout as ProtocolGraphLayout, HistoryRow, HistoryWindow, LayoutEdge,
    LayoutPoint, LayoutRow, NodeDetails, RepositoryInfo, Request, RequestBody, Response,
    ResponseBody,
};

/// A loaded workspace: chain ops + git repositories.
#[derive(Debug)]
pub struct Workspace {
    /// The unified history projection.
    pub projection: HistoryProjection,
    /// Discovered git repositories.
    pub repositories: Vec<editchain_git::RepositoryDiscovery>,
    /// Cached canonical node list per `(hide_submodules, filter)` state.
    ///
    /// The topological sort over the full chain is the expensive part; it is
    /// computed once per filter state and reused for every window/layout call.
    sorted_nodes: std::collections::HashMap<
        (bool, editchain_project::filter::ChainFilterKey),
        Vec<editchain_project::HistoryNode>,
    >,
    /// Cached layout context per `(hide_submodules, filter)` state.
    ///
    /// The context bundles all O(V) derived data (keys, row map, lane map, lane
    /// assignment) so per-window edge computation is O(window). Computed once
    /// per filter state and reused across scrolls/resizes.
    contexts: std::collections::HashMap<
        (bool, editchain_project::filter::ChainFilterKey),
        editchain_project::layout::LayoutContext,
    >,
}

impl Workspace {
    /// Create a workspace from an existing projection (used in tests).
    #[must_use]
    pub fn from_projection(projection: HistoryProjection) -> Self {
        Self {
            projection,
            repositories: Vec::new(),
            sorted_nodes: std::collections::HashMap::new(),
            contexts: std::collections::HashMap::new(),
        }
    }

    /// Load a workspace from a chain directory and discover git repos.
    ///
    /// # Errors
    ///
    /// Returns an error if the chain cannot be read or repos cannot be discovered.
    pub fn open(workspace_path: &str, chain_dir: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Resolve the chain directory relative to the workspace when it is a
        // relative path (e.g. ".editchain"). The service process's CWD is not
        // necessarily the workspace root, so we must join explicitly.
        let chain_path = if PathBuf::from(chain_dir).is_absolute() {
            PathBuf::from(chain_dir)
        } else {
            PathBuf::from(workspace_path).join(chain_dir)
        };
        let ops = read_chain_ops(&chain_path)?;
        let mut projection = HistoryProjection::from_ops(ops);
        let repositories = discover_repositories(&PathBuf::from(workspace_path))?;
        // Walk each discovered repo's history into the projection.
        for discovery in &repositories {
            let opened = open_repository_handle(discovery);
            let Ok(handle) = opened else {
                continue;
            };
            let walked = walk_history(&handle, 0);
            if let Ok(commits) = walked {
                projection.merge_git_commits(commits);
            }
        }
        // Stitch sessions and git history into a single edit chain.
        projection.link_history();
        Ok(Self {
            projection,
            repositories,
            sorted_nodes: std::collections::HashMap::new(),
            contexts: std::collections::HashMap::new(),
        })
    }

    /// Get a window of history rows (newest-first).
    #[must_use]
    pub fn history_window(
        &mut self,
        offset: u64,
        limit: u64,
        hide_submodules: bool,
        filter: &ChainFilter,
    ) -> HistoryWindow {
        let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        // Build the node list, optionally filtering out submodules and applying
        // the chain filter.
        let filtered = self.filtered_nodes(hide_submodules, filter);
        let total = filtered.len();

        // Build/access the cached layout context so each row can carry its graph
        // geometry (lane + active lanes + transitions) for per-row rendering.
        let key = (hide_submodules, filter.key());
        if !self.contexts.contains_key(&key) {
            let sorted = self.filtered_nodes(hide_submodules, filter);
            let ctx = self.projection.layout_context(&sorted);
            drop(self.contexts.insert(key.clone(), ctx));
        }
        let ctx = self.contexts.get(&key);

        let rows = filtered
            .into_iter()
            .enumerate()
            .skip(offset_usize)
            .take(limit_usize)
            .map(|(abs_idx, node)| {
                // Per-row graph geometry from the layout context (absolute row
                // index into the full sorted list).
                let (lane, above, below, transitions) =
                    ctx.map_or((0, Vec::new(), Vec::new(), Vec::new()), |c| {
                        (
                            c.lanes.get(abs_idx).map_or(0, |r| r.lane),
                            c.row_above.get(abs_idx).cloned().unwrap_or_default(),
                            c.row_below.get(abs_idx).cloned().unwrap_or_default(),
                            c.row_transitions.get(abs_idx).cloned().unwrap_or_default(),
                        )
                    });
                HistoryRow {
                    op_id: node.op_id().map(|id| id.to_string()),
                    git_oid: node.git_oid(),
                    repository: node.repository(),
                    summary: node.summary(),
                    timestamp_ms: node.timestamp_ms(),
                    group: node.group(),
                    node_key: node.node_key(),
                    parents: node.parent_keys(&self.projection.git.links),
                    is_submodule: node
                        .repository()
                        .is_some_and(|rid| self.repo_is_submodule(rid)),
                    is_system: node_is_system(&node),
                    author: node_author(&node),
                    commit_id: node_commit_id(&node),
                    kind: node.kind(),
                    lane,
                    above,
                    below,
                    transitions,
                }
            })
            .collect();
        let max_lane = ctx.map_or(0, |c| c.lanes.iter().map(|r| r.lane).max().unwrap_or(0));
        HistoryWindow {
            rows,
            total: u64::try_from(total).unwrap_or(u64::MAX),
            chain_generation: 0,
            max_lane,
        }
    }

    /// Returns the canonical node list, optionally filtering out submodules and
    /// applying a chain filter.
    ///
    /// This is the single source of truth for both [`Self::history_window`] and
    /// [`Self::graph_layout`], so their row indices stay in lockstep. The result
    /// is cached per `(hide_submodules, filter)` so the expensive topological
    /// sort runs only once per filter state.
    #[must_use]
    fn filtered_nodes(
        &mut self,
        hide_submodules: bool,
        filter: &ChainFilter,
    ) -> Vec<editchain_project::HistoryNode> {
        let key = (hide_submodules, filter.key());
        if let Some(nodes) = self.sorted_nodes.get(&key) {
            return nodes.clone();
        }
        let all_nodes = self.projection.filtered_nodes(filter);
        let filtered: Vec<_> = if hide_submodules {
            all_nodes
                .into_iter()
                .filter(|n| {
                    !n.repository()
                        .is_some_and(|rid| self.repo_is_submodule(rid))
                })
                .collect()
        } else {
            all_nodes
        };
        drop(self.sorted_nodes.insert(key, filtered.clone()));
        filtered
    }

    /// Compute the graph layout for a bounded window of rows.
    ///
    /// The layout context (all O(V) derived data) is cached per `hide_submodules`
    /// and computed once; only edges whose child falls inside `[offset,
    /// offset+limit)` are emitted. This keeps per-scroll cost proportional to the
    /// visible slice rather than the whole graph.
    #[must_use]
    #[expect(
        clippy::print_stderr,
        reason = "Diagnostic logging to the VS Code output pane via service stderr"
    )]
    pub fn graph_layout(
        &mut self,
        hide_submodules: bool,
        offset: u64,
        limit: u64,
        filter: &ChainFilter,
    ) -> ProtocolGraphLayout {
        // Ensure the context is built (once per filter state), then borrow it.
        let key = (hide_submodules, filter.key());
        if !self.contexts.contains_key(&key) {
            let sorted = self.filtered_nodes(hide_submodules, filter);
            let ctx = self.projection.layout_context(&sorted);
            drop(self.contexts.insert(key.clone(), ctx));
        }
        let Some(ctx) = self.contexts.get(&key) else {
            return ProtocolGraphLayout {
                rows: Vec::new(),
                edges: Vec::new(),
                max_lane: 0,
            };
        };
        let offset_usize = usize::try_from(offset).unwrap_or(0);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        let edges = ctx.edges_for_window(offset_usize, limit_usize);
        eprintln!(
            "[layout] GetLayout offset={} limit={} rows={} edges={} max_lane={}",
            offset_usize,
            limit_usize,
            ctx.lanes.len(),
            edges.len(),
            ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0)
        );

        // Only send the window slice of rows (the webview needs lanes only for
        // visible rows). Sending all V rows would serialize the whole graph on
        // every scroll.
        let row_end = offset_usize
            .saturating_add(limit_usize)
            .min(ctx.lanes.len());
        let rows = ctx
            .lanes
            .get(offset_usize..row_end)
            .unwrap_or(&[])
            .iter()
            .map(|r| LayoutRow {
                node: r.node.clone(),
                lane: r.lane,
            })
            .collect();

        // Global max lane across ALL rows (not just this window), so the client
        // can size the graph column stably regardless of which window is loaded.
        let max_lane = ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0);

        ProtocolGraphLayout {
            rows,
            edges: edges
                .into_iter()
                .map(|e| LayoutEdge {
                    child: e.child,
                    parent: e.parent,
                    points: e
                        .points
                        .into_iter()
                        .map(|p| LayoutPoint {
                            row: p.row,
                            lane: p.lane,
                        })
                        .collect(),
                })
                .collect(),
            max_lane,
        }
    }

    /// Get details for a specific node by operation ID or git OID.
    #[must_use]
    pub fn node_details(
        &self,
        op_id: Option<String>,
        git_oid: Option<GitOid>,
    ) -> Option<NodeDetails> {
        if let Some(op_id_str) = op_id {
            let op_id = OpId::from_display_str(&op_id_str)?;
            let op = self.projection.ops.iter().find(|op| op.id == op_id)?;
            return Some(node_details_from_op(op));
        }
        if let Some(oid) = git_oid {
            let commit = self
                .projection
                .git
                .commits
                .values()
                .find(|c| c.oid == oid)?;
            return Some(node_details_from_commit(commit));
        }
        None
    }

    /// List discovered repositories.
    #[must_use]
    pub fn repositories_info(&self) -> Vec<RepositoryInfo> {
        self.repositories
            .iter()
            .map(|d| RepositoryInfo {
                id: d.id,
                path: d.path.to_string_lossy().to_string(),
                is_worktree: d.is_worktree,
                is_submodule: self.is_submodule(d),
            })
            .collect()
    }

    /// Returns true if a repository is nested inside another discovered repo
    /// (i.e. a submodule or vendored nested repo, not the workspace root).
    #[must_use]
    fn is_submodule(&self, discovery: &editchain_git::RepositoryDiscovery) -> bool {
        // Discovery paths point at `.git`; the repo root is the parent dir.
        let root = discovery.path.parent().unwrap_or(&discovery.path);
        let root_str = root.to_string_lossy();
        self.repositories.iter().any(|other| {
            if other.id == discovery.id || other.path == discovery.path {
                return false;
            }
            let other_root = other.path.parent().unwrap_or(&other.path);
            let other_str = other_root.to_string_lossy();
            // This repo's root is strictly inside another repo's root.
            other_str.len() < root_str.len() && root_str.starts_with(&*other_str)
        })
    }

    /// Returns true if the repository with the given id is a submodule.
    #[must_use]
    fn repo_is_submodule(&self, repository_id: editchain_core::RepositoryId) -> bool {
        self.repositories
            .iter()
            .any(|d| d.id == repository_id && self.is_submodule(d))
    }
}

/// Convert an optional protocol filter DTO into a [`ChainFilter`].
///
/// A `None` DTO yields the default filter (hide undated, splice on), matching
/// the webview's default behavior. An empty DTO yields an empty filter that
/// hides nothing.
#[must_use]
fn chain_filter_from_dto(dto: Option<&ChainFilterDto>) -> ChainFilter {
    match dto {
        Some(d) => ChainFilter::new(
            d.summary_pattern.clone(),
            d.kind_pattern.clone(),
            d.hide_undated,
            d.splice,
        ),
        None => ChainFilter::default(),
    }
}

/// Whether a history node is a system-generated artifact (tool results, raw
/// import records) rather than user-facing text.
///
/// The viewer uses this to dim or hide such rows. It is derived from the node's
/// kind — not from sniffing the summary text — so it stays correct regardless
/// of content.
#[must_use]
fn node_is_system(node: &editchain_project::HistoryNode) -> bool {
    match node {
        editchain_project::HistoryNode::EditOperation(op) => {
            matches!(op.kind, OpKind::Tool(_) | OpKind::Import(_))
        }
        // Collapsed imports fold a raw import + its children into one node; the
        // dominant child kind tells us whether it is user-facing text or a
        // system artifact.
        editchain_project::HistoryNode::CollapsedImport { kind, .. } => {
            kind == "tool" || kind == "import"
        }
        editchain_project::HistoryNode::GitCommit(_) => false,
    }
}

/// Author display value for a history node.
///
/// Git commits show the commit author's name. `EditChain` ops have no stored
/// author name (their actor is a derived hash), so they show a tag-derived
/// label (`human` / `agent` / `system`) so the Author column reads uniformly
/// across both row types instead of being blank for ops.
#[must_use]
fn node_author(node: &editchain_project::HistoryNode) -> String {
    match node {
        editchain_project::HistoryNode::EditOperation(op) => op_author_label(op.tags),
        // Collapsed imports carry their author label directly (derived from the
        // children's tags in the projection), since the raw import op's own tags
        // only carry `IMPORT`.
        editchain_project::HistoryNode::CollapsedImport { author, .. } => author.clone(),
        editchain_project::HistoryNode::GitCommit(commit) => payload_text(&commit.author.name),
    }
}

/// Derive a short author label from an op's tags.
///
/// Prefers the actor role tags (`HUMAN` / `AGENT`); falls back to `system` for
/// anything else (imports, tools, commands, etc.).
#[must_use]
fn op_author_label(tags: Tags) -> String {
    if tags.matches_any(Tags::HUMAN) {
        "human".to_string()
    } else if tags.matches_any(Tags::AGENT) {
        "agent".to_string()
    } else {
        "system".to_string()
    }
}

/// Commit/ID display value for a history node.
///
/// Git commits show an abbreviated OID; `EditChain` ops show an abbreviated
/// op id (`node:seq`, dropping the boot counter) so both row types read as a
/// short, uniform identifier in this column.
#[must_use]
fn node_commit_id(node: &editchain_project::HistoryNode) -> String {
    match node {
        editchain_project::HistoryNode::EditOperation(op)
        | editchain_project::HistoryNode::CollapsedImport { op, .. } => abbreviate_op_id(&op.id),
        editchain_project::HistoryNode::GitCommit(commit) => abbreviate_oid(&commit.oid),
    }
}

/// Abbreviate an op id (`node:boot:seq`) to a short `node:seq` form.
///
/// The boot counter is almost always 0 and adds noise; dropping it keeps the
/// column compact while preserving the distinguishing sequence number.
#[must_use]
fn abbreviate_op_id(id: &OpId) -> String {
    format!("{}:{}", id.node.0, id.seq)
}

/// Abbreviate a git OID to its first 7 hex characters.
#[must_use]
fn abbreviate_oid(oid: &GitOid) -> String {
    let hex = oid.to_hex();
    hex.chars().take(7).collect()
}

/// Build node details from an `EditChain` operation.
#[must_use]
fn node_details_from_op(op: &Op) -> NodeDetails {
    NodeDetails {
        op_id: Some(op.id.to_string()),
        git_oid: None,
        repository: None,
        summary: op_summary(op),
        body: op_body(op),
        parents: op.parents.iter().copied().collect(),
        git_parents: Vec::new(),
        refs: Vec::new(),
        changed_paths: Vec::new(),
    }
}

/// Build node details from a git commit entity.
#[must_use]
fn node_details_from_commit(commit: &editchain_core::GitCommitEntity) -> NodeDetails {
    let body = match &commit.message {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    };
    let refs = commit
        .live_refs
        .iter()
        .chain(commit.imported_refs.iter())
        .filter_map(|r| match r {
            Payload::Inline(b) => Some(String::from_utf8_lossy(b).to_string()),
            Payload::Empty | Payload::Blob(_) => None,
        })
        .collect();
    let changed_paths = commit
        .changed_paths
        .iter()
        .map(|p| p.0.to_string())
        .collect();
    NodeDetails {
        op_id: commit.imported_record.map(|id| id.to_string()),
        git_oid: Some(commit.oid),
        repository: Some(commit.repository),
        summary: body.clone(),
        body,
        parents: Vec::new(),
        git_parents: commit.parents.clone(),
        refs,
        changed_paths,
    }
}

/// Read all decoded operations from a chain directory.
///
/// # Errors
///
/// Returns an error if the chain directory cannot be read.
fn read_chain_ops(chain_dir: &PathBuf) -> Result<Vec<Op>, Box<dyn std::error::Error>> {
    if chain_dir.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    let store = SegmentStore::open(chain_dir)?;
    let pages = store.read_all()?;
    let mut ops = Vec::new();
    for page in &pages {
        for record in &page.records {
            if let Ok(op) = decode_op(&record.data) {
                ops.push(op);
            }
        }
    }
    Ok(ops)
}

/// Build a lexical index over all chain ops and git commits.
///
/// # Errors
///
/// Returns an error if the index cannot be created or populated.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Generation counter increments are bounded by the number of indexed ops"
)]
fn build_lexical_index(workspace: &Workspace) -> Result<LexicalIndex, Box<dyn std::error::Error>> {
    let mut index = LexicalIndex::new()?;
    let mut generation = 0u64;
    for op in &workspace.projection.ops {
        drop(index.index_op(op, generation)?);
        generation += 1;
    }
    // Index git commits as synthetic ops.
    for commit in workspace.projection.git.commits.values() {
        let op = Op {
            id: OpId::new(NodeId(0), 0, generation),
            parents: ParentSet::None,
            actor: ActorId(0),
            clock: Clock::UnixMs(u64::try_from(commit.committed_at).unwrap_or(0)),
            scope: ScopeRef::None,
            tags: Tags::IMPORT,
            kind: OpKind::GitCommit(Box::new(commit.clone())),
        };
        drop(index.index_op(&op, generation)?);
        generation += 1;
    }
    index.commit()?;
    Ok(index)
}

/// Resolve a git commit by OID in a discovered repository.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or the object cannot
/// be resolved.
pub fn resolve_git_commit(
    workspace: &Workspace,
    repository_id: editchain_core::RepositoryId,
    oid: &GitOid,
) -> Result<Option<editchain_core::GitCommitEntity>, Box<dyn std::error::Error>> {
    let Some(discovery) = workspace
        .repositories
        .iter()
        .find(|d| d.id == repository_id)
    else {
        return Ok(None);
    };
    let Ok(handle) = open_repository_handle(discovery) else {
        return Ok(None);
    };
    match resolve_commit(&handle, oid) {
        Ok(res) => Ok(Some(res.commit)),
        Err(_) => Ok(None),
    }
}

/// Open a discovered repository as a `RepositoryHandle`.
fn open_repository_handle(
    discovery: &editchain_git::RepositoryDiscovery,
) -> Result<RepositoryHandle, Box<dyn std::error::Error>> {
    let open_path = if discovery.is_worktree {
        discovery
            .path
            .parent()
            .unwrap_or(&discovery.path)
            .to_path_buf()
    } else {
        discovery.path.clone()
    };
    let repo = gix::open(&open_path)?;
    Ok(RepositoryHandle {
        repo,
        discovery: discovery.clone(),
    })
}

/// A stateful server that owns a loaded workspace across requests.
#[derive(Debug)]
pub struct Server {
    /// The currently loaded workspace (None until `Open`).
    pub workspace: Option<Workspace>,
    /// The lexical search index (built on `Open`).
    pub lexical: Option<LexicalIndex>,
}

impl Server {
    /// Create a new empty server.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workspace: None,
            lexical: None,
        }
    }

    /// Handle a single request against the current state.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be handled.
    pub fn handle(&mut self, request: &Request) -> Result<Response, Box<dyn std::error::Error>> {
        let id = request.id;
        let body = match &request.body {
            RequestBody::Open(req) => {
                let workspace = Workspace::open(&req.workspace_path, &req.chain_dir)?;
                // The lexical index is built lazily on first Search (it is
                // expensive for large chains and unnecessary for the graph view).
                self.workspace = Some(workspace);
                self.lexical = None;
                ResponseBody::Ok(serde_json::json!({
                    "workspace": req.workspace_path,
                    "chain": req.chain_dir,
                    "repos": self.workspace.as_ref().map_or(0, |w| w.repositories.len()),
                    "nodes": self.workspace.as_ref().map_or(0, |w| w.projection.len()),
                }))
            }
            RequestBody::GetWindow(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let window = ws.history_window(req.offset, req.limit, req.hide_submodules, &filter);
                ResponseBody::Ok(serde_json::to_value(window)?)
            }
            RequestBody::GetLayout(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let layout = ws.graph_layout(req.hide_submodules, req.offset, req.limit, &filter);
                ResponseBody::Ok(serde_json::to_value(layout)?)
            }
            RequestBody::GetNodeDetails(req) => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                match ws.node_details(Some(req.op_id.clone()), None) {
                    Some(details) => ResponseBody::Ok(serde_json::to_value(details)?),
                    None => ResponseBody::Error("node not found".to_string()),
                }
            }
            RequestBody::GetRepositories => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                ResponseBody::Ok(serde_json::to_value(ws.repositories_info())?)
            }
            RequestBody::ResolveObject(req) => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                match resolve_git_commit(ws, req.repository, &req.oid)? {
                    Some(commit) => ResponseBody::Ok(serde_json::to_value(&commit)?),
                    None => ResponseBody::Error("object not found".to_string()),
                }
            }
            RequestBody::SetFilters(_) => ResponseBody::Error("filters not yet wired".to_string()),
            RequestBody::Search(req) => {
                // Build the lexical index lazily on first search.
                if self.lexical.is_none() {
                    let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                    self.lexical = Some(build_lexical_index(ws)?);
                }
                let lexical = self.lexical.as_ref().ok_or("no index built")?;
                let results = lexical.search_internal(&req.query, &req.filters, req.top_k)?;
                ResponseBody::Ok(serde_json::to_value(results)?)
            }
        };
        Ok(Response { id, body })
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

/// Produce a short summary for an `EditChain` operation.
#[must_use]
fn op_summary(op: &Op) -> String {
    match &op.kind {
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.tool_name),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::File(f) => format!("file:{}", f.path.0),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(cs) => String::from_utf8_lossy(&cs.name).to_string(),
        OpKind::Actor(a) => payload_text(&a.label),
        OpKind::Import(i) => payload_text(&i.raw_ref),
        OpKind::GitCommit(c) => payload_text(&c.message),
        OpKind::GitLink(l) => format!("git:{}", l.target_oid),
        OpKind::Unknown(u) => format!("unknown kind={}", u.kind_discriminant),
    }
}

/// Produce the full body text for an `EditChain` operation.
#[must_use]
fn op_body(op: &Op) -> String {
    match &op.kind {
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.content),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::File(_)
        | OpKind::Import(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => String::new(),
    }
}

/// Extract text from a payload, or empty string.
#[must_use]
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}
