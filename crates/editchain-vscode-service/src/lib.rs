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
use editchain_project::HistoryProjection;
use editchain_protocol::{
    HistoryRow, HistoryWindow, NodeDetails, RepositoryInfo, Request, RequestBody, Response,
    ResponseBody,
};

/// A loaded workspace: chain ops + git repositories.
#[derive(Debug)]
pub struct Workspace {
    /// The unified history projection.
    pub projection: HistoryProjection,
    /// Discovered git repositories.
    pub repositories: Vec<editchain_git::RepositoryDiscovery>,
}

impl Workspace {
    /// Load a workspace from a chain directory and discover git repos.
    ///
    /// # Errors
    ///
    /// Returns an error if the chain cannot be read or repos cannot be discovered.
    pub fn open(workspace_path: &str, chain_dir: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let ops = read_chain_ops(chain_dir)?;
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
        Ok(Self {
            projection,
            repositories,
        })
    }

    /// Get a window of history rows (newest-first).
    #[must_use]
    pub fn history_window(&self, offset: u64, limit: u64, hide_submodules: bool) -> HistoryWindow {
        let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        // Build the full node list, optionally filtering out submodules.
        let all_nodes = self.projection.nodes();
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
        let total = filtered.len();

        let rows = filtered
            .into_iter()
            .skip(offset_usize)
            .take(limit_usize)
            .map(|node| HistoryRow {
                op_id: node.op_id(),
                git_oid: node.git_oid(),
                repository: node.repository(),
                summary: node.summary(),
                timestamp_ms: node.timestamp_ms(),
                group: node.group(),
                node_key: node.node_key(),
                parents: node.parent_keys(),
                is_submodule: node
                    .repository()
                    .is_some_and(|rid| self.repo_is_submodule(rid)),
            })
            .collect();
        HistoryWindow {
            rows,
            total: u64::try_from(total).unwrap_or(u64::MAX),
            chain_generation: 0,
        }
    }

    /// Get details for a specific node by operation ID or git OID.
    #[must_use]
    pub fn node_details(
        &self,
        op_id: Option<OpId>,
        git_oid: Option<GitOid>,
    ) -> Option<NodeDetails> {
        if let Some(op_id) = op_id {
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

/// Build node details from an `EditChain` operation.
#[must_use]
fn node_details_from_op(op: &Op) -> NodeDetails {
    NodeDetails {
        op_id: Some(op.id),
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
        op_id: commit.imported_record,
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
fn read_chain_ops(chain_dir: &str) -> Result<Vec<Op>, Box<dyn std::error::Error>> {
    if chain_dir.is_empty() {
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
                let lexical = build_lexical_index(&workspace)?;
                self.workspace = Some(workspace);
                self.lexical = Some(lexical);
                ResponseBody::Ok(serde_json::json!({
                    "workspace": req.workspace_path,
                    "chain": req.chain_dir,
                    "repos": self.workspace.as_ref().map_or(0, |w| w.repositories.len()),
                    "nodes": self.workspace.as_ref().map_or(0, |w| w.projection.len()),
                }))
            }
            RequestBody::GetWindow(req) => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                let window = ws.history_window(req.offset, req.limit, req.hide_submodules);
                ResponseBody::Ok(serde_json::to_value(window)?)
            }
            RequestBody::GetNodeDetails(req) => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                match ws.node_details(Some(req.op_id), None) {
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
