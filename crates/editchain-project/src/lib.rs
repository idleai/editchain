//! UI-neutral history projections for the unified `EditChain` + `Git` viewer.
//!
//! This crate builds deterministic projections over `EditChain` operations and
//! `Git` commits, and provides windowed/paged access for the viewer. It is
//! intentionally free of filesystem and process dependencies so it can later
//! target WASM.

// Crate-level dependency marker (used by Cargo for feature resolution).
use regex as _;

/// General chain filtering with truncation.
pub mod filter;
/// Deterministic lane layout for graph rendering.
pub mod layout;
/// History linking — stitch sessions and git into a single edit chain.
pub mod link;

use std::collections::HashMap;

use editchain_core::{
    Clock, GitCommitEntity, GitOid, GitProjection, Op, OpId, Payload, RepositoryId,
};

use crate::layout::{compute_graph_layout, compute_lane_assignment, GraphLayout, GraphRow};
use crate::link::link_history;

/// A unified history row — either an `EditChain` operation or a `Git` commit.
#[derive(Debug, Clone)]
pub enum HistoryNode {
    /// An `EditChain` operation.
    EditOperation(Op),
    /// A raw import op collapsed with its normalized children into one node.
    ///
    /// The raw import op forms the linear backbone of a session; its normalized
    /// children (messages, tools, commands) are folded into this single node so
    /// the graph reads as a clean chain rather than a dense star per line. The
    /// `summary` is derived from the children's content (not the raw JSONL).
    CollapsedImport {
        /// The underlying raw import op (kept for id/clock/parents).
        op: Op,
        /// Display summary derived from the normalized children.
        summary: String,
        /// Dominant child kind (e.g. "tool", "message", "command") for styling.
        kind: String,
        /// Author label derived from the children's tags (`human` / `agent` /
        /// `system`). The raw import op's own tags only carry `IMPORT`, so the
        /// role must come from the normalized children that carry `HUMAN` /
        /// `AGENT`.
        author: String,
    },
    /// A `Git` commit entity.
    GitCommit(GitCommitEntity),
}

impl HistoryNode {
    /// Returns a display summary for this node.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::EditOperation(op) => op_summary(op),
            Self::CollapsedImport { summary, .. } => summary.clone(),
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
            Self::EditOperation(op) | Self::CollapsedImport { op, .. } => op.clock.as_u64(),
            Self::GitCommit(commit) => {
                let secs = u64::try_from(commit.committed_at).unwrap_or(0);
                secs.saturating_mul(1000)
            }
        }
    }

    /// Set an effective timestamp (Unix ms) on a node that lacks one.
    ///
    /// Used to give timestamp-less nodes the date of the first dated node that
    /// follows them, so they interleave correctly instead of clustering at the
    /// top of the history (which would otherwise inflate the lane count). For
    /// ops this sets the clock; for git commits it sets `committed_at` (seconds).
    pub fn set_effective_timestamp(&mut self, ms: u64) {
        match self {
            Self::EditOperation(op) | Self::CollapsedImport { op, .. } => {
                op.clock = Clock::UnixMs(ms);
            }
            Self::GitCommit(commit) => {
                let secs = i64::try_from(ms / 1000).unwrap_or(i64::MAX);
                commit.committed_at = secs;
            }
        }
    }

    /// Returns the operation ID, if this is an `EditChain` operation.
    #[must_use]
    pub fn op_id(&self) -> Option<OpId> {
        match self {
            Self::EditOperation(op) | Self::CollapsedImport { op, .. } => Some(op.id),
            Self::GitCommit(_) => None,
        }
    }

    /// Returns the git commit OID, if this is a git commit.
    #[must_use]
    pub fn git_oid(&self) -> Option<GitOid> {
        match self {
            Self::EditOperation(_) | Self::CollapsedImport { .. } => None,
            Self::GitCommit(commit) => Some(commit.oid),
        }
    }

    /// Returns the repository, if this is a git commit.
    #[must_use]
    pub fn repository(&self) -> Option<RepositoryId> {
        match self {
            Self::EditOperation(_) | Self::CollapsedImport { .. } => None,
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
            Self::EditOperation(op) | Self::CollapsedImport { op, .. } => match op.scope {
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
            Self::EditOperation(op) | Self::CollapsedImport { op, .. } => op.id.to_string(),
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
            Self::EditOperation(op) | Self::CollapsedImport { op, .. } => {
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

    /// Rewrite this node's causal parents to a new set of parent keys.
    ///
    /// Used by chain filtering to splice edges across hidden intermediate nodes.
    /// Op parents are parsed from `"node:boot:seq"` display strings; git parents
    /// are parsed from OID hex. Keys that fail to parse are dropped.
    #[expect(
        clippy::indexing_slicing,
        reason = "ids[0]/ids[1] are guarded by the match on ids.len()"
    )]
    pub fn set_parent_keys(&mut self, keys: &[String]) {
        match self {
            Self::EditOperation(op) | Self::CollapsedImport { op, .. } => {
                let mut ids: Vec<OpId> = keys
                    .iter()
                    .filter_map(|k| OpId::from_display_str(k))
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                op.parents = match ids.len() {
                    0 => editchain_core::parents::ParentSet::None,
                    1 => editchain_core::parents::ParentSet::One(ids[0]),
                    _ => editchain_core::parents::ParentSet::Two(ids[0], ids[1]),
                };
            }
            Self::GitCommit(commit) => {
                let mut oids: Vec<GitOid> =
                    keys.iter().filter_map(|k| GitOid::from_hex(k)).collect();
                oids.sort_unstable();
                oids.dedup();
                commit.parents = oids;
            }
        }
    }

    /// Returns a short type tag for this node, used by the viewer to style rows.
    ///
    /// `EditChain` ops return their `OpKind` name (lowercased); collapsed imports
    /// return the dominant child kind; git commits return `"git"`.
    #[must_use]
    pub fn kind(&self) -> String {
        use editchain_core::OpKind;
        match self {
            Self::EditOperation(op) => match &op.kind {
                OpKind::ChainStart(_) => "chainstart".to_string(),
                OpKind::Actor(_) => "actor".to_string(),
                OpKind::Message(_) => "message".to_string(),
                OpKind::Tool(_) => "tool".to_string(),
                OpKind::Command(_) => "command".to_string(),
                OpKind::File(_) => "file".to_string(),
                OpKind::Reflection(_) => "reflection".to_string(),
                OpKind::Import(_) => "import".to_string(),
                OpKind::Note(_) => "note".to_string(),
                OpKind::Error(_) => "error".to_string(),
                OpKind::GitCommit(_) => "gitcommit".to_string(),
                OpKind::GitLink(_) => "gitlink".to_string(),
                OpKind::Unknown(_) => "unknown".to_string(),
            },
            // Collapsed imports report their dominant child kind so the viewer
            // can style tool calls vs messages differently.
            Self::CollapsedImport { kind, .. } => kind.clone(),
            Self::GitCommit(_) => "git".to_string(),
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

    /// Returns history nodes (newest-first) with a [`filter::ChainFilter`] applied.
    ///
    /// Hidden intermediate nodes are removed and (when the filter splices) their
    /// causal edges are reconnected to the nearest kept ancestors. The result is
    /// in the same canonical order as [`Self::nodes`], so layout row indices stay
    /// in lockstep with window row positions.
    #[must_use]
    pub fn filtered_nodes(&self, filter: &filter::ChainFilter) -> Vec<HistoryNode> {
        let nodes = self.ordered_nodes();
        filter::apply(&nodes, &self.git.links, filter)
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
        // Raw import ops are collapsed with their normalized children into a
        // single node so the graph reads as a clean chain rather than a dense
        // star per source line.
        let mut nodes: Vec<HistoryNode> = Vec::with_capacity(self.len());
        for op in self.collapsed_ops().into_iter().rev() {
            nodes.push(op);
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

        // Track which keys have not yet been emitted so we can break cycles
        // deterministically when Kahn stalls.
        let mut unemitted: std::collections::HashSet<String> = indegree.keys().cloned().collect();

        let mut sorted_oldest_first: Vec<HistoryNode> = Vec::with_capacity(nodes.len());
        while !unemitted.is_empty() {
            // Normal Kahn step: emit every queued node whose present parents
            // have all been emitted.
            while let Some(key) = queue.pop_front() {
                if !unemitted.contains(&key) {
                    continue;
                }
                if let Some(node) = node_by_key.get(&key) {
                    sorted_oldest_first.push(node.clone());
                }
                let _ = unemitted.remove(&key);
                if let Some(children) = children_of.get(&key) {
                    for child in children {
                        if let Some(deg) = indegree.get_mut(child) {
                            *deg -= 1;
                            if *deg == 0 && unemitted.contains(child) {
                                queue.push_back(child.clone());
                            }
                        }
                    }
                }
            }

            // If nodes remain, we've hit a cycle. Break it deterministically by
            // emitting the remaining node with the smallest in-degree (fewest
            // still-blocking present parents), tie-broken by key. Its remaining
            // parents are treated as dropped (their edges simply won't draw),
            // which keeps every other edge pointing forward in the final order.
            if !unemitted.is_empty() {
                // Pick the remaining node with the smallest in-degree (fewest
                // still-blocking present parents), tie-broken by key.
                let pick = unemitted
                    .iter()
                    .min_by(|a, b| {
                        indegree
                            .get(*a)
                            .copied()
                            .unwrap_or(0)
                            .cmp(&indegree.get(*b).copied().unwrap_or(0))
                            .then_with(|| a.cmp(b))
                    })
                    .cloned()
                    .unwrap_or_default();
                if let Some(node) = node_by_key.get(&pick) {
                    sorted_oldest_first.push(node.clone());
                }
                let _ = unemitted.remove(&pick);
                if let Some(children) = children_of.get(&pick) {
                    for child in children {
                        if let Some(deg) = indegree.get_mut(child) {
                            *deg -= 1;
                            if *deg == 0 && unemitted.contains(child) {
                                queue.push_back(child.clone());
                            }
                        }
                    }
                }
            }
        }

        // Reverse to newest-first.
        sorted_oldest_first.reverse();

        // Assign effective timestamps to nodes that lack one (timestamp_ms() == 0).
        // Each such node gets the timestamp of the next OP in the chain that has
        // a real date, minus a small offset (1 minute) so it sits just before
        // that dated neighbor. This pulls undated nodes out of the "top cluster"
        // (where they'd otherwise all sort newest and occupy lanes) and lets
        // them interleave with dated work, so their components can reuse freed
        // lanes instead of each claiming a fresh column.
        //
        // Only dated OPS are used as dating sources — git commits are skipped.
        // Git commits can carry genuinely old dates (e.g. vendored/external repo
        // history predating this repo), and we don't want an undated import op
        // to inherit a pre-repo date from one.
        //
        // Walk newest → oldest, remembering the most recent dated op's
        // timestamp; every undated node before it gets that date minus 1 minute.
        let mut next_dated_ts = 0u64;
        for node in &mut sorted_oldest_first {
            if node.timestamp_ms() == 0 {
                if next_dated_ts != 0 {
                    node.set_effective_timestamp(next_dated_ts.saturating_sub(60_000));
                }
            } else if node.git_oid().is_none() {
                // A dated op — use it as the dating source for undated nodes.
                next_dated_ts = node.timestamp_ms();
            }
            // Git commits are skipped as dating sources.
        }

        // Time-sort by default: interleave git commits and ops by timestamp so
        // newer work (whether a commit or an op) appears higher in the list.
        // This is a stable sort — nodes with equal or unknown (0) timestamps
        // keep their topological order, so parent-before-child is preserved for
        // causally-ordered nodes (whose clocks are monotonic). Time-sorting only
        // changes which row a node occupies; the lane assignment is computed
        // separately from topology and is unaffected.
        sorted_oldest_first.sort_by_key(|n| std::cmp::Reverse(n.timestamp_ms()));
        sorted_oldest_first
    }

    /// Collapse raw import ops with their normalized children into single nodes.
    ///
    /// Each raw `Import` op is the linear backbone of a session; its normalized
    /// children (`Message`, `Tool`, `Command`, `File`) branch off it. This folds
    /// each raw op + its children into one [`HistoryNode::CollapsedImport`] whose
    /// summary is derived from the children's content, so the graph shows one
    /// meaningful node per source line instead of a dense star. Non-import ops
    /// (e.g. `ChainStart`, git-link records) are kept as-is.
    #[must_use]
    fn collapsed_ops(&self) -> Vec<HistoryNode> {
        // Set of raw import op ids (the linear backbone).
        let import_ids: std::collections::HashSet<OpId> = self
            .ops
            .iter()
            .filter(|op| matches!(op.kind, editchain_core::OpKind::Import(_)))
            .map(|op| op.id)
            .collect();
        // Map raw import op id -> its normalized children (in input order).
        let mut children_of: HashMap<OpId, Vec<&Op>> = HashMap::new();
        // Track which non-import ops are folded into an import parent (so they
        // are dropped), versus standalone ops that must be kept.
        let mut folded: std::collections::HashSet<OpId> = std::collections::HashSet::new();
        for op in &self.ops {
            if matches!(op.kind, editchain_core::OpKind::Import(_)) {
                continue;
            }
            for &parent in &op.parents {
                if import_ids.contains(&parent) {
                    let _: bool = folded.insert(op.id);
                    children_of.entry(parent).or_default().push(op);
                }
            }
        }

        self.ops
            .iter()
            .filter_map(|op| {
                if matches!(op.kind, editchain_core::OpKind::Import(_)) {
                    let children = children_of.get(&op.id);
                    let summary = collapsed_import_summary(op, children);
                    let kind = collapsed_import_kind(children);
                    let author = collapsed_import_author(children);
                    Some(HistoryNode::CollapsedImport {
                        op: op.clone(),
                        summary,
                        kind,
                        author,
                    })
                } else if folded.contains(&op.id) {
                    // Drop normalized ops folded into their parent import op.
                    None
                } else {
                    // Standalone op (e.g. ChainStart, or a message not tied to an
                    // import) — keep as-is.
                    Some(HistoryNode::EditOperation(op.clone()))
                }
            })
            .collect()
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
        let is_git =
            |key: &str| -> bool { key_to_node.get(key).is_some_and(|n| n.git_oid().is_some()) };
        compute_graph_layout(&keys, parents_of, &is_git)
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
        let is_git =
            |key: &str| -> bool { key_to_node.get(key).is_some_and(|n| n.git_oid().is_some()) };
        compute_lane_assignment(&keys, &parents_of, &is_git)
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
        let is_git =
            |key: &str| -> bool { key_to_node.get(key).is_some_and(|n| n.git_oid().is_some()) };
        layout::LayoutContext::new(&keys, &parents_of, &is_git)
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

/// Derive a display summary for a collapsed import op from its normalized
/// children.
///
/// Prefers the most meaningful content: a message's text, then a tool's name,
/// then a command's content. Falls back to the raw import reference when there
/// are no children (e.g. structural lines like `custom-title`).
#[must_use]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only message/tool/command children contribute to the summary; all other kinds are ignored"
)]
fn collapsed_import_summary(op: &Op, children: Option<&Vec<&Op>>) -> String {
    use editchain_core::OpKind;
    let mut message = String::new();
    let mut tool = String::new();
    let mut command = String::new();
    if let Some(children) = children {
        for child in children {
            match &child.kind {
                OpKind::Message(m) if message.is_empty() => {
                    message = payload_text(&m.content);
                }
                OpKind::Tool(t) if tool.is_empty() => {
                    tool = payload_text(&t.tool_name);
                }
                OpKind::Command(c) if command.is_empty() => {
                    command = payload_text(&c.content);
                }
                _ => {}
            }
        }
    }
    if !message.is_empty() {
        return message;
    }
    if !tool.is_empty() {
        return format!("tool: {tool}");
    }
    if !command.is_empty() {
        return format!("$ {command}");
    }
    // No meaningful children — fall back to the raw reference.
    match &op.kind {
        OpKind::Import(i) => payload_text(&i.raw_ref),
        OpKind::ChainStart(cs) => String::from_utf8_lossy(&cs.name).to_string(),
        OpKind::Actor(a) => payload_text(&a.label),
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.tool_name),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::File(f) => format!("file:{}", f.path.0),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::GitCommit(c) => payload_text(&c.message),
        OpKind::GitLink(l) => format!("git:{}", l.target_oid),
        OpKind::Unknown(u) => format!("unknown kind={}", u.kind_discriminant),
    }
}

/// Determine the dominant child kind for a collapsed import op.
///
/// Prefers message, then tool, then command — matching the summary derivation.
/// Falls back to `"import"` when there are no meaningful children.
#[must_use]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only message/tool/command children determine the dominant kind; all other kinds fall through"
)]
fn collapsed_import_kind(children: Option<&Vec<&Op>>) -> String {
    use editchain_core::OpKind;
    if let Some(children) = children {
        for child in children {
            match &child.kind {
                OpKind::Message(_) => return "message".to_string(),
                OpKind::Tool(_) => return "tool".to_string(),
                OpKind::Command(_) => return "command".to_string(),
                _ => {}
            }
        }
    }
    "import".to_string()
}

/// Determine the author label for a collapsed import op from its children's
/// tags.
///
/// The raw import op's tags only carry `IMPORT`; the role (`HUMAN` / `AGENT`)
/// lives on the normalized children. Prefers `human`, then `agent`, and falls
/// back to `system` when no child carries a role tag.
#[must_use]
fn collapsed_import_author(children: Option<&Vec<&Op>>) -> String {
    use editchain_core::Tags;
    if let Some(children) = children {
        for child in children {
            if child.tags.matches_any(Tags::HUMAN) {
                return "human".to_string();
            }
        }
        for child in children {
            if child.tags.matches_any(Tags::AGENT) {
                return "agent".to_string();
            }
        }
    }
    "system".to_string()
}

/// Extract text from a payload, or empty string.
#[must_use]
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}
