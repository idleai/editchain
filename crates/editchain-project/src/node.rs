//! History row identity, source time, and provider-neutral presentation metadata.

use crate::ancestry::{
    has_exact_provider_parent, has_exact_spawn_parent, is_visible_edge_relationship,
};
use crate::labels::combined_summary;
use crate::meta::NodeMeta;
use crate::taxonomy::{ActivityKind, ChainState, Outcome, RecordRole, Visibility as RowVisibility};
use crate::{content, meta, NodeKey};
use editchain_core::{
    GitCommitEntity, GitCommitKey, GitLinkKind, GitOid, Op, OpId, Payload, RepositoryId,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Provenance of a node's effective display time.
///
/// Source time is nullable and immutable; the projection never fabricates a
/// borrowed timestamp for a node whose source time is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveTime {
    /// A real, valid source timestamp.
    Observed(u64),
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
        /// Selected display content and its source completeness.
        content: content::SelectedContent,
        /// Source time of this record; `Unknown` when the source had none.
        source_time: EffectiveTime,
        /// Final parent keys for a derived view, when filtering or Activity
        /// contraction rewrites topology without changing canonical evidence.
        parent_override: Option<Vec<NodeKey>>,
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
        /// Final parent keys for a derived view, when filtering or Activity
        /// contraction rewrites topology without changing canonical evidence.
        parent_override: Option<Vec<NodeKey>>,
        /// Display content selected from normalized children before folding.
        content: content::SelectedContent,
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
    /// stays retrievable/inspectable by its real op id. The bundle occupies the
    /// newest member's display slot. Its node key is a real member op id: the
    /// newest member for a contiguous run, or the unique causal root for an
    /// exact provider-response component.
    ExecuteBundle {
        /// The real member retained as the bundle's graph identity.
        anchor: Arc<Op>,
        /// Effective display time of the newest member.
        source_time: EffectiveTime,
        /// Final parent keys for a derived view, when a later contraction
        /// rewrites this bundle's visible topology.
        parent_override: Option<Vec<NodeKey>>,
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
    /// A synthetic Activity-view summary node folding adjacent Plan rows that
    /// repeat the same normalized heading (see
    /// [`crate::activity::bundle_activity_plan_repeats`]).
    ///
    /// This is a display-only contraction, not importer deduplication. Every
    /// original reasoning record remains an expandable member with its real op
    /// id, timestamp, raw details, and causal position. The node key is the
    /// newest member's op id so the bundle occupies one point on the existing
    /// linear path instead of introducing a branch.
    PlanBundle {
        /// The newest member's op (identity/time/group anchor).
        anchor: Arc<Op>,
        /// Effective display time of the anchor member.
        source_time: EffectiveTime,
        /// Final parent keys for a derived view, when a later contraction
        /// rewrites this bundle's visible topology.
        parent_override: Option<Vec<NodeKey>>,
        /// Original top-level Plan rows, newest-first (display order).
        member_nodes: Vec<HistoryNode>,
        /// Flat expandable ops: every member's op followed by its own bundled
        /// metadata sub-ops, newest-first.
        members: Vec<Arc<Op>>,
        /// The repeated heading, preserving the newest member's presentation.
        summary: String,
        /// Dominant member kind tag (normally `"reflection"`).
        kind: String,
        /// Author label derived from the members.
        author: String,
        /// Deterministic semantic metadata (Narrative / Plan / Primary).
        meta: NodeMeta,
    },
    /// A synthetic Activity-view group containing the contiguous linear work
    /// performed between user/agent chat rows.
    ///
    /// Work groups are formed only from one graph path. Every row incident to
    /// a fork, merge, subagent/reconnect relation, or produced-commit edge is a
    /// hard boundary and remains outside the group. Existing execute/plan
    /// bundles remain as member nodes, giving the viewer a bounded second
    /// disclosure level without losing their original records.
    WorkGroup {
        /// The newest member retained as the group's graph identity.
        anchor: Arc<Op>,
        /// Effective display time of the newest member.
        source_time: EffectiveTime,
        /// Final parent keys for the contracted derived view.
        parent_override: Option<Vec<NodeKey>>,
        /// Original top-level member rows, newest-first. Existing synthetic
        /// bundles remain intact in this vector.
        member_nodes: Vec<HistoryNode>,
        /// Every represented source op, flattened only for exact find/detail
        /// lookup. Presentation hierarchy comes from `member_nodes`.
        members: Vec<Arc<Op>>,
        /// Deterministic aggregate summary derived from all member activities.
        summary: String,
        /// Aggregate semantic metadata (`Work` / Primary plus folded outcome).
        meta: NodeMeta,
    },
    /// A `Git` commit entity.
    GitCommit {
        /// Immutable source commit; derived edges never rewrite its parents.
        commit: Box<GitCommitEntity>,
        /// Final parents in a derived view, including operation parents.
        parent_override: Option<Vec<NodeKey>>,
    },
}

impl HistoryNode {
    /// Returns a display summary for this node.
    ///
    /// For collapsed imports, the summary is the row's own content combined with
    /// the content of any bundled tool results. Metadata remains independently
    /// visible through expansion and never masquerades as parent-row content.
    /// The combined result is truncated to ~1024 chars.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::EditOperation { content, .. } => content.summary.clone(),
            Self::CollapsedImport {
                content, sub_ops, ..
            } => combined_summary(&content.summary, sub_ops),
            Self::ExecuteBundle { summary, .. }
            | Self::PlanBundle { summary, .. }
            | Self::WorkGroup { summary, .. } => summary.clone(),
            Self::GitCommit { commit, .. } => match &commit.message {
                Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
                Payload::Empty | Payload::Blob(_) => commit.oid.to_hex(),
            },
        }
    }

    /// Typed display roles. Tool labels are source fields, never inferred from
    /// the summary string. Expanded metadata and source actions remain separate.
    #[must_use]
    pub fn display_content(&self) -> content::DisplayContent {
        match self {
            Self::EditOperation { content, .. } => content.display.clone(),
            Self::CollapsedImport {
                content, sub_ops, ..
            } => content.display.with_results(sub_ops),
            Self::ExecuteBundle { summary, .. }
            | Self::PlanBundle { summary, .. }
            | Self::WorkGroup { summary, .. } => content::DisplayContent::summary(summary.clone()),
            Self::GitCommit { commit, .. } => {
                content::git_message(&commit.message, commit.oid.to_hex())
            }
        }
    }

    /// Returns the timestamp in Unix ms (0 if unknown).
    ///
    /// Git commits store `committed_at` in Unix **seconds**; `EditChain` ops
    /// store their clock in Unix **milliseconds**. This converts git seconds
    /// to milliseconds so both render as correct dates.
    ///
    /// A node whose source time is absent returns 0. The stored `op.clock` is
    /// never mutated or supplemented from another row.
    #[must_use]
    pub fn timestamp_ms(&self) -> u64 {
        match self.effective_time() {
            EffectiveTime::Observed(ms) => ms,
            EffectiveTime::Unknown => 0,
        }
    }

    /// Returns the effective display time with provenance.
    ///
    /// `Observed` is a real source timestamp; `Unknown` means the source had no
    /// usable time and the projection must not fabricate one.
    #[must_use]
    pub fn effective_time(&self) -> EffectiveTime {
        match self {
            Self::EditOperation { source_time, .. }
            | Self::CollapsedImport { source_time, .. }
            | Self::ExecuteBundle { source_time, .. }
            | Self::PlanBundle { source_time, .. }
            | Self::WorkGroup { source_time, .. } => *source_time,
            Self::GitCommit { commit, .. } => {
                let secs = u64::try_from(commit.committed_at).unwrap_or(0);
                EffectiveTime::Observed(secs.saturating_mul(1000))
            }
        }
    }

    /// Returns the operation ID, if this is an `EditChain` operation.
    #[must_use]
    pub fn op_id(&self) -> Option<OpId> {
        match self {
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => Some(op.id),
            Self::ExecuteBundle { anchor, .. }
            | Self::PlanBundle { anchor, .. }
            | Self::WorkGroup { anchor, .. } => Some(anchor.id),
            Self::GitCommit { .. } => None,
        }
    }

    /// Returns the git commit OID, if this is a git commit.
    #[must_use]
    pub fn git_oid(&self) -> Option<GitOid> {
        match self {
            Self::EditOperation { .. }
            | Self::CollapsedImport { .. }
            | Self::ExecuteBundle { .. }
            | Self::PlanBundle { .. }
            | Self::WorkGroup { .. } => None,
            Self::GitCommit { commit, .. } => Some(commit.oid),
        }
    }

    /// Returns the repository, if this is a git commit.
    #[must_use]
    pub fn repository(&self) -> Option<RepositoryId> {
        match self {
            Self::EditOperation { .. }
            | Self::CollapsedImport { .. }
            | Self::ExecuteBundle { .. }
            | Self::PlanBundle { .. }
            | Self::WorkGroup { .. } => None,
            Self::GitCommit { commit, .. } => Some(commit.repository),
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
            Self::ExecuteBundle { anchor, .. }
            | Self::PlanBundle { anchor, .. }
            | Self::WorkGroup { anchor, .. } => bundle_group(anchor),
            Self::GitCommit { commit, .. } => format!("repo:{}", commit.repository.0),
        }
    }

    /// Returns a stable node key for graph wiring.
    ///
    /// `EditChain` ops use their `OpId` string; Git commits use `git:<repository>:<oid>` keys.
    #[must_use]
    pub fn node_key(&self) -> String {
        self.key().to_string()
    }

    /// Typed graph identity, independent of display formatting.
    #[must_use]
    pub fn key(&self) -> NodeKey {
        match self {
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => NodeKey::Op(op.id),
            Self::ExecuteBundle { anchor, .. }
            | Self::PlanBundle { anchor, .. }
            | Self::WorkGroup { anchor, .. } => NodeKey::Op(anchor.id),
            Self::GitCommit { commit, .. } => NodeKey::Git(commit.key()),
        }
    }

    /// Returns the parent node keys for drawing graph edges.
    ///
    /// A derived-view parent override is authoritative when present. It lets
    /// filtering and Activity contraction splice visible topology while the
    /// canonical operation envelope and relationship evidence remain intact.
    ///
    /// For `EditChain` ops, this includes the causal `Op.parents`, inbound
    /// graph-bearing Git links (with repository-qualified target keys), and
    /// — when `notes` annotates this op with a graph-bearing relationship — the
    /// note's target as a *virtual* parent. A `ProducedBy` link has the opposite
    /// direction: its Git commit gains the source operation as a parent. Git
    /// links are explicit stored relations; no timestamp/text inference is
    /// performed by this projection.
    /// Virtual parents let provider-event, fork, and subagent branches render
    /// without mutating stored source-order causality (SPEC §1.1, §5).
    /// `notes` maps a causal parent op id to the structural notes that annotate
    /// it.
    ///
    /// A collapsed row also inherits graph-bearing links whose source is one of
    /// its bundled sub-ops. This preserves an explicit session-to-Git edge when
    /// its source record is folded into a visible semantic turn.
    ///
    /// An exact `SpawnedBy` parent suppresses an inherited session-start
    /// `BasedOn` edge in this display graph. The Git fact remains stored and
    /// queryable, but drawing both would branch every subagent directly from
    /// its parent's base commit instead of from its exact spawn occurrence.
    ///
    /// Keys are deduplicated preserving first-occurrence order (stored causal
    /// parents, then non-redundant git-link targets from the row and its
    /// sub-ops, then virtual note targets), so a target shared between any of
    /// the three sources is emitted exactly once. This keeps parent keys
    /// deterministic and duplicate-free across source and derived views.
    #[must_use]
    pub fn parent_keys(
        &self,
        git_links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
        notes: &HashMap<OpId, Vec<Op>>,
    ) -> Vec<String> {
        self.parent_nodes(git_links, notes)
            .into_iter()
            .map(|key| key.to_string())
            .collect()
    }

    /// Authoritative typed parents for this stage of the projection.
    ///
    /// Source ancestry, stored links, resolved notes, and group membership are
    /// interpreted here once; ordering and derived views use this same contract.
    #[must_use]
    pub fn parent_nodes(
        &self,
        git_links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
        notes: &HashMap<OpId, Vec<Op>>,
    ) -> Vec<NodeKey> {
        match self {
            Self::EditOperation {
                parent_override: Some(keys),
                ..
            }
            | Self::CollapsedImport {
                parent_override: Some(keys),
                ..
            }
            | Self::ExecuteBundle {
                parent_override: Some(keys),
                ..
            }
            | Self::PlanBundle {
                parent_override: Some(keys),
                ..
            }
            | Self::WorkGroup {
                parent_override: Some(keys),
                ..
            }
            | Self::GitCommit {
                parent_override: Some(keys),
                ..
            } => return keys.clone(),
            Self::EditOperation {
                parent_override: None,
                ..
            }
            | Self::CollapsedImport {
                parent_override: None,
                ..
            }
            | Self::ExecuteBundle {
                parent_override: None,
                ..
            }
            | Self::PlanBundle {
                parent_override: None,
                ..
            }
            | Self::WorkGroup {
                parent_override: None,
                ..
            }
            | Self::GitCommit {
                parent_override: None,
                ..
            } => {}
        }
        match self {
            Self::EditOperation { op, .. } | Self::CollapsedImport { op, .. } => {
                let mut keys: Vec<NodeKey> = Vec::new();
                let mut seen: std::collections::HashSet<NodeKey> = std::collections::HashSet::new();
                let anchored_notes = notes.get(&op.id);
                // A resolved exact provider parent supersedes the physical
                // source predecessor in the display graph. Provider identity
                // alone does not: root-like transport/meta events often carry a
                // UUID with no parentUuid, so they retain source order as the
                // conservative fallback instead of starting a phantom chain.
                let has_provider_parent =
                    has_exact_provider_parent(op.id, anchored_notes.map(Vec::as_slice));
                if !has_provider_parent {
                    for parent in &op.parents {
                        let key = NodeKey::Op(*parent);
                        if seen.insert(key) {
                            keys.push(key);
                        }
                    }
                }
                for source in std::iter::once(op).chain(self.sub_ops()) {
                    // A collapsed row can carry metadata from a different
                    // session (notably a spawned child's session_meta folded
                    // into the parent's spawn tool row). Decide whether Git
                    // provenance is superseded for each source independently;
                    // using only the visible anchor's notes leaks the child's
                    // BasedOn edge onto the parent row and fans every spawn
                    // back to the base commit.
                    // `notes` is canonicalized by the visible anchor, so facts
                    // stored on a folded sub-op live in this same bucket. The
                    // helper checks the fact's immutable stored parent against
                    // `source.id`, keeping one member's relation from affecting
                    // another member.
                    let source_has_spawn_parent =
                        has_exact_spawn_parent(source.id, anchored_notes.map(Vec::as_slice));
                    if let Some(links) = git_links.get(&source.id) {
                        for link in links.iter().filter(|link| {
                            link.kind != GitLinkKind::ProducedBy
                                && !(source_has_spawn_parent && link.kind == GitLinkKind::BasedOn)
                        }) {
                            let key = NodeKey::Git(link.target_key());
                            if seen.insert(key) {
                                keys.push(key);
                            }
                        }
                    }
                }
                // Virtual parents: this op is the child occurrence annotated by
                // a graph-bearing relation, so the note's target becomes a
                // parent edge. Correlation-only facts never enter this path.
                // A stored causal edge and a resolved note can name the same
                // parent; their shared endpoint appears only once.
                if let Some(notes) = anchored_notes {
                    for note in notes {
                        if let editchain_core::OpKind::Note(n) = &note.kind {
                            if !is_visible_edge_relationship(n.relationship) {
                                continue;
                            }
                            for target in &n.target_ids {
                                let key = NodeKey::Op(*target);
                                if seen.insert(key) {
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
            }
            | Self::PlanBundle {
                member_nodes,
                members,
                ..
            }
            | Self::WorkGroup {
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
                let member_keys: std::collections::HashSet<NodeKey> =
                    members.iter().map(|m| NodeKey::Op(m.id)).collect();
                let mut keys: Vec<NodeKey> = Vec::new();
                let mut seen: std::collections::HashSet<NodeKey> = std::collections::HashSet::new();
                for member in member_nodes {
                    for parent in member.parent_nodes(git_links, notes) {
                        if member_keys.contains(&parent) {
                            continue;
                        }
                        if seen.insert(parent) {
                            keys.push(parent);
                        }
                    }
                }
                keys
            }
            Self::GitCommit { commit, .. } => {
                let mut keys: Vec<NodeKey> = commit
                    .parents
                    .iter()
                    .map(|oid| NodeKey::Git(GitCommitKey::new(commit.repository, *oid)))
                    .collect();
                let mut seen: std::collections::HashSet<NodeKey> = keys.iter().copied().collect();
                for links in git_links.values() {
                    for link in links.iter().filter(|link| {
                        link.kind == GitLinkKind::ProducedBy
                            && link.target_repo == commit.repository
                            && link.target_oid == commit.oid
                    }) {
                        let key = NodeKey::Op(link.source);
                        if seen.insert(key) {
                            keys.push(key);
                        }
                    }
                }
                keys
            }
        }
    }

    /// Set authoritative typed parents for this derived view.
    ///
    /// Operation and Git source envelopes remain unchanged, including when a
    /// view contracts more than two parents or mixes Git and operation edges.
    pub fn override_parents(&mut self, keys: &[NodeKey]) {
        let mut seen = std::collections::HashSet::with_capacity(keys.len());
        let normalized = keys
            .iter()
            .copied()
            .filter(|key| seen.insert(*key))
            .collect();
        match self {
            Self::EditOperation {
                parent_override, ..
            }
            | Self::CollapsedImport {
                parent_override, ..
            }
            | Self::ExecuteBundle {
                parent_override, ..
            }
            | Self::PlanBundle {
                parent_override, ..
            }
            | Self::WorkGroup {
                parent_override, ..
            }
            | Self::GitCommit {
                parent_override, ..
            } => *parent_override = Some(normalized),
        }
    }

    /// Parents used by Activity passes after canonical edges have been frozen.
    /// Standalone advanced-pass callers retain source-parent behavior.
    pub(super) fn activity_parents(&self) -> Vec<NodeKey> {
        self.parent_nodes(&std::collections::BTreeMap::new(), &HashMap::new())
    }

    /// Returns the bundled metadata sub-ops attached to this node (empty for
    /// nodes without any). These are raw `Import` ops tagged `META` that carry
    /// no user-facing content; the viewer reveals them on click.
    #[must_use]
    pub fn sub_ops(&self) -> &[Arc<Op>] {
        match self {
            Self::CollapsedImport { sub_ops, .. } => sub_ops,
            Self::ExecuteBundle { members, .. }
            | Self::PlanBundle { members, .. }
            | Self::WorkGroup { members, .. } => members,
            Self::EditOperation { .. } | Self::GitCommit { .. } => &[],
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
            Self::CollapsedImport { kind, .. }
            | Self::ExecuteBundle { kind, .. }
            | Self::PlanBundle { kind, .. } => kind.clone(),
            Self::WorkGroup { .. } => "work-group".to_string(),
            Self::GitCommit { .. } => "git".to_string(),
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
            Self::CollapsedImport { meta, .. }
            | Self::ExecuteBundle { meta, .. }
            | Self::PlanBundle { meta, .. }
            | Self::WorkGroup { meta, .. } => *meta,
            Self::EditOperation { op, .. } => meta::for_edit_operation(op),
            Self::GitCommit { .. } => meta::for_git_commit(),
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

    /// The render prominence of this row (`Trace` rows are hidden from the
    /// Activity view).
    #[must_use]
    pub fn visibility(&self) -> RowVisibility {
        self.record_meta().visibility
    }

    /// The concluded outcome of this row, when structured evidence exists.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        self.record_meta().outcome
    }

    /// The reusable presentation state of this row and its child-owned edge.
    #[must_use]
    pub fn chain_state(&self) -> ChainState {
        self.record_meta().chain_state
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

/// Decode the effective source time of an op, distinguishing `Observed` from
/// `Unknown`.
///
/// Core owns unknown-time and legacy-zero interpretation. The stored clock
/// is never rewritten; this only adapts observed provenance for projection.
pub(super) fn source_time_of(op: &Op) -> EffectiveTime {
    op.observed_unix_ms()
        .map_or(EffectiveTime::Unknown, EffectiveTime::Observed)
}
