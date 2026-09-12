//! UI-neutral history projections for the unified `EditChain` + `Git` viewer.
//!
//! This crate builds deterministic projections over `EditChain` operations and
//! `Git` commits, and provides windowed/paged access for the viewer. It is
//! intentionally free of filesystem and process dependencies so it can later
//! target WASM.

use serde as _;

pub mod activity;
pub mod activity_view;
pub mod content;
pub mod git;
/// Human work semantics shared by historical and live projections.
pub mod human;
/// Deterministic lane layout for graph rendering.
pub mod layout;
/// Mutable logical-item projection for the live activity view.
pub mod live;
/// Deterministic semantic metadata for projected history rows.
pub mod meta;
/// Provider-neutral readability taxonomy shared with the protocol layer.
pub mod taxonomy;

mod view;

mod graph;
mod materialization;
mod provider;
pub use git::GitProjection;
pub use graph::{NodeKey, ResolvedGraph, ResolvedRelation};
pub use materialization::CodexLogicalItem;

mod ancestry;
mod collapse;
mod labels;
mod node;

pub use ancestry::{ParentRelation, RelationKind};
pub use labels::import_token_accounting_summary;
pub use node::{EffectiveTime, HistoryNode};

use crate::layout::{GraphLayout, GraphRow};
use ancestry::{canonical_op_id, is_projected_relation_fact, ordering_parent_keys};
use editchain_core::{GitCommitEntity, Op, OpId};
use std::collections::HashMap;
use std::sync::Arc;

/// A unified history projection over `EditChain` ops and `Git` commits.
#[derive(Debug, Clone, Default)]
pub struct HistoryProjection {
    /// `EditChain` operations in canonical causal order (oldest-first).
    ops: Vec<Op>,
    /// Selected occurrence revisions and their recomputed logical item state.
    materialization: materialization::Materialization,
    /// `Git` commits keyed by `(RepositoryId, GitOid)`.
    git: GitProjection,
    /// Typed relationship facts keyed by the raw occurrence they annotate, as
    /// stored in `Op.parents`. This is the construction-time index: collapse
    /// reads it to resolve provider entities and canonicalize endpoints
    /// exposed by [`Self::relationship_notes`]. Keeping the raw index here means
    /// collapse-time logic never needs the canonical map before it exists.
    relationship_notes: HashMap<OpId, Vec<Op>>,
    /// Cached collapsed (top-level-row) projection with its canonical
    /// representative map and canonicalized relationship notes. Computed once at
    /// construction so every per-row path (`ordered_nodes`, `independent_chains`,
    /// `lifted_parent_keys`, layout, Activity view, windowed edges) reads a stable
    /// canonical view without rebuilding it per row. The collapse is ~linear in
    /// op count and cheap relative to the per-row consumers that reuse it.
    collapsed_projection: Arc<CollapsedProjection>,
}

/// Result of collapsing raw imports into top-level history rows.
///
/// Alongside the rows carries the reversible bundle membership maps so layout/view
/// code can preserve chain continuity when a child's parent is a bundled META op — without
/// ever rewriting stored `Op.parents`.
///
/// The central invariant of the semantic collapse: **every source operation that
/// participates in a collapsed bundle resolves deterministically to a visible
/// projected row**. `representative` maps each op that does not render as its own
/// row (a normalized child folded into its raw import parent, a bundled META
/// sub-op, a tool result folded into its call, an exact-payload copied
/// occurrence, or a relationship fact folded out of rendering) to the
/// op id of the visible row that represents it. Relationship anchors and targets
/// are canonicalized through this map before any parent-key construction, lane
/// allocation, Activity-view shaping, or windowed edge geometry runs, so a folded endpoint can
/// never dangle or draw a phantom interval.
#[derive(Debug, Clone, Default)]
struct CollapsedProjection {
    /// Top-level rows (raw imports collapsed; META records bundled away).
    nodes: Vec<HistoryNode>,
    /// Canonical representative map: op id -> the op id of the visible row that
    /// represents it.
    ///
    /// Invariant: for every op in the projection's op set, either the op renders
    /// as its own row (its id is a key in `present`) or `representative` maps its
    /// id — possibly through a chain of representatives — to an op id that is a
    /// key in `present`. Covers normalized children folded into their raw import
    /// parent, META sub-ops bundled into an anchor, tool results folded into their
    /// call, equivalent copied provider occurrences, and relationship facts
    /// folded out of rendering (mapped to their anchor's visible row).
    representative: HashMap<OpId, OpId>,
    /// Structural relationship notes re-keyed for edge drawing: keyed by the
    /// CANONICAL visible anchor (the representative of the note's stored causal
    /// parent), so a note whose anchor was folded into a bundle is still reachable
    /// from the visible row that represents it. A metadata anchor folded into
    /// its own structural target is also indexed on its unique visible causal
    /// successor, preserving the relation kind on that branch row. Provider
    /// entity targets are resolved first to a unique same-source occurrence (or
    /// one globally unique exact-equivalence class); ambiguous targets are
    /// removed. Every
    /// edge-construction path then lifts folded physical targets through
    /// `representative` via [`ancestry::canonicalize_parents`], so a virtual edge never
    /// reaches lane allocation or windowed edge geometry with a phantom key.
    /// Built once per collapse so per-row paths (layout/view/order) don't
    /// re-derive it.
    canonical_notes: HashMap<OpId, Vec<Op>>,
    /// Precomputed `node_key` set of every top-level row (the "present" rows
    /// used to decide whether a lifted/raw parent resolves to a rendered row).
    /// Built once here so per-row paths (lift/layout) don't rebuild it each call.
    present: std::collections::HashSet<String>,
    /// Inert relationship evidence with no resolvable projected anchor.
    unresolved_relations: std::collections::HashSet<OpId>,
}

impl HistoryProjection {
    /// Create an empty projection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ops: Vec::new(),
            materialization: materialization::Materialization::default(),
            git: GitProjection::new(),
            relationship_notes: HashMap::new(),
            collapsed_projection: Arc::default(),
        }
    }

    /// Build a projection from a set of operations.
    ///
    /// Operations are stored in input order; git commits are projected into
    /// the `GitProjection` keyed by `(RepositoryId, GitOid)`. Structural
    /// relationship notes are indexed by their causal parent for later use as
    /// virtual graph edges.
    #[must_use]
    pub fn from_ops(ops: Vec<Op>) -> Self {
        Self::from_preview_ops(ops, &std::collections::HashSet::new())
    }

    /// Build from display payloads, recording operations that were shortened or
    /// unavailable before projection. Source operation identities are unchanged.
    #[must_use]
    pub fn from_preview_ops(ops: Vec<Op>, incomplete: &std::collections::HashSet<OpId>) -> Self {
        let messages = materialization::source_messages(&ops, incomplete);
        let materialization = materialization::Materialization::from_ops(&ops, &messages);
        Self::from_previews(ops, incomplete, materialization)
    }

    /// Build bounded display rows while retaining exact source message payload
    /// identities and derivation evidence for echo comparison. Equal shortened
    /// previews never prove equality; durable blob references or complete inline
    /// sources do. Evidence must also remain complete to validate source coverage.
    #[must_use]
    pub fn from_source_previews(
        sources: &[Op],
        previews: Vec<Op>,
        incomplete: &std::collections::HashSet<OpId>,
    ) -> Self {
        let messages = materialization::source_messages(sources, &std::collections::HashSet::new());
        let materialization = materialization::Materialization::from_ops(sources, &messages);
        Self::from_previews(previews, incomplete, materialization)
    }

    fn from_previews(
        ops: Vec<Op>,
        incomplete: &std::collections::HashSet<OpId>,
        materialization: materialization::Materialization,
    ) -> Self {
        let provider_relations = provider::resolve(&ops);
        let mut git = GitProjection::new();
        let mut relationship_notes: HashMap<OpId, Vec<Op>> = HashMap::new();
        for op in &ops {
            git.reduce(op);
            if is_projected_relation_fact(op) && !provider_relations.replaces_legacy_note(op) {
                if let Some(parent) = op.parents.iter().next() {
                    relationship_notes
                        .entry(*parent)
                        .or_default()
                        .push(op.clone());
                }
            }
        }
        for note in provider_relations.notes {
            if let Some(parent) = note.parents.iter().next() {
                relationship_notes.entry(*parent).or_default().push(note);
            }
        }
        let mut projection = Self {
            ops,
            materialization,
            git,
            relationship_notes,
            collapsed_projection: Arc::default(),
        };
        // Build the canonical collapse eagerly so `relationship_notes` and every
        // layout/view/order path see a stable canonical view from the start
        // and reused by every row/layout path.
        projection.collapsed_projection = Arc::new(projection.collapsed_ops(incomplete));
        projection
    }

    /// Accepted source operations, immutable for this projection.
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Current Codex logical items after replaying admitted upserts and removals.
    /// Historical revisions remain available through [`Self::ops`] and the view.
    #[must_use]
    pub fn codex_logical_items(&self) -> &[CodexLogicalItem] {
        &self.materialization.items
    }

    /// Presentation identity retained when an immutable provider occurrence is revised.
    #[must_use]
    pub fn continuity_key(&self, id: OpId) -> Option<&str> {
        self.materialization
            .continuity_keys
            .get(&id)
            .map(String::as_str)
    }

    /// Observed Git facts. New commits enter through `merge_git_commits`.
    #[must_use]
    pub const fn git(&self) -> &GitProjection {
        &self.git
    }

    /// Returns the structural relationship notes re-keyed for edge drawing:
    /// keyed by the CANONICAL visible anchor (the representative of the note's
    /// stored causal parent), plus a unique visible successor when metadata
    /// contraction would otherwise collapse a structural relation into its own
    /// target. Provider entity targets have been resolved to an unambiguous
    /// physical occurrence; direct physical targets remain stored as supplied.
    /// Used by [`HistoryNode::parent_keys`] so virtual
    /// fork/subagent/reconnect edges are reachable from rendered rows even when
    /// their source ops were folded into a collapsed bundle; targets are lifted
    /// to visible rows (or dropped) by every layout/view/order path through the
    /// canonical representative map.
    #[must_use]
    pub fn relationship_notes(&self) -> &HashMap<OpId, Vec<Op>> {
        &self.collapsed_projection.canonical_notes
    }

    /// Returns the number of history nodes (ops + git commits).
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len().saturating_add(self.git.commits().len())
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

    /// Returns Activity candidates before presentation grouping (newest-first).
    ///
    /// Timestamp-less metadata and semantic trace rows are removed, with their
    /// causal edges reconnected to the nearest visible ancestors. Dated
    /// structural relationship rows remain visible. The result keeps the same
    /// canonical order as [`Self::nodes`]. Advanced pass tests may use these
    /// candidates directly. Normal consumers should call [`Self::build_activity_view`]
    /// for complete grouping, expansion, and source-to-row ownership.
    #[must_use]
    pub fn activity_nodes(&self) -> Vec<HistoryNode> {
        self.activity_nodes_with_omissions().0
    }

    /// Resolve an operation id to the canonical op id of the visible top-level
    /// row that represents it in the full (unfiltered) projection.
    ///
    /// Follows the semantic-collapse representative map, so a normalized child
    /// folded into its raw import parent, a bundled META sub-op, a tool result
    /// folded into its call, an exact copied occurrence, or a relation fact all resolve
    /// to the row that renders them. Returns `None` when the id is neither a
    /// top-level row nor folded into one (for example a synthetic git-index op
    /// id, which is never a projection node). Find-in-chain callers use this to
    /// lift a search hit to its visible row, then check that row against the
    /// Activity snapshot so hidden hits are dropped.
    #[must_use]
    pub fn visible_op_id(&self, op_id: OpId) -> Option<OpId> {
        canonical_op_id(
            op_id,
            &self.collapsed_projection.representative,
            &self.collapsed_projection.present,
        )
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
        let mut nodes: Vec<HistoryNode> = self.collapsed_projection.nodes.clone();
        for commit in self.git.commits().values() {
            nodes.push(HistoryNode::GitCommit {
                commit: Box::new(commit.clone()),
                parent_override: None,
            });
        }
        let present = nodes.iter().map(HistoryNode::key).collect();
        nodes
            .iter()
            .filter(|node| {
                ordering_parent_keys(
                    node,
                    self.git.links(),
                    self.relationship_notes(),
                    &self.collapsed_projection.representative,
                    &present,
                )
                .is_empty()
            })
            .count()
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
    ///
    /// Ordering contract: every present parent appears BELOW its child (all
    /// drawn edges point downward), and among causally independent (eligible)
    /// nodes the list is newest-first by effective time so git commits and ops
    /// interleave chronologically. No global timestamp sort is applied after
    /// the schedule: a plain `Reverse(timestamp_ms)` stable sort can violate
    /// edges when causal clocks across sessions are inconsistent (a child can
    /// carry an older timestamp than its parent). Instead the schedule itself
    /// is topology-preserving chronological scheduling — Kahn's algorithm
    /// emits parents before children (oldest-first), at each step choosing the
    /// eligible node with the smallest effective time (ties broken by input
    /// order), and the result is reversed so children render above parents
    /// while independent chains stay interleaved newest-first.
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "Scheduler indices originate from these equally sized node/adjacency vectors; in-degree increments/decrements are bounded by discovered edges"
    )]
    fn ordered_nodes(&self) -> Vec<HistoryNode> {
        // Build a unified node list: ops (newest-first) then git commits.
        // Raw import ops are collapsed with their normalized children into a
        // single node so the graph reads as a clean chain rather than a dense
        // star per source line.
        let mut nodes: Vec<HistoryNode> = Vec::with_capacity(self.len());
        for op in self.collapsed_projection.nodes.iter().rev() {
            nodes.push(op.clone());
        }
        for commit in self.git.commits().values() {
            nodes.push(HistoryNode::GitCommit {
                commit: Box::new(commit.clone()),
                parent_override: None,
            });
        }

        // Unknown source time stays unknown. Topology and source order place
        // those rows; timestamp is only a tie-break among currently eligible
        // nodes and never supplies ancestry or a borrowed session date.

        // Timestamp-prioritized Kahn scheduling, oldest-first. The acyclic path
        // is O(V + E + V log V); the deterministic malformed-cycle fallback may
        // additionally scan the remaining nodes for each cycle break.
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
        let keys: Vec<NodeKey> = nodes.iter().map(HistoryNode::key).collect();
        let present: std::collections::HashSet<NodeKey> = keys.iter().copied().collect();
        // Use typed Copy keys while scheduling, avoiding repeated OpId string
        // formatting, allocation, and hashing on the first large-chain window.
        let index_of: HashMap<NodeKey, usize> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| (*key, index))
            .collect();
        // Reverse adjacency (parent index -> child indices) and in-degree
        // (count of present parents still un-emitted).
        let mut children_of: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
        let mut indegree: Vec<usize> = vec![0; nodes.len()];
        for (child_index, node) in nodes.iter().enumerate() {
            // Resolve every parent to a canonical visible row: bundled-away META
            // ops, folded children, tool results, relationship facts, and copied
            // occurrences all lift to the row that represents them. Parents that
            // fail to resolve are dropped so a phantom can never block the sort.
            for parent in ordering_parent_keys(
                node,
                self.git.links(),
                self.relationship_notes(),
                &self.collapsed_projection.representative,
                &present,
            ) {
                if let Some(parent_index) = index_of.get(&parent).copied() {
                    children_of[parent_index].push(child_index);
                    indegree[child_index] += 1;
                }
            }
        }

        // Seed the schedule with nodes that have no present parents. The
        // priority queue is keyed by (effective time, input index): among
        // eligible nodes the OLDEST is emitted first (oldest-first topological
        // order), so after the final reversal the newest eligible node renders
        // at the top and independent chains interleave chronologically. Equal
        // timestamps break by input index — never HashMap iteration order — and
        // the final reversal also reverses that equal-time tie order. This makes
        // ties reproducible without claiming recency when their clocks are equal.
        let mut queue: std::collections::BinaryHeap<std::cmp::Reverse<(u64, usize)>> =
            std::collections::BinaryHeap::with_capacity(nodes.len());
        for (input_index, node) in nodes.iter().enumerate() {
            if indegree[input_index] == 0 {
                queue.push(std::cmp::Reverse((node.timestamp_ms(), input_index)));
            }
        }

        // Track which indices have not yet been emitted so we can break cycles
        // deterministically when Kahn stalls.
        let mut unemitted = vec![true; nodes.len()];
        let mut remaining = nodes.len();

        let mut sorted_oldest_first: Vec<usize> = Vec::with_capacity(nodes.len());
        while remaining > 0 {
            // Normal Kahn step: emit every node whose present parents have all
            // been emitted, oldest-eligible first (chronological interleave).
            while let Some(std::cmp::Reverse((_, index))) = queue.pop() {
                if !unemitted[index] {
                    continue;
                }
                sorted_oldest_first.push(index);
                unemitted[index] = false;
                remaining -= 1;
                for &child in &children_of[index] {
                    indegree[child] -= 1;
                    if indegree[child] == 0 && unemitted[child] {
                        queue.push(std::cmp::Reverse((nodes[child].timestamp_ms(), child)));
                    }
                }
            }

            // If nodes remain, we've hit a cycle. Break it deterministically by
            // emitting the remaining node with the smallest in-degree (fewest
            // still-blocking present parents), tie-broken by key. Its remaining
            // parents are treated as dropped (their edges simply won't draw),
            // which keeps every other edge pointing forward in the final order.
            if remaining > 0 {
                let pick = (0..nodes.len())
                    .filter(|&index| unemitted[index])
                    .min_by(|&a, &b| {
                        indegree[a]
                            .cmp(&indegree[b])
                            // Cycles are malformed and rare. Preserve the old
                            // display-string tie-break exactly on that fallback
                            // path without paying String allocation per node on
                            // every healthy schedule.
                            .then_with(|| nodes[a].node_key().cmp(&nodes[b].node_key()))
                    })
                    .unwrap_or(0);
                sorted_oldest_first.push(pick);
                unemitted[pick] = false;
                remaining -= 1;
                for &child in &children_of[pick] {
                    indegree[child] -= 1;
                    if indegree[child] == 0 && unemitted[child] {
                        queue.push(std::cmp::Reverse((nodes[child].timestamp_ms(), child)));
                    }
                }
            }
        }

        // Reverse to newest-first. Move each node out of its input slot so the
        // ordered result does not clone payloads a second or third time.
        sorted_oldest_first.reverse();
        let mut slots: Vec<Option<HistoryNode>> = nodes.into_iter().map(Some).collect();
        sorted_oldest_first
            .into_iter()
            .filter_map(|index| slots[index].take())
            .collect()
    }

    /// Merge resolved git commits into the projection.
    ///
    /// Commits are keyed by `(RepositoryId, GitOid)`; a later commit with the
    /// same key replaces an earlier one.
    pub fn merge_git_commits(&mut self, commits: Vec<GitCommitEntity>) {
        for commit in commits {
            self.git.observe_commit(commit);
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
        self.resolved_graph(sorted).layout()
    }

    /// Assign lanes for this exact ordered graph, without edge geometry.
    #[must_use]
    pub fn lane_assignment_filtered(&self, sorted: &[HistoryNode]) -> Vec<GraphRow> {
        self.resolved_graph(sorted).lane_assignment()
    }

    /// Build reusable geometry from the same finalized typed graph as rows.
    #[must_use]
    pub fn layout_context(&self, sorted: &[HistoryNode]) -> layout::LayoutContext {
        self.resolved_graph(sorted).layout_context()
    }

    /// Resolve a complete graph stage from canonical or derived nodes.
    ///
    /// All endpoints belong to `sorted`; geometry and structural labels share
    /// this table. Source operations and commits are never modified.
    #[must_use]
    pub fn resolved_graph(&self, sorted: &[HistoryNode]) -> ResolvedGraph {
        let keys: Vec<NodeKey> = sorted.iter().map(HistoryNode::key).collect();
        let present = keys.iter().copied().collect();
        let rows = sorted
            .iter()
            .map(|node| {
                let parents = ordering_parent_keys(
                    node,
                    self.git.links(),
                    self.relationship_notes(),
                    &self.collapsed_projection.representative,
                    &present,
                );
                let relations = self.resolved_relations_for(node, &parents);
                (
                    node.key(),
                    graph::ResolvedGraphRow {
                        parents,
                        relations,
                        chain_state: node.chain_state(),
                    },
                )
            })
            .collect();
        ResolvedGraph::new(keys, rows)
    }
}
