//! UI-neutral history projections for the unified `EditChain` + `Git` viewer.
//!
//! This crate builds deterministic projections over `EditChain` operations and
//! `Git` commits, and provides windowed/paged access for the viewer. It is
//! intentionally free of filesystem and process dependencies so it can later
//! target WASM.

/// Deterministic lane layout for graph rendering.
pub mod layout;
/// History linking — stitch sessions and git into a single edit chain.
pub mod link;

use std::collections::HashMap;

use editchain_core::{GitCommitEntity, GitOid, GitProjection, Op, OpId, Payload, RepositoryId};

use crate::layout::{compute_graph_layout, compute_lane_assignment, GraphLayout, GraphRow};
use crate::link::link_history;

/// A unified history row — either an `EditChain` operation or a `Git` commit.
#[derive(Debug, Clone)]
pub enum HistoryNode {
    /// An `EditChain` operation.
    EditOperation(Op),
    /// A `Git` commit entity.
    GitCommit(GitCommitEntity),
}

impl HistoryNode {
    /// Returns a display summary for this node.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::EditOperation(op) => op_summary(op),
            Self::GitCommit(commit) => match &commit.message {
                Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
                Payload::Empty | Payload::Blob(_) => commit.oid.to_hex(),
            },
        }
    }

    /// Returns the timestamp in Unix ms (0 if unknown).
    ///
    /// Git commits store `committed_at` in Unix **seconds**; `EditChain` ops
    /// store their clock in Unix **milliseconds**. This converts git seconds
    /// to milliseconds so both render as correct dates.
    #[must_use]
    pub fn timestamp_ms(&self) -> u64 {
        match self {
            Self::EditOperation(op) => op.clock.as_u64(),
            Self::GitCommit(commit) => {
                let secs = u64::try_from(commit.committed_at).unwrap_or(0);
                secs.saturating_mul(1000)
            }
        }
    }

    /// Returns the operation ID, if this is an `EditChain` operation.
    #[must_use]
    pub fn op_id(&self) -> Option<OpId> {
        match self {
            Self::EditOperation(op) => Some(op.id),
            Self::GitCommit(_) => None,
        }
    }

    /// Returns the git commit OID, if this is a git commit.
    #[must_use]
    pub fn git_oid(&self) -> Option<GitOid> {
        match self {
            Self::EditOperation(_) => None,
            Self::GitCommit(commit) => Some(commit.oid),
        }
    }

    /// Returns the repository, if this is a git commit.
    #[must_use]
    pub fn repository(&self) -> Option<RepositoryId> {
        match self {
            Self::EditOperation(_) => None,
            Self::GitCommit(commit) => Some(commit.repository),
        }
    }

    /// Returns a grouping key for block separation.
    ///
    /// `EditChain` ops group by their session scope (or "ops" if unscoped);
    /// git commits group by their repository id.
    #[must_use]
    pub fn group(&self) -> String {
        match self {
            Self::EditOperation(op) => match op.scope {
                editchain_core::ScopeRef::Session(sid) => format!("session:{}", sid.0),
                editchain_core::ScopeRef::None
                | editchain_core::ScopeRef::Chain(_)
                | editchain_core::ScopeRef::Turn(_)
                | editchain_core::ScopeRef::File(_) => "ops".to_string(),
            },
            Self::GitCommit(commit) => format!("repo:{}", commit.repository.0),
        }
    }

    /// Returns a stable node key for graph wiring.
    ///
    /// `EditChain` ops use their `OpId` string; git commits use their OID hex.
    #[must_use]
    pub fn node_key(&self) -> String {
        match self {
            Self::EditOperation(op) => op.id.to_string(),
            Self::GitCommit(commit) => commit.oid.to_hex(),
        }
    }

    /// Returns the parent node keys for drawing graph edges.
    ///
    /// For `EditChain` ops, this includes both the causal `Op.parents` and any
    /// explicit git links (whose target OID hex becomes a parent key, so the
    /// graph draws an edge from the op to that commit).
    #[must_use]
    pub fn parent_keys(
        &self,
        git_links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
    ) -> Vec<String> {
        match self {
            Self::EditOperation(op) => {
                let mut keys: Vec<String> = op.parents.iter().map(ToString::to_string).collect();
                if let Some(links) = git_links.get(&op.id) {
                    for link in links {
                        keys.push(link.target_oid.to_hex());
                    }
                }
                keys
            }
            Self::GitCommit(commit) => commit.parents.iter().map(GitOid::to_hex).collect(),
        }
    }
}

/// A unified history projection over `EditChain` ops and `Git` commits.
#[derive(Debug, Clone, Default)]
pub struct HistoryProjection {
    /// `EditChain` operations in canonical causal order (oldest-first).
    pub ops: Vec<Op>,
    /// `Git` commits keyed by `(RepositoryId, GitOid)`.
    pub git: GitProjection,
}

impl HistoryProjection {
    /// Create an empty projection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ops: Vec::new(),
            git: GitProjection::new(),
        }
    }

    /// Build a projection from a set of operations.
    ///
    /// Operations are stored in input order; git commits are projected into
    /// the `GitProjection` keyed by `(RepositoryId, GitOid)`.
    #[must_use]
    pub fn from_ops(ops: Vec<Op>) -> Self {
        let mut git = GitProjection::new();
        for op in &ops {
            git.reduce(op);
        }
        Self { ops, git }
    }

    /// Returns the number of history nodes (ops + git commits).
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len().saturating_add(self.git.commits.len())
    }

    /// Returns true if the projection is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns all history nodes (newest-first).
    ///
    /// Uses the same canonical topologically-sorted ordering as
    /// [`Self::graph_layout`], so layout row indices always correspond to
    /// window row positions.
    #[must_use]
    pub fn nodes(&self) -> Vec<HistoryNode> {
        self.ordered_nodes()
    }

    /// Returns a window of history nodes (newest-first).
    ///
    /// The window is a slice of the canonical topologically-sorted ordering,
    /// matching [`Self::graph_layout`]. `offset`/`limit` provide cursor-based
    /// paging.
    #[must_use]
    pub fn window(&self, offset: usize, limit: usize) -> Vec<HistoryNode> {
        self.ordered_nodes()
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect()
    }

    /// Returns the canonical topologically-sorted node list (newest-first).
    ///
    /// Git commits are stored in `BTreeMap` order (by repository + OID), which
    /// is *not* topological w.r.t. ancestry, so we re-sort them here. This is
    /// the single source of truth for both the windowed rows and the graph
    /// layout, guaranteeing they stay in lockstep.
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::let_underscore_untyped,
        reason = "In-degree counters are bounded by the number of present parents; HashMap insert returns Option which is discarded"
    )]
    fn ordered_nodes(&self) -> Vec<HistoryNode> {
        // Build a unified node list: ops (newest-first) then git commits.
        let mut nodes: Vec<HistoryNode> = Vec::with_capacity(self.len());
        for op in self.ops.iter().rev() {
            nodes.push(HistoryNode::EditOperation(op.clone()));
        }
        for commit in self.git.commits.values() {
            nodes.push(HistoryNode::GitCommit(commit.clone()));
        }

        // Kahn's algorithm (O(V+E)) for topological sort, oldest-first.
        //
        // A parent blocks a node only if it is present in the list; parents
        // outside the list (e.g. a git commit whose parent wasn't imported) do
        // not block. We emit parents before children, then reverse the result so
        // the final list is newest-first — guaranteeing every edge that *can* be
        // drawn points downward (child above, parent below).
        let present: std::collections::HashSet<String> =
            nodes.iter().map(HistoryNode::node_key).collect();
        let node_by_key: HashMap<String, HistoryNode> =
            nodes.iter().map(|n| (n.node_key(), n.clone())).collect();

        // Reverse adjacency (parent -> children) and in-degree (count of present
        // parents still un-emitted).
        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        let mut indegree: HashMap<String, usize> = HashMap::new();
        for node in &nodes {
            let key = node.node_key();
            let _ = indegree.entry(key.clone()).or_insert(0);
            for parent in node.parent_keys(&self.git.links) {
                if present.contains(&parent) {
                    children_of.entry(parent).or_default().push(key.clone());
                    *indegree.entry(key.clone()).or_insert(0) += 1;
                }
            }
        }

        // Seed the queue with nodes that have no present parents.
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        for (key, &deg) in &indegree {
            if deg == 0 {
                queue.push_back(key.clone());
            }
        }

        let mut sorted_oldest_first: Vec<HistoryNode> = Vec::with_capacity(nodes.len());
        while let Some(key) = queue.pop_front() {
            if let Some(node) = node_by_key.get(&key) {
                sorted_oldest_first.push(node.clone());
            }
            if let Some(children) = children_of.get(&key) {
                for child in children {
                    if let Some(deg) = indegree.get_mut(child) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(child.clone());
                        }
                    }
                }
            }
        }

        // If some nodes remain (a cycle), append them in original order so we
        // don't drop history. Edges to un-emitted parents simply won't be drawn.
        if sorted_oldest_first.len() < nodes.len() {
            let emitted: std::collections::HashSet<String> = sorted_oldest_first
                .iter()
                .map(HistoryNode::node_key)
                .collect();
            for node in &nodes {
                if !emitted.contains(&node.node_key()) {
                    sorted_oldest_first.push(node.clone());
                }
            }
        }

        // Reverse to newest-first.
        sorted_oldest_first.reverse();
        sorted_oldest_first
    }

    /// Merge resolved git commits into the projection.
    ///
    /// Commits are keyed by `(RepositoryId, GitOid)`; a later commit with the
    /// same key replaces an earlier one.
    pub fn merge_git_commits(&mut self, commits: Vec<GitCommitEntity>) {
        for commit in commits {
            drop(
                self.git
                    .commits
                    .insert((commit.repository, commit.oid), commit),
            );
        }
    }

    /// Stitch sessions and git history into a single edit chain.
    ///
    /// Applies session-to-session stitching (mutating `Op.parents`) and creates
    /// op→git links (stored in `GitProjection.links`). Call after loading all ops
    /// and git commits, before computing windows or layouts.
    pub fn link_history(&mut self) {
        let commits: Vec<GitCommitEntity> = self.git.commits.values().cloned().collect();
        let result = link_history(&self.ops, &commits);
        self.ops = result.ops;
        for link in result.git_links {
            let entry = self.git.links.entry(link.source).or_default();
            entry.push(link);
        }
    }

    /// Compute the graph layout for rendering unified history.
    ///
    /// The layout is computed over the same canonical topologically-sorted node
    /// list as [`Self::nodes`]/[`Self::window`], so layout row indices always
    /// correspond to window row positions. Every edge's parent appears below its
    /// child (newest-first).
    #[must_use]
    pub fn graph_layout(&self) -> GraphLayout {
        self.graph_layout_filtered(&self.ordered_nodes())
    }

    /// Compute the graph layout over a pre-sorted node list.
    ///
    /// `sorted` must be in the same canonical newest-first order as the rows the
    /// webview renders (i.e. from [`Self::ordered_nodes`], possibly filtered), so
    /// layout row indices correspond to window row positions. Every edge's parent
    /// appears below its child.
    #[must_use]
    pub fn graph_layout_filtered(&self, sorted: &[HistoryNode]) -> GraphLayout {
        let keys = Self::layout_keys(sorted);
        let key_to_node = Self::layout_index(sorted);
        let links = &self.git.links;
        let parents_of = |key: &str| -> Vec<String> {
            key_to_node
                .get(key)
                .map_or(Vec::new(), |n| n.parent_keys(links))
        };
        compute_graph_layout(&keys, parents_of)
    }

    /// Compute the lane assignment over a pre-sorted node list (no edges).
    ///
    /// This is linear and stable across viewport sizes, so it can be cached and
    /// reused for many windowed edge computations.
    #[must_use]
    pub fn lane_assignment_filtered(&self, sorted: &[HistoryNode]) -> Vec<GraphRow> {
        let keys = Self::layout_keys(sorted);
        let key_to_node = Self::layout_index(sorted);
        let links = &self.git.links;
        let parents_of = |key: &str| -> Vec<String> {
            key_to_node
                .get(key)
                .map_or(Vec::new(), |n| n.parent_keys(links))
        };
        compute_lane_assignment(&keys, &parents_of)
    }

    /// Build a cached [`layout::LayoutContext`] over a pre-sorted node list.
    ///
    /// The context bundles all O(V) derived data (keys, row map, lane map, lane
    /// assignment) so per-window edge computation is O(window). Build once per
    /// filter state and reuse across scrolls/resizes.
    #[must_use]
    pub fn layout_context(&self, sorted: &[HistoryNode]) -> layout::LayoutContext {
        let keys = Self::layout_keys(sorted);
        let key_to_node = Self::layout_index(sorted);
        let links = &self.git.links;
        let parents_of = |key: &str| -> Vec<String> {
            key_to_node
                .get(key)
                .map_or(Vec::new(), |n| n.parent_keys(links))
        };
        layout::LayoutContext::new(&keys, &parents_of)
    }

    /// Build the string-keyed node list for layout.
    fn layout_keys(sorted: &[HistoryNode]) -> Vec<String> {
        sorted.iter().map(HistoryNode::node_key).collect()
    }

    /// Build a node-key → node index over a pre-sorted node list.
    fn layout_index(sorted: &[HistoryNode]) -> HashMap<String, &HistoryNode> {
        sorted.iter().map(|n| (n.node_key(), n)).collect()
    }
}

/// Produce a short summary for an `EditChain` operation.
#[must_use]
fn op_summary(op: &Op) -> String {
    use editchain_core::OpKind;
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

/// Extract text from a payload, or empty string.
#[must_use]
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}
