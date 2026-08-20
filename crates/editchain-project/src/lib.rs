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

use editchain_core::op::NoteRelationship;
use editchain_core::{
    Clock, GitCommitEntity, GitOid, GitProjection, NodeId, Op, OpId, Payload, RepositoryId,
};

use crate::layout::{compute_graph_layout, compute_lane_assignment, GraphLayout, GraphRow};
use crate::link::link_history;

/// Provenance of a node's effective display time.
///
/// q6 Phase-1: source time is nullable and immutable; the projection must never
/// fabricate a borrowed timestamp for a node whose source time is absent. Display
/// order may use `Observed` times; `BundleAnchor` never re-orders across chains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveTime {
    /// A real, valid source timestamp.
    Observed(u64),
    /// Inherited from a bundling anchor (metadata sub-op). Never re-sorts across chains.
    BundleAnchor(u64),
    /// The source carried no usable timestamp.
    Unknown,
}

/// A unified history row — either an `EditChain` operation or a `Git` commit.
#[derive(Debug, Clone)]
pub enum HistoryNode {
    /// An `EditChain` operation.
    EditOperation {
        /// The underlying operation.
        op: Op,
        /// Source time of this record; `Unknown` when the source had none.
        source_time: EffectiveTime,
    },
    /// A raw import op collapsed with its normalized children into one node.
    ///
    /// The raw import op forms the linear backbone of a session; its normalized
    /// children (messages, tools, commands) are folded into this single node so
    /// the graph reads as a clean chain rather than a dense star per line. The
    /// `summary` is derived from the children's content (not the raw JSONL).
    CollapsedImport {
        /// The underlying raw import op (kept for id/clock/parents).
        op: Op,
        /// Source time of this record; `Unknown` when the source had none.
        source_time: EffectiveTime,
        /// Display summary derived from the normalization children.
        summary: String,
        /// Dominant child kind (e.g. "tool", "message", "command") for styling.
        kind: String,
        /// Author label derived from the children's tags (`human` / `agent` /
        /// `system`). The raw import op's own tags only carry `IMPORT`, so the
        /// role must come from the normalized children that carry `HUMAN` /
        /// `AGENT`.
        author: String,
        /// Bundled metadata-only sub-ops attached to this node (revealed on
        /// click). These are raw Import ops tagged `META` that carry no
        /// user-facing content; they hang off this real turn/tool node rather
        /// than occupying their own graph row/lane.
        sub_ops: Vec<Op>,
    },
    /// A `Git` commit entity.
    GitCommit(GitCommitEntity),
}

impl HistoryNode {
    /// Returns a display summary for this node.
    ///
    /// For collapsed imports, the summary is the row's own content combined with
    /// the content of any bundled sub-ops (metadata records, tool results), so a
    /// row that would otherwise show `(no summary)` still carries meaningful
    /// text. The combined result is truncated to ~200 chars.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::EditOperation { op, .. } => op_summary(op),
            Self::CollapsedImport {
                summary, sub_ops, ..
            } => combined_summary(summary, sub_ops),
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
    ///
    /// This returns the *effective* display time (see [`Self::effective_time`]);
    /// a node whose source time is absent falls back to its bundle-anchor time,
    /// else 0. The stored `op.clock` is never mutated.
    #[must_use]
    pub fn timestamp_ms(&self) -> u64 {
        match self.effective_time() {
            EffectiveTime::Observed(ms) | EffectiveTime::BundleAnchor(ms) => ms,
            EffectiveTime::Unknown => 0,
        }
    }

    /// Returns the effective display time with provenance.
    ///
    /// `Observed` is a real source timestamp; `Unknown` means the source had no
    /// usable time (the projection must not fabricate one); `BundleAnchor` is an
    /// inherited time on a bundled sub-op and never re-orders across chains.
    #[must_use]
    pub fn effective_time(&self) -> EffectiveTime {
        match self {
            Self::EditOperation { source_time, .. } | Self::CollapsedImport { source_time, .. } => {
                *source_time
            }
            Self::GitCommit(commit) => {
                let secs = u64::try_from(commit.committed_at).unwrap_or(0);
                EffectiveTime::Observed(secs.saturating_mul(1000))
            }
        }
    }

    /// Set an effective timestamp (Unix ms) on a node that lacks one.
    ///
    /// Used to give timestamp-less nodes the date of the first dated node that
    /// follows them, so they interleave correctly instead of clustering at the
    /// top of the history (which would otherwise inflate the lane count). For
    /// ops this sets the clock; for git commits it sets `committed_at` (seconds).
    /// Set a display-only `BundleAnchor` time on an undated node.
    ///
    /// Does **not** mutate the stored `op.clock` — provenance only, so the raw op
    /// never carries a fabricated timestamp (DR invariant 8).
    pub fn set_bundle_anchor(&mut self, ms: u64) {
        match self {
            Self::EditOperation { source_time, .. } | Self::CollapsedImport { source_time, .. } => {
                *source_time = EffectiveTime::BundleAnchor(ms);
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
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => Some(op.id),
            Self::GitCommit(_) => None,
        }
    }

    /// Returns the git commit OID, if this is a git commit.
    #[must_use]
    pub fn git_oid(&self) -> Option<GitOid> {
        match self {
            Self::EditOperation { .. } | Self::CollapsedImport { .. } => None,
            Self::GitCommit(commit) => Some(commit.oid),
        }
    }

    /// Returns the repository, if this is a git commit.
    #[must_use]
    pub fn repository(&self) -> Option<RepositoryId> {
        match self {
            Self::EditOperation { .. } | Self::CollapsedImport { .. } => None,
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
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => match op.scope {
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
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => op.id.to_string(),
            Self::GitCommit(commit) => commit.oid.to_hex(),
        }
    }

    /// Returns the parent node keys for drawing graph edges.
    ///
    /// For `EditChain` ops, this includes both the causal `Op.parents`, any
    /// explicit git links (whose target OID hex becomes a parent key, so the
    /// graph draws an edge from the op to that commit), and — when `notes`
    /// annotates this op as the causal parent of a structural relationship
    /// note — the note's target as a *virtual* parent. Virtual parents let
    /// fork/subagent branches render without mutating stored causality (SPEC
    /// §1.1, §5). `notes` maps a causal parent op id to the structural notes
    /// that annotate it.
    #[must_use]
    pub fn parent_keys(
        &self,
        git_links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
        notes: &HashMap<OpId, Vec<Op>>,
    ) -> Vec<String> {
        match self {
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => {
                let mut keys: Vec<String> = op.parents.iter().map(ToString::to_string).collect();
                if let Some(links) = git_links.get(&op.id) {
                    for link in links {
                        keys.push(link.target_oid.to_hex());
                    }
                }
                // Virtual parents: this op is the causal parent a structural
                // note annotates, so the note's target becomes a parent edge.
                if let Some(notes) = notes.get(&op.id) {
                    for note in notes {
                        if let editchain_core::OpKind::Note(n) = &note.kind {
                            keys.extend(n.target_ids.iter().map(ToString::to_string));
                        }
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
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => {
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

    /// Returns the bundled metadata sub-ops attached to this node (empty for
    /// nodes without any). These are raw `Import` ops tagged `META` that carry
    /// no user-facing content; the viewer reveals them on click.
    #[must_use]
    pub fn sub_ops(&self) -> &[Op] {
        match self {
            Self::CollapsedImport { sub_ops, .. } => sub_ops,
            Self::EditOperation { .. } | Self::GitCommit(_) => &[],
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
            Self::EditOperation { op, .. } => match &op.kind {
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
    /// Structural relationship notes (`ForkOf`, `SubagentOf`, `ReconnectsTo`)
    /// keyed by the causal parent they annotate. The layout reads these as
    /// virtual edges so fork/subagent branches render without mutating stored
    /// `Op.parents` (SPEC §1.1, §5).
    relationship_notes: HashMap<OpId, Vec<Op>>,
    /// Explicit projection options (bundling policy, etc.). Threaded through so
    /// projection behavior is deterministic and a real cache key — never global.
    options: ProjectionOptions,
    /// Cached collapsed (top-level-row) projection. Computed lazily because it is
    /// expensive (~linear in op count) and called from several per-row paths
    /// (`ordered_nodes`, `independent_chains`, `lifted_parent_keys`, layout).
    /// Recomputed and re-cached on the few mutation points (`link_history`). A
    /// `CollapsedProjection` is cheaper to clone than to rebuild, so per-node
    /// callers can `clone()` it instead of recomputing. `RefCell` lets the
    /// memoized `collapsed()` accessor populate it from `&self` without forcing
    /// every caller to be `&mut self`. An all-empty `Default` value marks "not
    /// yet built"; a real chain always has at least one node.
    collapsed: std::cell::RefCell<CollapsedProjection>,
}

/// Result of collapsing raw imports into top-level history rows.
///
/// Alongside the rows carries the reversible bundle membership maps so layout/filter
/// can preserve chain continuity when a child's parent is a bundled META op — without
/// ever rewriting stored `Op.parents`.
#[derive(Debug, Clone, Default)]
struct CollapsedProjection {
    /// Top-level rows (raw imports collapsed; META records bundled away).
    nodes: Vec<HistoryNode>,
    /// Bundled META op id -> its anchor op id (lift a missing parent onto the row).
    representative: HashMap<OpId, OpId>,
    /// Precomputed `node_key` set of every top-level row (the "present" rows
    /// used to decide whether a lifted/raw parent resolves to a rendered row).
    /// Built once here so per-row paths (lift/layout) don't rebuild it each call.
    present: std::collections::HashSet<String>,
}

/// Explicit, versionable options controlling projection behavior.
///
/// q6 Phase-1: replaces the process-global `META_BUNDLE_ENABLED` toggle. Bundling
/// stays OFF by default; when enabled it is scoped per source chain and never
/// touches stored `Op.parents`/clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProjectionOptions {
    /// Whether metadata-only raw imports bundle as sub-ops of their own source
    /// chain's nearest eligible dated content row. Default `false`.
    pub bundle_metadata: bool,
}

impl HistoryProjection {
    /// Create an empty projection with default options.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ops: Vec::new(),
            git: GitProjection::new(),
            relationship_notes: HashMap::new(),
            options: ProjectionOptions::default(),
            collapsed: std::cell::RefCell::new(CollapsedProjection::default()),
        }
    }

    /// Build a projection from a set of operations with default options.
    ///
    /// Operations are stored in input order; git commits are projected into
    /// the `GitProjection` keyed by `(RepositoryId, GitOid)`. Structural
    /// relationship notes are indexed by their causal parent for later use as
    /// virtual graph edges.
    #[must_use]
    pub fn from_ops(ops: Vec<Op>) -> Self {
        Self::from_ops_with(ops, ProjectionOptions::default())
    }

    /// Build a projection from a set of operations with explicit options.
    ///
    /// Options are a cache key: two projections built from the same ops with
    /// different options may render differently but only per that option.
    #[must_use]
    pub fn from_ops_with(ops: Vec<Op>, options: ProjectionOptions) -> Self {
        let mut git = GitProjection::new();
        let mut relationship_notes: HashMap<OpId, Vec<Op>> = HashMap::new();
        for op in &ops {
            git.reduce(op);
            if is_structural_note(op) {
                if let Some(parent) = op.parents.iter().next() {
                    relationship_notes
                        .entry(*parent)
                        .or_default()
                        .push(op.clone());
                }
            }
        }
        Self {
            ops,
            git,
            relationship_notes,
            options,
            collapsed: std::cell::RefCell::new(CollapsedProjection::default()),
        }
    }

    /// Returns the structural relationship notes indexed by the causal parent
    /// they annotate (used by [`HistoryNode::parent_keys`] to draw virtual
    /// fork/subagent edges). Borrowed by consumers that project rows directly.
    #[must_use]
    pub fn relationship_notes(&self) -> &HashMap<OpId, Vec<Op>> {
        &self.relationship_notes
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
        filter::apply(&nodes, &self.git.links, &self.relationship_notes, filter)
    }

    /// Returns the number of independent (disconnected) chains among the top-level
    /// rows, matching how the source Claude Code sessions/streams are expected to
    /// break down.
    ///
    /// IMPORTANT: a child whose stored parent is a bundled META op is counted as
    /// connected to the META op's anchor (same lift as [`Self::ordered_nodes`]), so
    /// bundling metadata does NOT fragment a source chain into extra roots.
    #[must_use]
    pub fn independent_chains(&self) -> usize {
        let guard = self.collapsed();
        let collapsed: CollapsedProjection = (*guard).clone();
        let mut nodes: Vec<HistoryNode> = collapsed.nodes;
        for commit in self.git.commits.values() {
            nodes.push(HistoryNode::GitCommit(commit.clone()));
        }
        let present: std::collections::HashSet<String> =
            nodes.iter().map(HistoryNode::node_key).collect();
        let roots: Vec<&HistoryNode> = nodes
            .iter()
            .filter(|node| {
                node.parent_keys(&self.git.links, &self.relationship_notes)
                    .into_iter()
                    .all(|parent| {
                        // A parent only makes this node non-root if it resolves to a
                        // present row (directly or via a bundled-META representative).
                        let resolved = OpId::from_display_str(&parent)
                            .and_then(|pid| collapsed.representative.get(&pid).copied())
                            .map_or_else(|| parent.clone(), |rep| rep.to_string());
                        !present.contains(&resolved)
                    })
            })
            .collect();
        roots.len()
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
        let guard = self.collapsed();
        let collapsed: CollapsedProjection = (*guard).clone();
        let mut nodes: Vec<HistoryNode> = Vec::with_capacity(self.len());
        for op in collapsed.nodes.into_iter().rev() {
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
        //
        // Chain continuity after META bundling: a child may have a causal parent
        // that is a bundled META op (not present in `nodes`). Resolve that parent
        // through `representative` to the absorbing anchor row, so the child stays
        // connected to its source chain instead of fragmenting into a new root.
        // This lifts the edge WITHOUT rewriting stored `Op.parents`.
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
            for parent in node.parent_keys(&self.git.links, &self.relationship_notes) {
                // Resolve a parent that was bundled away to its anchor row's id.
                let resolved = OpId::from_display_str(&parent)
                    .and_then(|pid| collapsed.representative.get(&pid).copied())
                    .map_or_else(|| parent.clone(), |rep| rep.to_string());
                if present.contains(&resolved) {
                    children_of.entry(resolved).or_default().push(key.clone());
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

        // Assign BLOCK-ORDER display anchors to nodes whose source time is unknown.
        // Undated nodes (metadata headers like `custom-title`, `mode`, and
        // `file-history-snapshot`) are anchored to their OWN session's dated
        // range, not to the global-newest date. The old global walk gave every
        // undated node the most-recently-seen dated op across ALL sessions, so an
        // old session's header (e.g. seed-d0's `custom-title`, a Jul 10 record)
        // inherited the newest corpus date (Aug 5) and floated to the top of the
        // timeline — visually mixing an old session into the newest cluster.
        //
        // q6 Phase-1 change: this records a `BundleAnchor` display time instead of
        // rewriting the stored `op.clock` (DR: never rewrite clocks; time stays
        // nullable and immutable with provenance). `BundleAnchor` participates in
        // display order but never re-orders across chains, and the raw op keeps no
        // fabricated timestamp. Sessions with no dated ops stay undated (`Unknown`).
        // Git commits and unscoped ops are not re-dated.
        let mut session_first_ts: HashMap<String, u64> = HashMap::new();
        for node in &sorted_oldest_first {
            if matches!(node.effective_time(), EffectiveTime::Unknown) || node.git_oid().is_some() {
                continue;
            }
            if node.group().starts_with("session:") {
                let entry = session_first_ts.entry(node.group()).or_insert(u64::MAX);
                *entry = (*entry).min(node.timestamp_ms());
            }
        }
        for node in &mut sorted_oldest_first {
            if !matches!(node.effective_time(), EffectiveTime::Unknown) {
                continue;
            }
            let group = node.group();
            if group.starts_with("session:")
                && session_first_ts.get(&group).copied().unwrap_or(0) != 0
            {
                let anchor = session_first_ts.get(&group).copied().unwrap_or(0);
                // Record as a BundleAnchor display time — clone is a display-only
                // provenance, not a clock mutation.
                node.set_bundle_anchor(anchor);
            }
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
    ///
    /// Metadata-only raw imports (tagged `META`) render as their own top-level
    /// nodes by default so unrelated chains never group together. When
    /// [`ProjectionOptions::bundle_metadata`] is enabled they are instead
    /// bundled as sub-ops of the nearest preceding real turn/tool node **within
    /// the same `(OpId.node, OpId.boot)` source chain** — never across sessions
    /// — attached to that node's `sub_ops` and dropped from the top-level list.
    /// Bundling never rewrites stored `Op.parents` or clocks; chain continuity
    /// through a bundled op is resolved by the representative map.
    #[must_use]
    fn collapsed_ops(&self) -> CollapsedProjection {
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

        // Build top-level nodes in input order, bundling META imports into the
        // nearest preceding real turn/tool row **within the same source chain**.
        //
        // The per-chain key is `(OpId.node, OpId.boot)` — the DR "immediate safe
        // milestone" (q6 §6). A single global cursor is the bug that cross-bundled
        // unrelated sessions; a chain-scoped cursor never does. Branch or
        // concurrent-writer ambiguity stays standalone until full execution/path
        // resolution (Phase 3).
        //
        // A META record bundles only if its own chain already has an anchor; it is
        // attached as a sub-op of that anchor and recorded in the membership map.
        // Storage `Op.parents` and `Clock` are NEVER rewritten (DR: bundling must
        // not splice parents or clocks). A child that points at a bundled META op
        // keeps pointing at it; layout resolves it through `bundle_representative`.
        let mut result: Vec<HistoryNode> = Vec::with_capacity(self.ops.len());
        // Per (node, boot) chain: index in `result` of the last eligible anchor.
        let mut anchors: HashMap<(NodeId, u32), usize> = HashMap::new();
        // Entity(op id) -> anchor OP ID (stable, not a row index) so layout can
        // lift edges whose parent is a bundled META op onto the absorbing row.
        let mut representative: HashMap<OpId, OpId> = HashMap::new();
        for op in &self.ops {
            // Structural relationship notes (ForkOf/SubagentOf/ReconnectsTo) are
            // pure edge bookkeeping — they never render as rows themselves, only
            // their virtual edges do. They remain addressable in the OpSet and
            // indexed in `relationship_notes`.
            if is_structural_note(op) {
                continue;
            }
            if matches!(op.kind, editchain_core::OpKind::Import(_)) {
                let chain = (op.id.node, op.id.boot);
                let is_meta = op.tags.matches_any(editchain_core::Tags::META);
                let is_structural = op.tags.matches_any(editchain_core::Tags::STRUCTURAL);
                // Only bundling metadata bundles; structural/diagnostic/unknown and
                // ordinary content records are their own rows.
                if is_meta && self.options.bundle_metadata {
                    // Bundle only into THIS chain's anchor, if one exists.
                    if let Some(idx) = anchors.get(&chain).copied() {
                        if let Some(HistoryNode::CollapsedImport {
                            op: anchor,
                            sub_ops,
                            ..
                        }) = result.get_mut(idx)
                        {
                            sub_ops.push(op.clone());
                            let _: Option<OpId> = representative.insert(op.id, anchor.id);
                            // Never an anchor itself (metadata is never an anchor).
                            continue;
                        }
                    }
                    // Leading metadata with no anchor in THIS chain: render
                    // standalone on its own chain (a same-chain preamble) — never
                    // forward-attached and never cross-chain. It is NOT an anchor.
                }
                let children = children_of.get(&op.id);
                let summary = collapsed_import_summary(op, children);
                let kind = collapsed_import_kind(children);
                let author = collapsed_import_author(children);
                // A META/structural record that falls through is standalone and is
                // NOT an anchor. Only a dated content-bearing import becomes one.
                let source_time = source_time_of(op);
                let is_anchor_eligible = !is_meta && !is_structural;
                let idx = result.len();
                result.push(HistoryNode::CollapsedImport {
                    op: op.clone(),
                    source_time,
                    summary,
                    kind,
                    author,
                    sub_ops: Vec::new(),
                });
                if is_anchor_eligible {
                    let _: Option<usize> = anchors.insert(chain, idx);
                }
            } else if folded.contains(&op.id) {
                // Drop normalized ops folded into their parent import op.
            } else {
                // Standalone op (e.g. ChainStart, or a message not tied to an
                // import) — keep as-is. It is a content/structural row and anchors
                // its chain for subsequent metadata.
                let source_time = source_time_of(op);
                let idx = result.len();
                result.push(HistoryNode::EditOperation {
                    op: op.clone(),
                    source_time,
                });
                let chain = (op.id.node, op.id.boot);
                let _: Option<usize> = anchors.insert(chain, idx);
            }
        }

        // Tool-grouping pass: fold each tool RESULT into its tool CALL's sub-ops
        // so a call + its result render as one row (the result revealed on click).
        //
        // A tool result is a CollapsedImport whose dominant Tool child is
        // `stage: Finish` with an empty name; its parent is the tool call's raw
        // import op. We attach the result's Tool op to the call's `sub_ops` and
        // drop the result from the top-level list, splicing its children to the
        // call (same technique as META bundling) so chain continuity holds.
        self.group_tool_results(&mut result, &children_of, &mut representative);

        // Fork-prologue fold: a session that FORKS off a trunk carries its own
        // copy of the shared prologue (the backlog before the divergence point).
        // A `ForkOf` note anchors the branch at the divergence boundary, so we
        // elide the branch's pre-boundary prologue from the rows — the shared
        // messages already render on the trunk. This is the "branch at the
        // divergence point, don't duplicate the root chain" requirement: the
        // branch node draws its edge off the trunk at the split, and the
        // duplicated prologue never appears twice.
        self.fold_fork_prologues(&mut result);

        CollapsedProjection {
            present: row_node_keys(&result),
            nodes: result,
            representative,
        }
    }

    /// Memoized collapsed projection.
    ///
    /// Computes `collapsed_ops()` once and reuses it across the many callers that
    /// otherwise rebuilt it per row/query. Callers may `clone()` the returned
    /// value only when they need owned rows; call sites that need just the lift
    /// maps or the "present" key set should take a reference (cheaper, no alloc).
    /// Invalidated by `link_history` (which edits `self.ops`).
    fn collapsed(&self) -> std::cell::Ref<'_, CollapsedProjection> {
        // Cheap check against an empty placeholder so we only rebuild when never
        // built (or after `link_history` invalidation). `nodes` is always
        // non-empty for a real chain; a `len()==0` placeholder marks "not built".
        if self.collapsed.borrow().nodes.is_empty() {
            *self.collapsed.borrow_mut() = self.collapsed_ops();
        }
        self.collapsed.borrow()
    }

    /// Fold each fork branch's pre-boundary prologue into the trunk it branches
    /// from, so shared messages never render twice.
    ///
    /// A `ForkOf` note has a causal parent `P` (the branch's op at the divergence
    /// boundary) and a target `T` (the trunk's op at that same boundary). The
    /// branch's own chain before `P` — the prologue — duplicates the trunk's
    /// earlier messages and must be elided. We drop those nodes from `result`,
    /// splice `P`'s causal parent onto `T`, and re-parent any surviving children
    /// of a dropped prologue node onto the boundary `P`, so chain continuity holds
    /// and the branch renders as a single edge off the trunk at the split.
    fn fold_fork_prologues(&self, result: &mut Vec<HistoryNode>) {
        // Collect (branch_boundary, trunk_boundary) pairs from ForkOf notes. The
        // branch boundary is the note's causal parent; the trunk boundary is its
        // targethare. The branch renders off the trunk at this split via the note's
        // virtual edge (see [`HistoryNode::parent_keys`]), so we need only ELIDE
        // the branch's duplicated prologue from the rows — no causal parent
        // mutation (SPEC §1.1).
        let mut boundary_pairs: Vec<(OpId, OpId)> = Vec::new();
        for notes in self.relationship_notes.values() {
            for note in notes {
                let editchain_core::OpKind::Note(n) = &note.kind else {
                    continue;
                };
                if n.relationship != NoteRelationship::ForkOf {
                    continue;
                }
                if let (Some(parent), Some(target)) =
                    (note.parents.iter().next(), n.target_ids.first())
                {
                    boundary_pairs.push((*parent, *target));
                }
            }
        }
        if boundary_pairs.is_empty() {
            return;
        }

        // Group ops by session scope, each as (op id, seq), so we can find, for each
        // branch boundary, the session it lives in and thus the prologue (all
        // lower-seq ops in that same session). We include every op kind (not just
        // imports) so the fold works uniformly over collapsed-import and
        // standalone message nodes in tests and real chains alike.
        let mut by_session: HashMap<u64, Vec<(OpId, u64)>> = HashMap::new();
        for op in &self.ops {
            if let editchain_core::ScopeRef::Session(sid) = op.scope {
                by_session
                    .entry(sid.0)
                    .or_default()
                    .push((op.id, op.id.seq));
            }
        }

        // Mark the prologue: for each branch boundary, every op in the same
        // session with a strictly smaller seq (the shared backlog before the
        // split) is a duplicated prologue node to elide.
        let mut prologue: std::collections::HashSet<OpId> = std::collections::HashSet::new();
        for (branch_boundary, _trunk_boundary) in &boundary_pairs {
            for vec in by_session.values() {
                if vec.iter().any(|(id, _)| id == branch_boundary) {
                    let boundary_seq = branch_boundary.seq;
                    for &(op_id, seq) in vec {
                        if op_id != *branch_boundary && seq < boundary_seq {
                            let _: bool = prologue.insert(op_id);
                        }
                    }
                    break;
                }
            }
        }
        if prologue.is_empty() {
            return;
        }

        // Drop the prologue nodes from the rendered rows. The branch's boundary
        // node and everything after it stay; their causal edge to the (now
        // removed) prologue simply won't draw, and the ForkOf virtual edge keeps
        // the branch attached to the trunk at the split.
        let mut drop_idx: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (i, n) in result.iter().enumerate() {
            if let Some(op_id) = n.op_id() {
                if prologue.contains(&op_id) {
                    let _: bool = drop_idx.insert(i);
                }
            }
        }
        let mut kept: Vec<HistoryNode> = Vec::with_capacity(result.len());
        for (i, n) in result.drain(..).enumerate() {
            if !drop_idx.contains(&i) {
                kept.push(n);
            }
        }
        *result = kept;
    }

    /// Fold tool-result nodes into their tool-call parents' sub-ops.
    ///
    /// Mutates `result` in place: tool-result nodes whose parent is a tool-call
    /// node are removed from the top-level list and their Tool op is appended to
    /// the call's `sub_ops`. Children of a dropped result are re-parented to the
    /// call so the chain stays continuous.
    fn group_tool_results(
        &self,
        result: &mut Vec<HistoryNode>,
        children_of: &HashMap<OpId, Vec<&Op>>,
        representative: &mut HashMap<OpId, OpId>,
    ) {
        // Map node key -> index in `result`.
        let mut index_of: HashMap<String, usize> = HashMap::with_capacity(result.len());
        for (i, n) in result.iter().enumerate() {
            let _: Option<usize> = index_of.insert(n.node_key(), i);
        }

        // Identify which nodes are tool results and which are tool calls.
        let mut is_result: Vec<bool> = Vec::with_capacity(result.len());
        for n in result.iter() {
            is_result.push(Self::node_is_tool_result(n, children_of));
        }

        // For each tool-result node, find its parent; if the parent is a tool
        // call, fold the result into it. Collect decisions first (no mutation of
        // `result` during iteration), then apply.
        let mut replacement: HashMap<OpId, OpId> = HashMap::new();
        let mut drop_idx: std::collections::HashSet<usize> = std::collections::HashSet::new();
        // parent index -> tool-result ops to attach as sub-ops.
        let mut attach: HashMap<usize, Vec<Op>> = HashMap::new();
        for (i, n) in result.iter().enumerate() {
            if !is_result.get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some(parent_key) = n
                .parent_keys(&self.git.links, &self.relationship_notes)
                .first()
                .cloned()
            else {
                continue;
            };
            let Some(&parent_idx) = index_of.get(&parent_key) else {
                continue;
            };
            let parent_is_call = !is_result.get(parent_idx).copied().unwrap_or(false)
                && result
                    .get(parent_idx)
                    .is_some_and(|pn| Self::node_is_tool_call(pn, children_of));
            if parent_is_call {
                if let Some(tool_op) = Self::tool_result_op(n, children_of) {
                    attach.entry(parent_idx).or_default().push(tool_op);
                }
                if let HistoryNode::CollapsedImport { op, .. } = n {
                    if let Some(parent_id) = OpId::from_display_str(&parent_key) {
                        let _: Option<OpId> = replacement.insert(op.id, parent_id);
                    }
                    let _: bool = drop_idx.insert(i);
                }
            }
        }

        // If any META op bundled into a tool-result row that is now being folded into
        // its call, re-point the representative to the call so the lift in
        // `ordered_nodes`/`independent_chains` still reaches a present row.
        for value in representative.values_mut() {
            if let Some(repl) = replacement.get(value) {
                *value = *repl;
            }
        }

        // Attach collected tool-result ops to their call's sub-ops.
        for (parent_idx, ops) in &attach {
            if let Some(HistoryNode::CollapsedImport { sub_ops, .. }) = result.get_mut(*parent_idx)
            {
                sub_ops.extend(ops.iter().cloned());
            }
        }

        // Remove dropped results and splice their children to the call.
        if !drop_idx.is_empty() {
            let mut kept: Vec<HistoryNode> = Vec::with_capacity(result.len());
            for (i, n) in result.drain(..).enumerate() {
                if drop_idx.contains(&i) {
                    continue;
                }
                kept.push(n);
            }
            *result = kept;
            for node in result.iter_mut() {
                let keys = node.parent_keys(&self.git.links, &self.relationship_notes);
                let mut spliced: Vec<String> = keys
                    .iter()
                    .map(|k| {
                        OpId::from_display_str(k)
                            .and_then(|id| replacement.get(&id).copied())
                            .map_or_else(|| k.clone(), |repl| repl.to_string())
                    })
                    .collect();
                spliced.sort_unstable();
                spliced.dedup();
                node.set_parent_keys(&spliced);
            }
        }
    }

    /// Whether a collapsed-import node is a tool RESULT (Finish stage, empty name).
    fn node_is_tool_result(node: &HistoryNode, children_of: &HashMap<OpId, Vec<&Op>>) -> bool {
        let HistoryNode::CollapsedImport { op, .. } = node else {
            return false;
        };
        Self::tool_child(op.id, children_of).is_some_and(|t| {
            matches!(t.stage, editchain_core::op::ToolStage::Finish)
                && payload_text(&t.tool_name).is_empty()
        })
    }

    /// Whether a collapsed-import node is a tool CALL (Start stage, named).
    fn node_is_tool_call(node: &HistoryNode, children_of: &HashMap<OpId, Vec<&Op>>) -> bool {
        let HistoryNode::CollapsedImport { op, .. } = node else {
            return false;
        };
        Self::tool_child(op.id, children_of).is_some_and(|t| {
            matches!(t.stage, editchain_core::op::ToolStage::Start)
                && !payload_text(&t.tool_name).is_empty()
        })
    }

    /// The first Tool child of a raw import op, if any.
    fn tool_child<'a>(
        import_id: OpId,
        children_of: &'a HashMap<OpId, Vec<&'a Op>>,
    ) -> Option<&'a editchain_core::op::ToolOp> {
        children_of.get(&import_id).and_then(|children| {
            children.iter().find_map(|c| match &c.kind {
                editchain_core::OpKind::Tool(t) => Some(t),
                editchain_core::OpKind::ChainStart(_)
                | editchain_core::OpKind::Actor(_)
                | editchain_core::OpKind::Message(_)
                | editchain_core::OpKind::Command(_)
                | editchain_core::OpKind::File(_)
                | editchain_core::OpKind::Reflection(_)
                | editchain_core::OpKind::Import(_)
                | editchain_core::OpKind::Note(_)
                | editchain_core::OpKind::Error(_)
                | editchain_core::OpKind::GitCommit(_)
                | editchain_core::OpKind::GitLink(_)
                | editchain_core::OpKind::Unknown(_) => None,
            })
        })
    }

    /// The normalized Tool op of a tool-result node (for attaching as a sub-op).
    fn tool_result_op(node: &HistoryNode, children_of: &HashMap<OpId, Vec<&Op>>) -> Option<Op> {
        let HistoryNode::CollapsedImport { op, .. } = node else {
            return None;
        };
        children_of.get(&op.id).and_then(|children| {
            children.iter().find_map(|c| match &c.kind {
                editchain_core::OpKind::Tool(_) => Some((*c).clone()),
                editchain_core::OpKind::ChainStart(_)
                | editchain_core::OpKind::Actor(_)
                | editchain_core::OpKind::Message(_)
                | editchain_core::OpKind::Command(_)
                | editchain_core::OpKind::File(_)
                | editchain_core::OpKind::Reflection(_)
                | editchain_core::OpKind::Import(_)
                | editchain_core::OpKind::Note(_)
                | editchain_core::OpKind::Error(_)
                | editchain_core::OpKind::GitCommit(_)
                | editchain_core::OpKind::GitLink(_)
                | editchain_core::OpKind::Unknown(_) => None,
            })
        })
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
        // `self.ops` changed (stitching) — the collapsed cache is now stale.
        self.collapsed = std::cell::RefCell::new(CollapsedProjection::default());
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
        let collapsed = self.collapsed();
        let present = row_node_keys(&collapsed.nodes);
        let representative = &collapsed.representative;
        let parents_of = |key: &str| -> Vec<String> {
            key_to_node.get(key).map_or(Vec::new(), |n| {
                lift_parents(
                    n.parent_keys(links, &self.relationship_notes),
                    representative,
                    &present,
                )
            })
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
            key_to_node.get(key).map_or(Vec::new(), |n| {
                n.parent_keys(links, &self.relationship_notes)
            })
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
        let collapsed = self.collapsed();
        let present = row_node_keys(&collapsed.nodes);
        let representative = &collapsed.representative;
        let parents_of = |key: &str| -> Vec<String> {
            key_to_node.get(key).map_or(Vec::new(), |n| {
                lift_parents(
                    n.parent_keys(links, &self.relationship_notes),
                    representative,
                    &present,
                )
            })
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

    /// Return this node's parent keys resolved through the META-bundle lift, so a
    /// parent that was bundled away is redirected to its present anchor row.
    ///
    /// Used by the service when emitting `HistoryRow::parents` so the client's
    /// chain assembly sees connected chains, not dangling bundled-META parents.
    pub fn lifted_parent_keys(&self, node: &HistoryNode) -> Vec<String> {
        let collapsed = self.collapsed();
        lift_parents(
            node.parent_keys(&self.git.links, &self.relationship_notes),
            &collapsed.representative,
            &collapsed.present,
        )
    }
}

/// Collect the node keys of collapsed top-level rows (the "present" set used to
/// decide whether a parent resolves to a rendered row).
fn row_node_keys(nodes: &[HistoryNode]) -> std::collections::HashSet<String> {
    nodes.iter().map(HistoryNode::node_key).collect()
}

/// Resolve each parent key through the META-bundle representative map: a parent
/// that was bundled away (a META sub-op not present as a row) is redirected to
/// its anchor row's op id. Parents that remain absent (external/unresolved) are
/// kept as-is — the caller drops them via row lookup. This never rewrites stored
/// `Op.parents`; it is a layout-time lift only.
fn lift_parents(
    parents: Vec<String>,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<String>,
) -> Vec<String> {
    parents
        .into_iter()
        .map(|parent| {
            let lifted = OpId::from_display_str(&parent)
                .and_then(|pid| representative.get(&pid).copied())
                .map(|rep| rep.to_string())
                .filter(|rep| present.contains(rep));
            lifted.unwrap_or(parent)
        })
        .collect()
}

/// Produce a short summary for an `EditChain` operation.
#[must_use]
fn op_summary(op: &Op) -> String {
    use editchain_core::OpKind;
    match &op.kind {
        OpKind::Message(m) => message_summary(&payload_text(&m.content)),
        // A tool_result (stage Finish, empty tool_name) carries its result in
        // `content`; show a pretty-printed, truncated preview of it rather than
        // the empty tool_name.
        OpKind::Tool(t)
            if matches!(t.stage, editchain_core::op::ToolStage::Finish)
                && payload_text(&t.tool_name).is_empty() =>
        {
            tool_result_summary(&payload_text(&t.content))
        }
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

/// Whether an op is a *structural* relationship note — one that drives a virtual
/// graph edge (fork/subagent branch or reconnect) rather than carrying prose.
///
/// Structural notes are indexed for edge drawing and folded out of rendered rows;
/// non-structural notes (`Explains`, `Corrects`, etc.) keep their display path.
fn is_structural_note(op: &Op) -> bool {
    matches!(
        &op.kind,
        editchain_core::OpKind::Note(n)
            if matches!(
                n.relationship,
                NoteRelationship::ForkOf
                    | NoteRelationship::SubagentOf
                    | NoteRelationship::ReconnectsTo
            )
    )
}

/// Produce a pretty-printed, truncated preview of a tool result's content.
///
/// Tool results are raw text (file contents, JSON, error messages). This:
/// 1. strips leading line-number prefixes (`N\t`) that Claude Code adds to
///    file reads;
/// 2. collapses to the first non-empty line;
/// 3. truncates to ~90 chars with an ellipsis.
///
/// The result is a single-line preview suitable for the main-pane summary cell.
#[must_use]
fn tool_result_summary(content: &str) -> String {
    const MAX: usize = 1024;
    // Strip leading `<digits>\t` line-number prefixes from every line so both
    // plain text and JSON blobs are readable.
    let stripped: String = content
        .lines()
        .map(|l| {
            // Strip a leading `<digits>\t` line-number prefix ONLY when the
            // digits are followed by a tab — otherwise a JSON line that happens
            // to start with a digit (e.g. an array element `5,`) would be mangled.
            let trimmed = l.trim_start();
            let after_digits = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
            after_digits.strip_prefix('\t').map_or(l, |rest| rest)
        })
        .collect::<Vec<_>>()
        .join("\n");
    // If the result is a JSON blob (e.g. a debugger status dump), pull a short
    // label from it instead of dumping the whole JSON.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&stripped) {
        if let Some(label) = json_status_label(&value) {
            return label;
        }
    }
    let mut line = stripped
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    if line.chars().count() > MAX {
        let mut cut = line.chars().take(MAX).collect::<String>();
        cut.push('…');
        line = cut;
    }
    line
}

/// Decode the effective source time of an op, distinguishing `Observed` from
/// `Unknown`.
///
/// A record is `Unknown` when either the importer tagged `SOURCE_TIME_UNKNOWN`
/// (absent/invalid source timestamp) or the op carries no usable clock value
/// (`Clock::None`, or `UnixMs(0)` — the legacy "undated" marker). The stored
/// `Clock` is never rewritten; this only records provenance.
fn source_time_of(op: &Op) -> EffectiveTime {
    let unknown = op
        .tags
        .matches_any(editchain_core::Tags::SOURCE_TIME_UNKNOWN)
        || matches!(op.clock, Clock::None)
        || op.clock.as_u64() == 0;
    if unknown {
        EffectiveTime::Unknown
    } else {
        EffectiveTime::Observed(op.clock.as_u64())
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
    // Whether the tool child is a result (Finish, empty name) — its summary is
    // the content preview, shown WITHOUT the `tool: ` prefix.
    let mut tool_is_result = false;
    let mut command = String::new();
    if let Some(children) = children {
        for child in children {
            match &child.kind {
                OpKind::Message(m) if message.is_empty() => {
                    message = message_summary(&payload_text(&m.content));
                }
                OpKind::Tool(t) if tool.is_empty() => {
                    // A tool_result (Finish, empty name) previews its content;
                    // a tool call shows its name.
                    if matches!(t.stage, editchain_core::op::ToolStage::Finish)
                        && payload_text(&t.tool_name).is_empty()
                    {
                        tool = tool_result_summary(&payload_text(&t.content));
                        tool_is_result = true;
                    } else {
                        tool = payload_text(&t.tool_name);
                    }
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
        return if tool_is_result {
            tool
        } else {
            format!("tool: {tool}")
        };
    }
    if !command.is_empty() {
        return format!("$ {command}");
    }
    // No meaningful children — fall back to a label derived from the raw record.
    match &op.kind {
        OpKind::Import(i) => raw_import_label(i),
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

/// Produce a meaningful display label for a raw import record.
///
/// The raw import's `raw_ref` is the original JSONL line. When it parses as
/// JSON, derive a human-readable label from the record's `type` and structured
/// fields (e.g. attachment filename, queued command prompt). Falls back to the
/// raw text when it isn't parseable JSON.
#[must_use]
fn raw_import_label(import: &editchain_core::op::ImportOp) -> String {
    let raw = match &import.raw_ref {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    };
    if raw.is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return raw;
    };
    let record_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match record_type {
        // Attachment records carry a structured `attachment` object.
        "attachment" => {
            let att = value.get("attachment");
            let att_type = att
                .and_then(|a| a.get("type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let filename = att
                .and_then(|a| a.get("filename"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("");
            let path = att
                .and_then(|a| a.get("path"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let prompt = att
                .and_then(|a| a.get("prompt"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            match att_type {
                "file"
                | "edited_text_file"
                | "opened_file_in_ide"
                | "already_read_file"
                | "selected_lines_in_ide"
                    if !filename.is_empty() =>
                {
                    format!("{att_type}: {filename}")
                }
                "directory" if !path.is_empty() => format!("directory: {path}"),
                "queued_command" if !prompt.is_empty() => {
                    format!("queued command: {}", truncate_line(prompt))
                }
                _ if !att_type.is_empty() => format!("attachment: {att_type}"),
                _ => "attachment".to_string(),
            }
        }
        // User records with nested content (e.g. debugger status JSON).
        "user" => {
            if let Some(text) = nested_user_text(&value) {
                // If the extracted text is itself a JSON blob (e.g. a debugger
                // session-status dump), pull a short label from it instead of
                // dumping the whole JSON.
                if let Ok(inner) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(label) = json_status_label(&inner) {
                        return label;
                    }
                }
                truncate_line(&text)
            } else {
                raw
            }
        }
        _ if !record_type.is_empty() => record_type.to_string(),
        _ => raw,
    }
}

/// Extract text from a user record's possibly-nested content blocks.
///
/// Some user records nest text under `message.content[].content[].text` (e.g.
/// debugger status messages). Flatten any `text` blocks found at any depth.
#[must_use]
fn nested_user_text(value: &serde_json::Value) -> Option<String> {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(serde_json::Value::as_str) {
                    if !text.trim().is_empty() {
                        out.push(text.to_string());
                    }
                }
                for val in map.values() {
                    walk(val, out);
                }
            }
            serde_json::Value::Array(arr) => {
                for val in arr {
                    walk(val, out);
                }
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    let mut texts = Vec::new();
    walk(value, &mut texts);
    texts.first().cloned()
}

/// Produce a short label for a JSON status blob (e.g. a debugger session dump).
///
/// Prefers a `configurationName` or `name` field; falls back to a compact
/// `{key: value, ...}` summary of the top-level fields. Returns `None` when the
/// value has no useful scalar fields.
#[must_use]
fn json_status_label(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    if let Some(name) = obj
        .get("configurationName")
        .or_else(|| obj.get("name"))
        .and_then(serde_json::Value::as_str)
    {
        if !name.is_empty() {
            return Some(truncate_line(name));
        }
    }
    // Fall back to a compact summary of scalar fields.
    let mut parts = Vec::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            if !s.is_empty() {
                parts.push(format!("{k}={}", truncate_line(s)));
            }
        } else if let Some(n) = v.as_i64() {
            parts.push(format!("{k}={n}"));
        } else if let Some(b) = v.as_bool() {
            parts.push(format!("{k}={b}"));
        }
        if parts.len() >= 3 {
            break;
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// Produce a display summary for a message's content.
///
/// If the content is itself a JSON blob (e.g. a debugger session-status dump),
/// pull a short label from it instead of dumping the whole JSON. Otherwise
/// truncate the plain text.
#[must_use]
fn message_summary(content: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(label) = json_status_label(&value) {
            return label;
        }
    }
    truncate_line(content)
}

/// Truncate a string to ~1024 chars with an ellipsis.
#[must_use]
fn truncate_line(s: &str) -> String {
    const MAX: usize = 1024;
    let trimmed = s.trim();
    if trimmed.chars().count() > MAX {
        let mut cut = trimmed.chars().take(MAX).collect::<String>();
        cut.push('…');
        cut
    } else {
        trimmed.to_string()
    }
}

/// Build a display summary for a collapsed import from its own content plus its
/// bundled sub-ops' content.
///
/// The row's own summary is combined with each sub-op's meaningful content
/// (tool-result previews, metadata labels), joined with spaces, and truncated to
/// ~1024 chars. If the row has no own content and no sub-op content, falls back
/// to `(no summary)`.
#[must_use]
fn combined_summary(row_summary: &str, sub_ops: &[Op]) -> String {
    const MAX: usize = 1024;
    let mut parts: Vec<String> = Vec::new();
    let own = row_summary.trim();
    if !own.is_empty() && own != "(no summary)" {
        parts.push(own.to_string());
    }
    for op in sub_ops {
        if let Some(content) = sub_op_content(op) {
            if !content.is_empty() {
                parts.push(content);
            }
        }
    }
    if parts.is_empty() {
        return "(no summary)".to_string();
    }
    let joined = parts.join(" ");
    if joined.chars().count() > MAX {
        let mut cut = joined.chars().take(MAX).collect::<String>();
        cut.push('…');
        cut
    } else {
        joined
    }
}

/// Extract meaningful display content from a bundled sub-op.
///
/// Tool-result sub-ops (Tool, Finish) contribute their content preview; metadata
/// Import sub-ops contribute a short label derived from the raw record. Returns
/// `None` for sub-ops with no useful text.
#[must_use]
fn sub_op_content(op: &Op) -> Option<String> {
    match &op.kind {
        editchain_core::OpKind::Tool(t)
            if matches!(t.stage, editchain_core::op::ToolStage::Finish) =>
        {
            let preview = tool_result_summary(&payload_text(&t.content));
            if preview.is_empty() {
                None
            } else {
                Some(preview)
            }
        }
        editchain_core::OpKind::Import(i) => {
            let label = raw_import_label(i);
            if label.is_empty() || label.starts_with('{') {
                None
            } else {
                Some(label)
            }
        }
        editchain_core::OpKind::ChainStart(_)
        | editchain_core::OpKind::Actor(_)
        | editchain_core::OpKind::Message(_)
        | editchain_core::OpKind::Tool(_)
        | editchain_core::OpKind::Command(_)
        | editchain_core::OpKind::File(_)
        | editchain_core::OpKind::Reflection(_)
        | editchain_core::OpKind::Note(_)
        | editchain_core::OpKind::Error(_)
        | editchain_core::OpKind::GitCommit(_)
        | editchain_core::OpKind::GitLink(_)
        | editchain_core::OpKind::Unknown(_) => None,
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
