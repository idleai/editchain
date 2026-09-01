//! UI-neutral history projections for the unified `EditChain` + `Git` viewer.
//!
//! This crate builds deterministic projections over `EditChain` operations and
//! `Git` commits, and provides windowed/paged access for the viewer. It is
//! intentionally free of filesystem and process dependencies so it can later
//! target WASM.

// Crate-level dependency markers (used by Cargo for feature resolution).
use regex as _;
use serde as _;

/// General chain filtering with truncation.
pub mod activity;
pub mod filter;
/// Deterministic lane layout for graph rendering.
pub mod layout;
/// Deterministic semantic metadata for projected history rows.
pub mod meta;
/// Provider-neutral readability taxonomy shared with the protocol layer.
pub mod taxonomy;

use std::collections::HashMap;
use std::sync::Arc;

use editchain_core::op::NoteRelationship;
use editchain_core::{
    Clock, GitCommitEntity, GitOid, GitProjection, NodeId, Op, OpId, Payload, RepositoryId,
};

use crate::layout::{compute_graph_layout, compute_lane_assignment, GraphLayout, GraphRow};
use crate::meta::NodeMeta;
use crate::taxonomy::{ActivityKind, Outcome, RecordRole, Visibility as RowVisibility};

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
        op: Arc<Op>,
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
        op: Arc<Op>,
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
        sub_ops: Vec<Arc<Op>>,
        /// Deterministic semantic readability metadata derived from the raw
        /// envelope and normalized children (see [`crate::meta`]).
        meta: NodeMeta,
    },
    /// A synthetic Activity-view summary node folding a maximal contiguous run
    /// of low-signal execute rows (see [`crate::activity::bundle_activity_execute_runs`]).
    ///
    /// The original rows are preserved as expandable members (backing
    /// [`HistoryNode::sub_ops`]), so the client's existing sub-op expansion and
    /// `sub_op_counts` virtualization work unchanged and every folded record
    /// stays retrievable/inspectable by its real op id. The node key is the
    /// newest member's op id, so the bundle contracts into the run's newest
    /// display slot and causal children keep pointing at a real present key.
    ExecuteBundle {
        /// The newest member's op (identity/time/group anchor).
        anchor: Arc<Op>,
        /// Effective display time of the anchor member.
        source_time: EffectiveTime,
        /// The original top-level member rows, newest-first (display order).
        /// Used to render faithful expandable sub-op labels.
        member_nodes: Vec<HistoryNode>,
        /// Flat expandable sub-op ops: every member's op followed by its own
        /// bundled metadata sub-ops, newest-first. Backs [`HistoryNode::sub_ops`]
        /// so expansion counts and paging indices work unchanged.
        members: Vec<Arc<Op>>,
        /// Synthetic display summary ("N tool steps" / "N commands").
        summary: String,
        /// Dominant member kind tag (e.g. "tool", "command").
        kind: String,
        /// Author label derived from the members (`human` / `agent` / `system`).
        author: String,
        /// Deterministic semantic metadata (Execute / Primary; outcome is
        /// `Success` only when every member is structured-successful, else
        /// conservatively `Unknown`).
        meta: NodeMeta,
    },
    /// A `Git` commit entity.
    GitCommit(Box<GitCommitEntity>),
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
            Self::ExecuteBundle { summary, .. } => summary.clone(),
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
            Self::ExecuteBundle { source_time, .. } => *source_time,
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
            Self::EditOperation { source_time, .. }
            | Self::CollapsedImport { source_time, .. }
            | Self::ExecuteBundle { source_time, .. } => {
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
            Self::ExecuteBundle { anchor, .. } => Some(anchor.id),
            Self::GitCommit(_) => None,
        }
    }

    /// Returns the git commit OID, if this is a git commit.
    #[must_use]
    pub fn git_oid(&self) -> Option<GitOid> {
        match self {
            Self::EditOperation { .. }
            | Self::CollapsedImport { .. }
            | Self::ExecuteBundle { .. } => None,
            Self::GitCommit(commit) => Some(commit.oid),
        }
    }

    /// Returns the repository, if this is a git commit.
    #[must_use]
    pub fn repository(&self) -> Option<RepositoryId> {
        match self {
            Self::EditOperation { .. }
            | Self::CollapsedImport { .. }
            | Self::ExecuteBundle { .. } => None,
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
            Self::ExecuteBundle { anchor, .. } => bundle_group(anchor),
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
            // The bundle contracts into its newest member's display slot, so its
            // key IS the anchor's real op id: causal children keep resolving.
            Self::ExecuteBundle { anchor, .. } => anchor.id.to_string(),
            Self::GitCommit(commit) => commit.oid.to_hex(),
        }
    }

    /// Returns the parent node keys for drawing graph edges.
    ///
    /// For `EditChain` ops, this includes the causal `Op.parents`, graph-bearing
    /// explicit git links (whose target OID hex becomes a parent key), and — when `notes`
    /// annotates this op as the causal parent of a structural relationship note
    /// — the note's target as a *virtual* parent. Git links are explicit stored
    /// relations; no timestamp/text inference is performed by this projection.
    /// Virtual parents let fork/subagent branches render without mutating stored
    /// causality (SPEC §1.1, §5).
    /// `notes` maps a causal parent op id to the structural notes that annotate
    /// it.
    ///
    /// A collapsed row also inherits graph-bearing links whose source is one of
    /// its bundled sub-ops. This preserves an explicit session-to-Git edge when
    /// its source record is folded into a visible semantic turn.
    ///
    /// Keys are deduplicated preserving first-occurrence order (stored causal
    /// parents, then git-link targets from the row and its sub-ops, then virtual
    /// note targets), so a target shared between any of the three sources is
    /// emitted exactly once. This keeps parent keys deterministic and
    /// duplicate-free even when a filtered clone has materialized a virtual
    /// target into its stored `Op.parents`.
    #[must_use]
    pub fn parent_keys(
        &self,
        git_links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
        notes: &HashMap<OpId, Vec<Op>>,
    ) -> Vec<String> {
        match self {
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => {
                let mut keys: Vec<String> = Vec::new();
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for parent in &op.parents {
                    let key = parent.to_string();
                    if seen.insert(key.clone()) {
                        keys.push(key);
                    }
                }
                for source in std::iter::once(op).chain(self.sub_ops()) {
                    if let Some(links) = git_links.get(&source.id) {
                        for link in links {
                            let key = link.target_oid.to_hex();
                            if seen.insert(key.clone()) {
                                keys.push(key);
                            }
                        }
                    }
                }
                // Virtual parents: this op is the causal parent a structural
                // note annotates, so the note's target becomes a parent edge.
                // Deduplicate against stored/git parents: the hide-undated
                // filter materializes virtual targets into the cloned op's
                // `Op.parents`, so a later read would otherwise repeat the same
                // edge even though the chain holds one note per child.
                if let Some(notes) = notes.get(&op.id) {
                    for note in notes {
                        if let editchain_core::OpKind::Note(n) = &note.kind {
                            for target in &n.target_ids {
                                let key = target.to_string();
                                if seen.insert(key.clone()) {
                                    keys.push(key);
                                }
                            }
                        }
                    }
                }
                keys
            }
            Self::ExecuteBundle {
                member_nodes,
                members,
                ..
            } => {
                // The bundle contracts a whole run, so it inherits the union of
                // every member's EXTERNAL parent keys (stored causal parents,
                // git-link targets, and virtual note targets, each already
                // deduplicated per member). Intra-run member keys are dropped:
                // those slots no longer render. In a linear chain the anchor's
                // own parents are intra-run, so this union — not just the
                // anchor's parents — is what keeps the run's incoming edges.
                let member_keys: std::collections::HashSet<String> =
                    members.iter().map(|m| m.id.to_string()).collect();
                let mut keys: Vec<String> = Vec::new();
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for member in member_nodes {
                    for parent in member.parent_keys(git_links, notes) {
                        if member_keys.contains(&parent) {
                            continue;
                        }
                        if seen.insert(parent.clone()) {
                            keys.push(parent);
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
                Arc::make_mut(op).parents = match ids.len() {
                    0 => editchain_core::parents::ParentSet::None,
                    1 => editchain_core::parents::ParentSet::One(ids[0]),
                    _ => editchain_core::parents::ParentSet::Two(ids[0], ids[1]),
                };
            }
            Self::ExecuteBundle { anchor, .. } => {
                let mut ids: Vec<OpId> = keys
                    .iter()
                    .filter_map(|k| OpId::from_display_str(k))
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                Arc::make_mut(anchor).parents = match ids.len() {
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
    pub fn sub_ops(&self) -> &[Arc<Op>] {
        match self {
            Self::CollapsedImport { sub_ops, .. } => sub_ops,
            Self::ExecuteBundle { members, .. } => members,
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
            Self::CollapsedImport { kind, .. } | Self::ExecuteBundle { kind, .. } => kind.clone(),
            Self::GitCommit(_) => "git".to_string(),
        }
    }

    /// Returns the deterministic semantic readability metadata for this row.
    ///
    /// Collapsed imports carry metadata derived at collapse time from their raw
    /// envelope and normalized children; standalone ops and git commits derive
    /// it on demand from their envelope/scope.
    #[must_use]
    pub fn record_meta(&self) -> NodeMeta {
        match self {
            Self::CollapsedImport { meta, .. } | Self::ExecuteBundle { meta, .. } => *meta,
            Self::EditOperation { op, .. } => meta::for_edit_operation(op),
            Self::GitCommit(_) => meta::for_git_commit(),
        }
    }

    /// The provider-neutral record role of this row.
    #[must_use]
    pub fn record_role(&self) -> RecordRole {
        self.record_meta().record_role
    }

    /// The provider-neutral activity kind of this row.
    #[must_use]
    pub fn activity_kind(&self) -> ActivityKind {
        self.record_meta().activity_kind
    }

    /// The render prominence of this row (`Trace` rows are hidden by
    /// `hide_trace` chain filtering).
    #[must_use]
    pub fn visibility(&self) -> RowVisibility {
        self.record_meta().visibility
    }

    /// The concluded outcome of this row, when structured evidence exists.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        self.record_meta().outcome
    }

    /// The owning turn identity of this row, if turn-scoped.
    #[must_use]
    pub fn turn_id(&self) -> Option<editchain_core::TurnId> {
        self.record_meta().turn_id
    }
}

/// Grouping key for an execute-run bundle: the anchor op's session scope
/// (same rule as `EditOperation`/`CollapsedImport` rows, so a bundle never
/// crosses a group boundary).
#[must_use]
fn bundle_group(anchor: &Op) -> String {
    match anchor.scope {
        editchain_core::ScopeRef::Session(sid) => format!("session:{}", sid.0),
        editchain_core::ScopeRef::None
        | editchain_core::ScopeRef::Chain(_)
        | editchain_core::ScopeRef::Turn(_)
        | editchain_core::ScopeRef::File(_) => "ops".to_string(),
    }
}

/// A fork-branch source chain key: `(OpId.node, OpId.boot)`. Fork-prologue
/// detection is scoped to the branch's exact chain so rows on other chains that
/// share a session id are never elided or rewired.
type SourceChainKey = (u64, u32);

/// A unified history projection over `EditChain` ops and `Git` commits.
#[derive(Debug, Clone, Default)]
pub struct HistoryProjection {
    /// `EditChain` operations in canonical causal order (oldest-first).
    pub ops: Vec<Op>,
    /// `Git` commits keyed by `(RepositoryId, GitOid)`.
    pub git: GitProjection,
    /// Structural relationship notes (`ForkOf`, `SubagentOf`, `ReconnectsTo`)
    /// keyed by the RAW causal parent they annotate, as stored in `Op.parents`.
    /// This is the construction-time index: the collapse reads it to fold fork
    /// prologues and to canonicalize anchors/targets into the visible-row map
    /// exposed by [`Self::relationship_notes`]. Keeping the raw index here means
    /// collapse-time logic never needs the canonical map before it exists.
    relationship_notes: HashMap<OpId, Vec<Op>>,
    /// Explicit projection options (bundling policy, etc.). Threaded through so
    /// projection behavior is deterministic and a real cache key — never global.
    options: ProjectionOptions,
    /// Git commit OID hexes of the currently projected commits, cached so
    /// per-row parent lifting stays O(parents) instead of cloning the full
    /// present set (or scanning every commit) for each windowed row.
    git_present: std::collections::HashSet<String>,
    /// Cached collapsed (top-level-row) projection with its canonical
    /// representative map and canonicalized relationship notes. Computed once at
    /// construction so every per-row path (`ordered_nodes`, `independent_chains`,
    /// `lifted_parent_keys`, layout, filtering, windowed edges) reads a stable
    /// canonical view without rebuilding it per row. The collapse is ~linear in
    /// op count and cheap relative to the per-row consumers that reuse it.
    collapsed_projection: CollapsedProjection,
}

/// Result of collapsing raw imports into top-level history rows.
///
/// Alongside the rows carries the reversible bundle membership maps so layout/filter
/// can preserve chain continuity when a child's parent is a bundled META op — without
/// ever rewriting stored `Op.parents`.
///
/// The central invariant of the semantic collapse: **every source operation that
/// participates in a collapsed bundle resolves deterministically to a visible
/// projected row**. `representative` maps each op that does not render as its own
/// row (a normalized child folded into its raw import parent, a bundled META
/// sub-op, a tool result folded into its call, a structural relationship note
/// folded out of rendering, or a fork prologue elided as a trunk duplicate) to the
/// op id of the visible row that represents it. Relationship anchors and targets
/// are canonicalized through this map before any parent-key construction, lane
/// allocation, filtering, or windowed edge geometry runs, so a folded endpoint can
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
    /// call, structural relationship notes folded out of rendering (mapped to
    /// their anchor's visible row), and fork prologues elided as trunk duplicates
    /// (mapped to the trunk boundary at the split).
    representative: HashMap<OpId, OpId>,
    /// Structural relationship notes re-keyed for edge drawing: keyed by the
    /// CANONICAL visible anchor (the representative of the note's stored causal
    /// parent), so a note whose anchor was folded into a bundle is still reachable
    /// from the visible row that represents it. Each note's `target_ids` stay as
    /// stored (raw, possibly folded op ids); every edge-construction path lifts
    /// them through `representative` via [`canonicalize_parents`] and drops any
    /// target that cannot be resolved to a visible row, so a virtual edge never
    /// reaches lane allocation or windowed edge geometry with a phantom key. Built
    /// once per collapse so per-row paths (layout/filter/order) don't re-derive it.
    canonical_notes: HashMap<OpId, Vec<Op>>,
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
            git_present: std::collections::HashSet::new(),
            collapsed_projection: CollapsedProjection::default(),
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
        let git_present: std::collections::HashSet<String> =
            git.commits.keys().map(|(_, oid)| oid.to_hex()).collect();
        let mut projection = Self {
            ops,
            git,
            relationship_notes,
            options,
            git_present,
            collapsed_projection: CollapsedProjection::default(),
        };
        // Build the canonical collapse eagerly so `relationship_notes` and every
        // layout/filter/order path see a stable canonical view from the start
        // and reused by every row/layout path.
        projection.collapsed_projection = projection.collapsed_ops();
        projection
    }

    /// Returns the structural relationship notes re-keyed for edge drawing: keyed
    /// by the CANONICAL visible anchor (the representative of the note's stored
    /// causal parent) with each note's `target_ids` kept as stored. Used by
    /// [`HistoryNode::parent_keys`] so virtual fork/subagent/reconnect edges are
    /// reachable from rendered rows even when their source ops were folded into a
    /// collapsed bundle; the raw targets are lifted to visible rows (or dropped)
    /// by every layout/filter/order path through the canonical representative map.
    #[must_use]
    pub fn relationship_notes(&self) -> &HashMap<OpId, Vec<Op>> {
        &self.collapsed_projection.canonical_notes
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
        filter::apply_owned(
            nodes,
            &self.git.links,
            self.relationship_notes(),
            &self.collapsed_projection.representative,
            filter,
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
        for commit in self.git.commits.values() {
            nodes.push(HistoryNode::GitCommit(Box::new(commit.clone())));
        }
        let present: std::collections::HashSet<String> =
            nodes.iter().map(HistoryNode::node_key).collect();
        let roots: Vec<&HistoryNode> = nodes
            .iter()
            .filter(|node| {
                // A node is a root only when none of its parents resolve to a
                // present row — directly, or through a folded-op representative
                // (bundled META op, normalized child, tool result, structural note,
                // fork prologue, ...). Unresolved parents are dropped, so they can
                // never fragment a source chain into an extra root.
                canonicalize_parents(
                    node.parent_keys(&self.git.links, self.relationship_notes()),
                    &self.collapsed_projection.representative,
                    &present,
                )
                .is_empty()
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
        for commit in self.git.commits.values() {
            nodes.push(HistoryNode::GitCommit(Box::new(commit.clone())));
        }

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
        // Git commits and unscoped ops are not re-dated. Anchors are assigned
        // BEFORE scheduling because the anchor is the effective time the
        // chronological tie-break reads; the values themselves are order-free
        // (per-session minimum observed timestamp).
        let mut session_first_ts: HashMap<u64, u64> = HashMap::new();
        for node in &nodes {
            if matches!(node.effective_time(), EffectiveTime::Unknown) || node.git_oid().is_some() {
                continue;
            }
            let session = match node {
                HistoryNode::EditOperation { op, .. } | HistoryNode::CollapsedImport { op, .. } => {
                    match op.scope {
                        editchain_core::ScopeRef::Session(session) => Some(session.0),
                        editchain_core::ScopeRef::None
                        | editchain_core::ScopeRef::Chain(_)
                        | editchain_core::ScopeRef::Turn(_)
                        | editchain_core::ScopeRef::File(_) => None,
                    }
                }
                HistoryNode::ExecuteBundle { anchor, .. } => match anchor.scope {
                    editchain_core::ScopeRef::Session(session) => Some(session.0),
                    editchain_core::ScopeRef::None
                    | editchain_core::ScopeRef::Chain(_)
                    | editchain_core::ScopeRef::Turn(_)
                    | editchain_core::ScopeRef::File(_) => None,
                },
                HistoryNode::GitCommit(_) => None,
            };
            if let Some(session) = session {
                let entry = session_first_ts.entry(session).or_insert(u64::MAX);
                *entry = (*entry).min(node.timestamp_ms());
            }
        }
        for node in &mut nodes {
            if !matches!(node.effective_time(), EffectiveTime::Unknown) {
                continue;
            }
            let session = match node {
                HistoryNode::EditOperation { op, .. } | HistoryNode::CollapsedImport { op, .. } => {
                    match op.scope {
                        editchain_core::ScopeRef::Session(session) => Some(session.0),
                        editchain_core::ScopeRef::None
                        | editchain_core::ScopeRef::Chain(_)
                        | editchain_core::ScopeRef::Turn(_)
                        | editchain_core::ScopeRef::File(_) => None,
                    }
                }
                HistoryNode::ExecuteBundle { anchor, .. } => match anchor.scope {
                    editchain_core::ScopeRef::Session(session) => Some(session.0),
                    editchain_core::ScopeRef::None
                    | editchain_core::ScopeRef::Chain(_)
                    | editchain_core::ScopeRef::Turn(_)
                    | editchain_core::ScopeRef::File(_) => None,
                },
                HistoryNode::GitCommit(_) => None,
            };
            if let Some(anchor) = session.and_then(|id| session_first_ts.get(&id).copied()) {
                if anchor == 0 {
                    continue;
                }
                // Record as a BundleAnchor display time — clone is a display-only
                // provenance, not a clock mutation.
                node.set_bundle_anchor(anchor);
            }
        }

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
        let keys: Vec<OrderingKey> = nodes.iter().map(ordering_key).collect();
        let present: std::collections::HashSet<OrderingKey> = keys.iter().copied().collect();
        // Use typed Copy keys while scheduling, avoiding repeated OpId string
        // formatting, allocation, and hashing on the first large-chain window.
        let index_of: HashMap<OrderingKey, usize> = keys
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
            // ops, folded children, tool results, structural notes, and fork
            // prologues all lift to the row that represents them. Parents that
            // fail to resolve are dropped so a phantom can never block the sort.
            for parent in ordering_parent_keys(
                node,
                &self.git.links,
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
        // One-to-one cross-record duplicate-pair state for the response_item /
        // event_msg echo family, computed once in a single O(n) pass over the
        // ops. Response_item rows consume one pair slot per matching event_msg
        // row as the main loop reaches them in input order.
        let mut echo_pairs = meta::EchoPairState::from_ops(&self.ops);
        // Set of raw import op ids (the linear backbone).
        let import_ids: std::collections::HashSet<OpId> = self
            .ops
            .iter()
            .filter(|op| matches!(op.kind, editchain_core::OpKind::Import(_)))
            .map(|op| op.id)
            .collect();
        // Map raw import op id -> its normalized children (in input order).
        let mut children_of: HashMap<OpId, Vec<&Op>> = HashMap::new();
        // Map folded child op id -> the raw import op id it folds into. Every
        // folded child must resolve to that import's visible row through the
        // canonical representative map (the semantic-collapse invariant).
        let mut parent_import_of: HashMap<OpId, OpId> = HashMap::new();
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
                    let _: &mut OpId = parent_import_of.entry(op.id).or_insert(parent);
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
        // Canonical representative map (op id -> visible row op id). This is the
        // semantic-collapse invariant: every op that does not render as its own
        // row resolves deterministically to the visible row that represents it.
        // Layout/filter/order lift parents and relationship endpoints through it.
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
                            sub_ops.push(Arc::new(op.clone()));
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
                // Semantic readability metadata is derived deterministically
                // here, where the raw envelope and its normalized children are
                // both available.
                let duplicate_of_event_msg = echo_pairs.is_paired_response_item(op);
                let meta = meta::for_collapsed_import(
                    op,
                    children.map(Vec::as_slice),
                    duplicate_of_event_msg,
                );
                // A META/structural record that falls through is standalone and is
                // NOT an anchor. Only a dated content-bearing import becomes one.
                let source_time = source_time_of(op);
                let is_anchor_eligible = !is_meta && !is_structural;
                let idx = result.len();
                result.push(HistoryNode::CollapsedImport {
                    op: Arc::new(op.clone()),
                    source_time,
                    summary,
                    kind,
                    author,
                    sub_ops: Vec::new(),
                    meta,
                });
                if is_anchor_eligible {
                    let _: Option<usize> = anchors.insert(chain, idx);
                }
            } else if folded.contains(&op.id) {
                // Drop normalized ops folded into their parent import op, and
                // record the fold so any parent/target that references this op
                // (relationship notes, causal parents of surviving rows) resolves
                // to the import's visible row instead of dangling.
                if let Some(import_id) = parent_import_of.get(&op.id).copied() {
                    let _: Option<OpId> = representative.insert(op.id, import_id);
                }
            } else {
                // Standalone op (e.g. ChainStart, or a message not tied to an
                // import) — keep as-is. It is a content/structural row and anchors
                // its chain for subsequent metadata.
                let source_time = source_time_of(op);
                let idx = result.len();
                result.push(HistoryNode::EditOperation {
                    op: Arc::new(op.clone()),
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
        self.fold_fork_prologues(&mut result, &mut representative);

        // The present row set is final once every fold pass has run. Any op id
        // that is neither a row nor resolvable to a row here is genuinely
        // external and must be dropped from edges — never fed to layout.
        let present = row_node_keys(&result);

        // Structural relationship notes are folded out of rendering entirely; give
        // each one a canonical representative (its anchor's visible row, falling
        // back to its first target's visible row) so the invariant holds even if
        // some op ever references a note id directly.
        for op in &self.ops {
            if !is_structural_note(op) || representative.contains_key(&op.id) {
                continue;
            }
            let endpoint = op.parents.iter().next().copied().or_else(|| {
                if let editchain_core::OpKind::Note(n) = &op.kind {
                    n.target_ids.first().copied()
                } else {
                    None
                }
            });
            if let Some(rep) =
                endpoint.and_then(|id| canonical_op_id(id, &representative, &present))
            {
                let _: Option<OpId> = representative.insert(op.id, rep);
            }
        }

        // Re-key relationship notes by their canonical visible anchor so a folded
        // anchor (e.g. a ReconnectsTo on a collab Tool op) is still reachable
        // from the visible row that represents it. Targets stay as stored —
        // every edge-construction path lifts them through the representative map
        // via `canonicalize_parents`, so a folded target resolves at layout time
        // and an unresolvable one is dropped before it can reach lane allocation
        // or windowed edge geometry.
        let canonical_notes = self.canonicalize_relationship_notes(&representative, &present);

        // Semantic-collapse invariant, checked once per collapse: every ordinary
        // op either renders as a row or resolves through the representative map
        // to a row. A structural note with no resolvable endpoint is inert and is
        // deliberately dropped, so it is the sole exception.
        debug_assert!(
            self.ops.iter().all(|op| {
                let key = op.id.to_string();
                present.contains(&key)
                    || representative.contains_key(&op.id)
                    || is_structural_note(op)
            }),
            "every ordinary op must render as a row or resolve through the canonical representative map"
        );

        CollapsedProjection {
            present,
            nodes: result,
            representative,
            canonical_notes,
        }
    }

    /// Canonicalize structural relationship notes for edge drawing.
    ///
    /// The raw [`Self::relationship_notes`] index is keyed by each note's stored
    /// causal parent — which may itself be a folded op (e.g. a `ReconnectsTo`
    /// anchored on a Tool op folded into its import, or a `SubagentOf` anchored on
    /// a subagent's first message). This re-keys every note by the canonical
    /// visible anchor (the representative of its stored parent), so the virtual
    /// edge is reachable from the row that represents the note's anchor. The
    /// note's `target_ids` are left exactly as stored — the raw ids may name
    /// folded ops (e.g. a `SubagentOf` target that is a folded structural start
    /// marker); every edge-construction path resolves them through the canonical
    /// representative map, dropping any target that cannot be lifted to a visible
    /// row. A note whose anchor cannot be resolved to a visible row is dropped.
    fn canonicalize_relationship_notes(
        &self,
        representative: &HashMap<OpId, OpId>,
        present: &std::collections::HashSet<String>,
    ) -> HashMap<OpId, Vec<Op>> {
        let mut out: HashMap<OpId, Vec<Op>> = HashMap::new();
        for (stored_anchor, notes) in &self.relationship_notes {
            let Some(anchor) = canonical_op_id(*stored_anchor, representative, present) else {
                continue;
            };
            for note in notes.iter().cloned() {
                out.entry(anchor).or_default().push(note);
            }
        }
        // Deterministic order per anchor (HashMap iteration order is
        // process-random; parent_keys emits virtual targets in list order).
        for notes in out.values_mut() {
            notes.sort_unstable_by_key(|n| (n.id.node.0, n.id.boot, n.id.seq));
        }
        out
    }

    /// Fold each fork branch's pre-boundary prologue into the trunk it branches
    /// from, so shared messages never render twice.
    ///
    /// A `ForkOf` note has a causal parent `P` (the branch's op at the divergence
    /// boundary) and a target `T` (the trunk's op at that same boundary). The
    /// branch's own chain before `P` — the prologue — duplicates the trunk's
    /// earlier messages and must be elided. We drop those nodes from `result` and
    /// record each dropped row in the canonical representative map, mapped to the
    /// trunk boundary at the split, so a surviving child's edge to a dropped
    /// prologue node resolves to a visible row instead of a phantom. The `ForkOf`
    /// virtual edge keeps the branch attached to the trunk at the split — no
    /// stored `Op.parents` mutation (SPEC §1.1).
    ///
    /// Prologue detection and the representative rewiring are restricted to the
    /// fork boundary's exact source chain `(OpId.node, OpId.boot)`: a session can
    /// hold several source chains at once (the branch, the trunk it forked from,
    /// and unrelated chains), and lower-seq rows on those other chains must never
    /// be elided or redirected just because they share the session id with the
    /// branch.
    fn fold_fork_prologues(
        &self,
        result: &mut Vec<HistoryNode>,
        representative: &mut HashMap<OpId, OpId>,
    ) {
        // Collect (branch_boundary, trunk_boundary) pairs from ForkOf notes. The
        // branch boundary is the note's causal parent; the trunk boundary is its
        // target. The branch renders off the trunk at this split via the note's
        // virtual edge (see [`HistoryNode::parent_keys`]), so we need only ELIDE
        // the branch's duplicated prologue from the rows — no causal parent
        // mutation (SPEC §1.1).
        // Sorted (HashMap iteration order is process-random) so the later
        // representative assignment for dropped prologue rows is deterministic.
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
        boundary_pairs.sort_unstable_by_key(|(p, _)| (p.node.0, p.boot, p.seq));
        if boundary_pairs.is_empty() {
            return;
        }

        // Group ops by source chain (node, boot), each as (op id, seq), so we can
        // find, for each branch boundary, the chain it lives in and thus the
        // prologue (all lower-seq ops on that exact chain). Sessions may carry
        // several chains at once — a fork branch plus an unrelated or trunk chain
        // sharing the same session id — so grouping by session alone would elide
        // and rewire rows that merely share the session. We include every op kind
        // (not just imports) so the fold works uniformly over collapsed-import
        // and standalone message nodes in tests and real chains alike.
        let mut by_chain: HashMap<SourceChainKey, Vec<(OpId, u64)>> = HashMap::new();
        for op in &self.ops {
            by_chain
                .entry((op.id.node.0, op.id.boot))
                .or_default()
                .push((op.id, op.id.seq));
        }

        // Mark the prologue: for each branch boundary, every op on the branch
        // boundary's exact source chain with a strictly smaller seq (the shared
        // backlog before the split) is a duplicated prologue node to elide.
        let mut prologue: std::collections::HashSet<OpId> = std::collections::HashSet::new();
        for (branch_boundary, _trunk_boundary) in &boundary_pairs {
            if let Some(vec) = by_chain.get(&(branch_boundary.node.0, branch_boundary.boot)) {
                let boundary_seq = branch_boundary.seq;
                for &(op_id, seq) in vec {
                    if op_id != *branch_boundary && seq < boundary_seq {
                        let _: bool = prologue.insert(op_id);
                    }
                }
            }
        }
        if prologue.is_empty() {
            return;
        }

        // Drop the prologue nodes from the rendered rows. The branch's boundary
        // node and everything after it stay; their causal edge to the (now
        // removed) prologue resolves through the representative map to the trunk
        // boundary at the split, and the ForkOf virtual edge keeps the branch
        // attached to the trunk.
        let mut drop_idx: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut dropped_ids: Vec<OpId> = Vec::new();
        for (i, n) in result.iter().enumerate() {
            if let Some(op_id) = n.op_id() {
                if prologue.contains(&op_id) {
                    let _: bool = drop_idx.insert(i);
                    dropped_ids.push(op_id);
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

        // Canonicalize each dropped prologue row onto its branch's trunk boundary:
        // a surviving branch node whose stored parent is a dropped prologue row
        // then resolves to the visible trunk row at the split (falling back to the
        // branch boundary itself), so the elision never leaves a dangling parent
        // that would inflate lanes or draw a phantom interval.
        if dropped_ids.is_empty() {
            return;
        }
        let present = row_node_keys(result);
        for dropped_id in dropped_ids {
            let rep = boundary_pairs
                .iter()
                .find_map(|(branch_boundary, trunk_boundary)| {
                    let same_source_chain = branch_boundary.node == dropped_id.node
                        && branch_boundary.boot == dropped_id.boot;
                    if !same_source_chain || dropped_id.seq >= branch_boundary.seq {
                        return None;
                    }
                    canonical_op_id(*trunk_boundary, representative, &present)
                        .or_else(|| canonical_op_id(*branch_boundary, representative, &present))
                });
            if let Some(rep) = rep {
                let _: Option<OpId> = representative.insert(dropped_id, rep);
            }
        }
    }

    /// Fold tool-result nodes into their tool-call parents' sub-ops.
    ///
    /// Mutates `result` in place: tool-result nodes whose parent is a tool-call
    /// node are removed from the top-level list and their Tool op plus any
    /// metadata already bundled beneath the result are appended to the call's
    /// `sub_ops`. Children of a dropped result are re-parented to the call so the
    /// chain stays continuous.
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
        let mut attach: HashMap<usize, Vec<Arc<Op>>> = HashMap::new();
        // parent index -> structured outcome carried by the absorbed result row.
        let mut outcome_fold: HashMap<usize, Outcome> = HashMap::new();
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
                let attached = attach.entry(parent_idx).or_default();
                if let Some(tool_op) = Self::tool_result_op(n, children_of) {
                    attached.push(tool_op);
                }
                // The absorbed result row's structured outcome (derived from
                // its raw `status`/`errorMessage`/`exitCode`) carries over to
                // the visible call row, so the combined call+result keeps its
                // evidence-based outcome instead of degrading to unknown.
                let result_outcome = n.record_meta().outcome;
                if result_outcome != Outcome::Unknown {
                    let _: &mut Outcome = outcome_fold
                        .entry(parent_idx)
                        .and_modify(|acc| *acc = merge_outcome(*acc, result_outcome))
                        .or_insert(result_outcome);
                }
                // META records are bundled before tool-result grouping. If the
                // result row is then absorbed into its call, move those records
                // with it; otherwise they remain in storage but disappear from
                // the expanded renderer/debug view.
                attached.extend(n.sub_ops().iter().cloned());
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

        // The dropped tool-result rows are folded bundles too: map each one to the
        // call that absorbed it (semantic-collapse invariant), so its folded
        // children, relationship notes, or later causal parents that reference the
        // result id resolve to the call's visible row instead of dangling.
        for (&dropped, &call) in &replacement {
            let _: &mut OpId = representative.entry(dropped).or_insert(call);
        }

        // Attach collected tool-result ops and their bundled metadata to the
        // call's sub-ops, folding the absorbed result's structured outcome
        // into the call row's metadata.
        for (parent_idx, ops) in &attach {
            if let Some(HistoryNode::CollapsedImport { sub_ops, meta, .. }) =
                result.get_mut(*parent_idx)
            {
                sub_ops.extend(ops.iter().cloned());
                if let Some(outcome) = outcome_fold.get(parent_idx).copied() {
                    meta.outcome = outcome;
                }
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
    fn tool_result_op(
        node: &HistoryNode,
        children_of: &HashMap<OpId, Vec<&Op>>,
    ) -> Option<Arc<Op>> {
        let HistoryNode::CollapsedImport { op, .. } = node else {
            return None;
        };
        children_of.get(&op.id).and_then(|children| {
            children.iter().find_map(|c| match &c.kind {
                editchain_core::OpKind::Tool(_) => Some(Arc::new((*c).clone())),
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
            let hex = commit.oid.to_hex();
            drop(
                self.git
                    .commits
                    .insert((commit.repository, commit.oid), commit),
            );
            let _: bool = self.git_present.insert(hex);
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
        // The visible rows are the rows being laid out (possibly filtered), so a
        // parent resolves only when it is present in THIS layout.
        let present = row_node_keys(sorted);
        let representative = &self.collapsed_projection.representative;
        let parents_of = |key: &str| -> Vec<String> {
            key_to_node.get(key).map_or(Vec::new(), |n| {
                canonicalize_parents(
                    n.parent_keys(links, self.relationship_notes()),
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
        let present = row_node_keys(sorted);
        let representative = &self.collapsed_projection.representative;
        let parents_of = |key: &str| -> Vec<String> {
            key_to_node.get(key).map_or(Vec::new(), |n| {
                canonicalize_parents(
                    n.parent_keys(links, self.relationship_notes()),
                    representative,
                    &present,
                )
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
        // The visible rows are the rows being laid out (possibly filtered), so a
        // parent resolves only when it is present in THIS layout.
        let present = row_node_keys(sorted);
        let representative = &self.collapsed_projection.representative;
        let parents_of = |key: &str| -> Vec<String> {
            key_to_node.get(key).map_or(Vec::new(), |n| {
                canonicalize_parents(
                    n.parent_keys(links, self.relationship_notes()),
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

    /// Return this node's parent keys resolved to their canonical visible rows, so
    /// a parent that was folded away (bundled META op, normalized child, tool
    /// result, structural note, fork prologue, ...) is redirected to the row that
    /// represents it. Unresolvable op parents are dropped.
    ///
    /// Used by the service when emitting `HistoryRow::parents` so the client's
    /// chain assembly sees connected chains, not dangling folded-op parents.
    #[must_use]
    pub fn lifted_parent_keys(&self, node: &HistoryNode) -> Vec<String> {
        // This public helper accepts either op or git rows. The collapsed cache's
        // `present` set contains op rows only, so projected commit keys are
        // checked against the cached [`Self::git_present`] set instead of
        // cloning the full present set per row: the window emission calls this
        // once per top-level row, so a per-row O(V) clone would make window
        // materialization superlinear.
        let representative = &self.collapsed_projection.representative;
        let present = &self.collapsed_projection.present;
        let mut out: Vec<String> = Vec::with_capacity(4);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for parent in node.parent_keys(&self.git.links, self.relationship_notes()) {
            let resolved = if present.contains(&parent) {
                Some(parent)
            } else if let Some(pid) = OpId::from_display_str(&parent) {
                canonical_op_id(pid, representative, present).map(|id| id.to_string())
            } else if self.git_present.contains(&parent) {
                Some(parent)
            } else {
                None
            };
            if let Some(key) = resolved {
                if seen.insert(key.clone()) {
                    out.push(key);
                }
            }
        }
        out
    }

    /// Returns the provider-neutral structural relations for one row in a view.
    ///
    /// `parents` must be the row's FINAL parent keys for that view — the exact
    /// keys the view renders (e.g. [`layout::LayoutContext::parents`] for the
    /// filtered snapshot), NOT the raw stored keys. Each structural note's raw
    /// (possibly folded) targets are canonicalized to their visible rows, and a
    /// relation is returned only when that visible row is one of the supplied
    /// parent keys — so a folded target whose representative row is hidden in
    /// the view never produces a stale relation. Every distinct `(parent, kind)`
    /// match is emitted (a canonical edge can carry several structural kinds at
    /// once, e.g. Subagent + Fork), deduplicating exact duplicates while keeping
    /// parent order deterministic.
    #[must_use]
    pub fn parent_relations_for(
        &self,
        node: &HistoryNode,
        parents: &[String],
    ) -> Vec<ParentRelation> {
        let Some(anchor_id) = node.op_id() else {
            return Vec::new();
        };
        let Some(notes) = self.relationship_notes().get(&anchor_id) else {
            return Vec::new();
        };
        let representative = &self.collapsed_projection.representative;
        let present = &self.collapsed_projection.present;
        let mut out: Vec<ParentRelation> = Vec::new();
        for parent in parents {
            for note in notes {
                let editchain_core::OpKind::Note(n) = &note.kind else {
                    continue;
                };
                let matches = n.target_ids.iter().any(|target| {
                    canonical_parent_key(&target.to_string(), representative, present).as_deref()
                        == Some(parent.as_str())
                });
                if !matches {
                    continue;
                }
                if let Some(kind) = relation_kind(n.relationship) {
                    let relation = ParentRelation {
                        parent: parent.clone(),
                        kind,
                    };
                    // Emit every distinct (parent, kind) pair rather than stopping
                    // at the first note kind that matches a canonical parent; skip
                    // exact duplicates deterministically (parent/note iteration
                    // order is stable).
                    if !out.contains(&relation) {
                        out.push(relation);
                    }
                }
            }
        }
        out
    }

    /// Returns the node keys of rows that participate in structural topology
    /// (`ForkOf` / `SubagentOf` / `ReconnectsTo` anchors or targets).
    ///
    /// Structural rows carry virtual edges, so a view must never fold them away:
    /// the Activity execute-run bundling excludes them exactly like the chain
    /// filter preserves them from every hide predicate. Targets are lifted to
    /// their canonical visible rows (or dropped when unresolvable in this view).
    #[must_use]
    pub fn structural_row_keys(&self, nodes: &[HistoryNode]) -> std::collections::HashSet<String> {
        let mut keys = std::collections::HashSet::new();
        let note_map = self.relationship_notes();
        let representative = &self.collapsed_projection.representative;
        let present = row_node_keys(nodes);
        for node in nodes {
            let Some(anchor_id) = node.op_id() else {
                continue;
            };
            if note_map.contains_key(&anchor_id) {
                let _: bool = keys.insert(node.node_key());
            }
            if let Some(anchor_notes) = note_map.get(&anchor_id) {
                for note in anchor_notes {
                    let editchain_core::OpKind::Note(n) = &note.kind else {
                        continue;
                    };
                    for target in &n.target_ids {
                        if let Some(key) =
                            canonical_parent_key(&target.to_string(), representative, &present)
                        {
                            let _: bool = keys.insert(key);
                        }
                    }
                }
            }
        }
        keys
    }
}

/// Map a structural note relationship to its provider-neutral kind, or `None`
/// for non-structural relationships (the projection only indexes structural
/// notes, but unknown/future kinds must degrade gracefully rather than invent
/// labels).
fn relation_kind(relationship: NoteRelationship) -> Option<RelationKind> {
    match relationship {
        NoteRelationship::SubagentOf => Some(RelationKind::Subagent),
        NoteRelationship::ReconnectsTo => Some(RelationKind::Reconnect),
        NoteRelationship::ForkOf => Some(RelationKind::Fork),
        NoteRelationship::Corrects
        | NoteRelationship::Supersedes
        | NoteRelationship::Rejects
        | NoteRelationship::Redacts
        | NoteRelationship::Explains => None,
    }
}

/// A provider-neutral structural parent edge on one visible history row.
///
/// Returned by [`HistoryProjection::parent_relations_for`] for the row's FINAL
/// parent keys in a view; `parent` is always one of those keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentRelation {
    /// The canonical visible parent row key this relation applies to — one of
    /// the row's final parent keys in the view that supplied them.
    pub parent: String,
    /// The provider-neutral relationship kind of this edge.
    pub kind: RelationKind,
}

/// Provider-neutral structural relationship kinds, derived from the
/// `SubagentOf` / `ReconnectsTo` / `ForkOf` structural notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// The row starts a subagent branch spawned by the target row.
    Subagent,
    /// The row is the parent thread's completion result returning into the
    /// subagent branch (the target row is the subagent's last op).
    Reconnect,
    /// The row branches off the target row at a fork divergence boundary.
    Fork,
}

/// Allocation-free identity used only by the topological scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OrderingKey {
    /// `EditChain` operation identity.
    Op(OpId),
    /// Git commit identity.
    Git(GitOid),
}

/// Return the typed scheduler key for one visible row.
fn ordering_key(node: &HistoryNode) -> OrderingKey {
    match node {
        HistoryNode::EditOperation { op, .. } | HistoryNode::CollapsedImport { op, .. } => {
            OrderingKey::Op(op.id)
        }
        HistoryNode::ExecuteBundle { anchor, .. } => OrderingKey::Op(anchor.id),
        HistoryNode::GitCommit(commit) => OrderingKey::Git(commit.oid),
    }
}

/// Resolve one operation id through folded representatives for scheduling.
fn canonical_ordering_op(
    mut id: OpId,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<OrderingKey>,
) -> Option<OrderingKey> {
    loop {
        let key = OrderingKey::Op(id);
        if present.contains(&key) {
            return Some(key);
        }
        match representative.get(&id).copied() {
            Some(next) if next != id => id = next,
            Some(_) | None => return None,
        }
    }
}

/// Build canonical typed parents without formatting IDs as strings.
fn ordering_parent_keys(
    node: &HistoryNode,
    git_links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
    notes: &HashMap<OpId, Vec<Op>>,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<OrderingKey>,
) -> Vec<OrderingKey> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |candidate: Option<OrderingKey>| {
        if let Some(key) = candidate {
            if seen.insert(key) {
                out.push(key);
            }
        }
    };
    match node {
        HistoryNode::EditOperation { op, .. } | HistoryNode::CollapsedImport { op, .. } => {
            for parent in &op.parents {
                push(canonical_ordering_op(*parent, representative, present));
            }
            for source in std::iter::once(op).chain(node.sub_ops()) {
                if let Some(links) = git_links.get(&source.id) {
                    for link in links {
                        let key = OrderingKey::Git(link.target_oid);
                        push(present.contains(&key).then_some(key));
                    }
                }
            }
            if let Some(relationship_notes) = notes.get(&op.id) {
                for note in relationship_notes {
                    if let editchain_core::OpKind::Note(note) = &note.kind {
                        for target in &note.target_ids {
                            push(canonical_ordering_op(*target, representative, present));
                        }
                    }
                }
            }
        }
        HistoryNode::ExecuteBundle { anchor, .. } => {
            for parent in &anchor.parents {
                push(canonical_ordering_op(*parent, representative, present));
            }
        }
        HistoryNode::GitCommit(commit) => {
            for parent in &commit.parents {
                let key = OrderingKey::Git(*parent);
                push(present.contains(&key).then_some(key));
            }
        }
    }
    out
}

/// Collect the node keys of collapsed top-level rows (the "present" set used to
/// decide whether a parent resolves to a rendered row).
fn row_node_keys(nodes: &[HistoryNode]) -> std::collections::HashSet<String> {
    nodes.iter().map(HistoryNode::node_key).collect()
}

/// Resolve each parent key to its canonical visible row key through the
/// representative map, dropping keys that cannot be resolved to a rendered row.
///
/// A parent that was folded into a bundle (a META sub-op, a normalized child, a
/// tool result, a fork prologue, a structural note) is redirected to the visible
/// row that represents it. A key that still fails to resolve — an op id absent
/// from the projection with no representative — is dropped so it can never reach
/// lane allocation or windowed edge geometry as a phantom. Non-op keys (git OID
/// hex) are external anchors and are kept for the caller's row lookup. The
/// result is deduplicated preserving first-occurrence order. This never rewrites
/// stored `Op.parents`; it is a layout/filter-time lift only.
fn canonicalize_parents(
    parents: Vec<String>,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(parents.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for parent in parents {
        if let Some(key) = canonical_parent_key(&parent, representative, present) {
            if seen.insert(key.clone()) {
                out.push(key);
            }
        }
    }
    out
}

/// Resolve a single parent key to its canonical visible row key.
///
/// Returns `None` when the key is an op id that is neither a rendered row nor
/// represented by one (genuinely unresolved). Non-op keys (git OID hex) survive
/// only when they name a row in `present`; absent external anchors are dropped
/// before layout just like absent op parents.
fn canonical_parent_key(
    parent: &str,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<String>,
) -> Option<String> {
    if present.contains(parent) {
        return Some(parent.to_string());
    }
    let pid = OpId::from_display_str(parent)?;
    canonical_op_id(pid, representative, present).map(|id| id.to_string())
}

/// Chase an `OpId` through the representative map until it reaches a visible row.
///
/// Returns `None` when the id is neither a row nor mapped (directly or through a
/// chain) to a row — the caller drops such ids instead of emitting phantom keys.
fn canonical_op_id(
    id: OpId,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<String>,
) -> Option<OpId> {
    let mut cur = id;
    loop {
        if present.contains(&cur.to_string()) {
            return Some(cur);
        }
        match representative.get(&cur).copied() {
            Some(next) if next != cur => cur = next,
            _ => return None,
        }
    }
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
    let mut file = String::new();
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
                OpKind::File(_) if file.is_empty() => {
                    if let Some(path) = annotated_file_path(child, Some(children)) {
                        file = format!("file: {path}");
                    }
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
    if !file.is_empty() {
        return file;
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

/// Resolve a collapsed File row's display path.
///
/// The core [`FileOp`] stores only a hashed `PathId`; the provider-neutral
/// path text is carried by an explicit `Explains` note targeting the file op
/// (the Codex importer emits one per file item). Rows without such an
/// annotation — e.g. Claude attachment rows, which carry no file path — fall
/// through to the raw-record label, preserving existing Claude behavior.
#[must_use]
fn annotated_file_path(file_op: &Op, children: Option<&Vec<&Op>>) -> Option<String> {
    let children = children?;
    for child in children {
        if let editchain_core::OpKind::Note(note) = &child.kind {
            if note.relationship == NoteRelationship::Explains
                && note.target_ids.contains(&file_op.id)
            {
                let text = payload_text(&note.content);
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }
    None
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
        // Codex event envelopes put the meaningful lifecycle discriminator in
        // `payload.type`; showing it avoids a wall of indistinguishable
        // `event_msg` labels when a leading/unbundled record is visible.
        "event_msg" => value
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(serde_json::Value::as_str)
            .filter(|event_type| !event_type.is_empty())
            .unwrap_or(record_type)
            .to_string(),
        // Codex response items put the meaningful label in `payload.type` and
        // its content: reasoning items expose a `summary` array of
        // summary-text blocks, message items carry a `content` array. Showing
        // the payload type (or its text) avoids a wall of indistinguishable
        // `response_item` labels when a leading/unbundled record is visible.
        "response_item" => {
            let Some(payload) = value.get("payload") else {
                return record_type.to_string();
            };
            let payload_type = payload
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            match payload_type {
                "reasoning" => {
                    if let Some(text) = first_response_summary_text(payload) {
                        return truncate_line(&text);
                    }
                    if let Some(text) = first_response_content_text(payload) {
                        return truncate_line(&text);
                    }
                    "reasoning".to_string()
                }
                "message" => {
                    if let Some(text) = first_response_content_text(payload) {
                        return truncate_line(&text);
                    }
                    "message".to_string()
                }
                _ if !payload_type.is_empty() => payload_type.to_string(),
                _ => record_type.to_string(),
            }
        }
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

/// Extract the first non-empty summary text from a response-item payload.
///
/// Reasoning items carry a `summary` array of `summary-text` blocks (and the
/// compacted projection keeps the same shape); a plain string summary is
/// accepted too. Returns `None` when there is no text, so callers can fall
/// back to content text.
#[must_use]
fn first_response_summary_text(payload: &serde_json::Value) -> Option<String> {
    let summary = payload.get("summary")?;
    let items = match summary {
        serde_json::Value::Array(items) => items,
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            return Some(text.clone());
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Object(_) => return None,
    };
    for item in items {
        match item {
            serde_json::Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(serde_json::Value::as_str) {
                    if !text.trim().is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
            serde_json::Value::String(text) if !text.trim().is_empty() => {
                return Some(text.clone());
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::String(_) => {}
        }
    }
    None
}

/// Extract the first non-empty content text from a response-item payload.
///
/// Content blocks are `{type, text|input_text|output_text}` records; the
/// compacted projection keeps the same shape. Returns `None` when there is
/// no text.
#[must_use]
fn first_response_content_text(payload: &serde_json::Value) -> Option<String> {
    let content = payload.get("content")?;
    let items = match content {
        serde_json::Value::Array(items) => items,
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            return Some(text.clone());
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Object(_) => return None,
    };
    for item in items {
        match item {
            serde_json::Value::Object(map) => {
                for key in ["text", "input_text", "output_text"] {
                    if let Some(text) = map.get(key).and_then(serde_json::Value::as_str) {
                        if !text.trim().is_empty() {
                            return Some(text.to_string());
                        }
                    }
                }
            }
            serde_json::Value::String(text) if !text.trim().is_empty() => {
                return Some(text.clone());
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::String(_) => {}
        }
    }
    None
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
fn combined_summary(row_summary: &str, sub_ops: &[Arc<Op>]) -> String {
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

/// Conservative merge precedence for folded outcomes.
///
/// When multiple tool-result rows fold into one call, the combined outcome is
/// the most severe concluded outcome among them, so a later milder result can
/// never mask earlier evidence of a problem: `Failure` > `Cancelled` >
/// `Warning` > `Success`. `Unknown` ranks lowest; callers exclude it before
/// merging so it can never erase known evidence.
#[must_use]
fn outcome_severity(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Failure => 4,
        Outcome::Cancelled => 3,
        Outcome::Warning => 2,
        Outcome::Success => 1,
        Outcome::Unknown => 0,
    }
}

/// Deterministic fold of two concluded outcomes: the more severe wins; ties
/// keep the existing (earlier) outcome.
#[must_use]
fn merge_outcome(acc: Outcome, next: Outcome) -> Outcome {
    if outcome_severity(next) > outcome_severity(acc) {
        next
    } else {
        acc
    }
}
