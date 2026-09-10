//! Canonical visible ancestry and exact structural relationship resolution.

use crate::node::HistoryNode;
use crate::{HistoryProjection, NodeKey, ResolvedRelation};
use editchain_core::op::NoteRelationship;
use editchain_core::{GitLinkKind, Op, OpId, Payload};
use std::collections::HashMap;

impl HistoryProjection {
    /// Canonicalize structural relationship notes for edge drawing.
    ///
    /// The raw [`Self::relationship_notes`] index is keyed by each note's stored
    /// causal parent — which may itself be a folded op (e.g. a `ReconnectsTo`
    /// anchored on a Tool op folded into its import, or a `SubagentOf` anchored on
    /// a subagent's first message). This re-keys every note by the canonical
    /// visible anchor (the representative of its stored parent), so the virtual
    /// edge is reachable from the row that represents the note's anchor. When a
    /// metadata anchor contracts into its own structural target, the note is
    /// additionally indexed on its unique first visible causal successor. That
    /// keeps the exact relation kind on the surviving branch-start row while the
    /// metadata remains bundled with its target. The provider entity targets
    /// have already been resolved to an unambiguous physical occurrence; direct
    /// physical targets stay unchanged. A copied
    /// occurrence suppressed by exact-equivalence contraction cannot contribute
    /// its `ProviderParent` edge to the surviving occurrence: doing so would
    /// union the incoming ancestry of separate physical transcripts and turn a
    /// normal chain into a false fan-in merge. Other structural relations remain
    /// eligible because they describe branch/lifecycle topology rather than the
    /// copied row's provider predecessor. Every edge-construction path then lifts
    /// folded targets through the canonical representative map. A note whose
    /// anchor cannot be resolved to a visible row is dropped.
    pub(super) fn canonicalize_relationship_notes(
        relationship_notes: &HashMap<OpId, Vec<Op>>,
        representative: &HashMap<OpId, OpId>,
        present: &std::collections::HashSet<String>,
        duplicate_event_occurrences: &std::collections::HashSet<OpId>,
        ops: &[Op],
    ) -> HashMap<OpId, Vec<Op>> {
        let mut out: HashMap<OpId, Vec<Op>> = HashMap::new();
        let mut causal_children: Option<HashMap<OpId, Vec<OpId>>> = None;
        for (stored_anchor, notes) in relationship_notes {
            let Some(anchor) = canonical_op_id(*stored_anchor, representative, present) else {
                continue;
            };
            for note in notes.iter().cloned() {
                let suppressed_copy_parent = *stored_anchor != anchor
                    && duplicate_event_occurrences.contains(stored_anchor)
                    && matches!(
                        &note.kind,
                        editchain_core::OpKind::Note(fact)
                            if fact.relationship == NoteRelationship::ProviderParent
                    );
                if suppressed_copy_parent {
                    continue;
                }
                // `Contains` participates in entity endpoint resolution during
                // collapse but is neither a row marker nor a display edge.
                if !matches!(
                    &note.kind,
                    editchain_core::OpKind::Note(fact)
                        if fact.relationship == NoteRelationship::Contains
                ) {
                    out.entry(anchor).or_default().push(note.clone());
                    let successor = if protected_relation_targets_anchor(
                        &note,
                        anchor,
                        representative,
                        present,
                    ) {
                        let children =
                            causal_children.get_or_insert_with(|| causal_children_by_parent(ops));
                        unique_visible_successor(
                            *stored_anchor,
                            anchor,
                            children,
                            representative,
                            present,
                        )
                    } else {
                        None
                    };
                    if let Some(successor) = successor.filter(|successor| *successor != anchor) {
                        out.entry(successor).or_default().push(note);
                    }
                }
            }
        }
        // Deterministic order per anchor (HashMap iteration order is
        // process-random; parent_keys emits virtual targets in list order).
        for notes in out.values_mut() {
            notes.sort_unstable_by_key(|n| (n.id.node.0, n.id.boot, n.id.seq));
        }
        out
    }

    /// Return this node's parent keys resolved to their canonical visible rows, so
    /// a parent that was folded away (bundled META op, normalized child, tool
    /// result, relationship fact, copied occurrence, ...) is redirected to the row that
    /// represents it. Unresolvable op parents are dropped.
    ///
    /// Used by the service when emitting `HistoryRow::parents` so the client's
    /// chain assembly sees connected chains, not dangling folded-op parents.
    #[must_use]
    pub fn lifted_parent_keys(&self, node: &HistoryNode) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        node.parent_nodes(self.git.links(), self.relationship_notes())
            .into_iter()
            .filter_map(|parent| {
                let resolved = match parent {
                    NodeKey::Op(id) => self.visible_op_id(id).map(NodeKey::Op),
                    NodeKey::Git(key) => self
                        .git
                        .commits()
                        .contains_key(&(key.repository, key.oid))
                        .then_some(parent),
                }?;
                (resolved != node.key() && seen.insert(resolved)).then(|| resolved.to_string())
            })
            .collect()
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
        let parents: Vec<NodeKey> = parents
            .iter()
            .filter_map(|key| NodeKey::from_display_str(key))
            .collect();
        self.resolved_relations_for(node, &parents)
            .into_iter()
            .map(|relation| ParentRelation {
                parent: relation.parent.to_string(),
                kind: relation.kind,
            })
            .collect()
    }

    pub(super) fn resolved_relations_for(
        &self,
        node: &HistoryNode,
        parents: &[NodeKey],
    ) -> Vec<ResolvedRelation> {
        let mut relations: Vec<ResolvedRelation> = Vec::new();
        let mut record = |parent, kind, evidence| {
            if let Some(relation) = relations
                .iter_mut()
                .find(|relation| relation.parent == parent && relation.kind == kind)
            {
                if !relation.evidence.contains(&evidence) {
                    relation.evidence.push(evidence);
                }
            } else {
                relations.push(ResolvedRelation {
                    parent,
                    kind,
                    evidence: vec![evidence],
                });
            }
        };
        if let HistoryNode::GitCommit { commit, .. } = node {
            for &parent in parents {
                for link in self.git.links().values().flatten().filter(|link| {
                    link.kind == GitLinkKind::ProducedBy
                        && link.target_key() == commit.key()
                        && self
                            .visible_op_id(link.source)
                            .is_some_and(|source| NodeKey::Op(source) == parent)
                }) {
                    record(parent, RelationKind::ProducedCommit, link.source);
                }
            }
        } else if let Some(notes) = node
            .op_id()
            .and_then(|id| self.relationship_notes().get(&id))
        {
            for &parent in parents {
                for note in notes {
                    let editchain_core::OpKind::Note(fact) = &note.kind else {
                        continue;
                    };
                    let Some(kind) = relation_kind(fact.relationship) else {
                        continue;
                    };
                    if fact.target_ids.iter().any(|target| {
                        self.visible_op_id(*target)
                            .is_some_and(|id| NodeKey::Op(id) == parent)
                    }) {
                        record(parent, kind, note.id);
                    }
                }
            }
        }
        relations
    }

    /// Returns the node keys of rows that participate in structural topology
    /// (`ForkOf` / `SubagentOf` / `ReconnectsTo` anchors or targets, or either
    /// endpoint of a produced-commit edge).
    ///
    /// Structural rows carry virtual edges, so a view must never fold them away:
    /// the Activity execute-run bundling excludes them exactly like the chain
    /// Activity view preserves them from trace removal. Targets are lifted to
    /// their canonical visible rows (or dropped when unresolvable in this view).
    #[must_use]
    pub fn structural_row_keys(&self, nodes: &[HistoryNode]) -> std::collections::HashSet<NodeKey> {
        let mut keys = std::collections::HashSet::new();
        let note_map = self.relationship_notes();
        let representative = &self.collapsed_projection.representative;
        let present: std::collections::HashSet<NodeKey> =
            nodes.iter().map(HistoryNode::key).collect();
        for link in self.git.links().values().flatten().filter(|link| {
            link.kind == GitLinkKind::ProducedBy
                && present.contains(&NodeKey::Git(link.target_key()))
        }) {
            if let Some(source) = canonical_op_id(
                link.source,
                representative,
                &self.collapsed_projection.present,
            ) {
                let _: bool = keys.insert(NodeKey::Op(source));
            }
            let _: bool = keys.insert(NodeKey::Git(link.target_key()));
        }
        for node in nodes {
            let Some(anchor_id) = node.op_id() else {
                continue;
            };
            if let Some(anchor_notes) = note_map.get(&anchor_id) {
                for note in anchor_notes {
                    let editchain_core::OpKind::Note(n) = &note.kind else {
                        continue;
                    };
                    if !is_protected_structural_relationship(n.relationship) {
                        continue;
                    }
                    let _: bool = keys.insert(node.key());
                    for target in &n.target_ids {
                        if let Some(key) = canonical_ordering_op(*target, representative, &present)
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
        NoteRelationship::SubagentOf | NoteRelationship::SpawnedBy => Some(RelationKind::Subagent),
        NoteRelationship::ReconnectsTo => Some(RelationKind::Reconnect),
        NoteRelationship::ForkOf => Some(RelationKind::Fork),
        NoteRelationship::Corrects
        | NoteRelationship::Supersedes
        | NoteRelationship::Rejects
        | NoteRelationship::Redacts
        | NoteRelationship::Explains
        | NoteRelationship::OccurrenceOf
        | NoteRelationship::ProviderParent
        | NoteRelationship::LogicalParent
        | NoteRelationship::ForkedFrom
        | NoteRelationship::Contains
        | NoteRelationship::ToolResultOf
        | NoteRelationship::ProviderEvidence => None,
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

/// Provider-neutral structural relationship kinds, derived from exact stored
/// relationship facts and Git links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// The row starts a subagent branch spawned by the target row.
    Subagent,
    /// The row is the parent thread's completion result returning into the
    /// subagent branch (the target row is the subagent's last op).
    Reconnect,
    /// The row branches off the target row at a fork divergence boundary.
    Fork,
    /// A Git commit was produced by the parent operation.
    ProducedCommit,
}

/// Resolve provider entity handles in cloned relation facts without choosing an
/// arbitrary occurrence.
///
/// The note's physical anchor supplies exact source context. If that source has
/// one occurrence (after exact-payload duplicate contraction), it is the
/// endpoint. With no same-source occurrence, the global set must likewise
/// converge to one exact-equivalence class. Otherwise the target is removed and
/// the relation stays inert in this projection.
pub(super) fn resolve_relationship_note_targets(
    relationship_notes: &HashMap<OpId, Vec<Op>>,
    entity_occurrences: &HashMap<OpId, Vec<OpId>>,
    representative: &HashMap<OpId, OpId>,
) -> HashMap<OpId, Vec<Op>> {
    let mut resolved = HashMap::with_capacity(relationship_notes.len());
    for (anchor, notes) in relationship_notes {
        let mut resolved_notes = Vec::with_capacity(notes.len());
        for note in notes {
            let mut note = note.clone();
            if let editchain_core::OpKind::Note(fact) = &mut note.kind {
                let mut targets = std::collections::BTreeSet::new();
                for target in &fact.target_ids {
                    let endpoint =
                        entity_occurrences
                            .get(target)
                            .map_or(Some(*target), |occurrences| {
                                resolve_entity_occurrence(*anchor, occurrences, representative)
                            });
                    if let Some(endpoint) = endpoint {
                        let _: bool = targets.insert(endpoint);
                    }
                }
                fact.target_ids = targets.into_iter().collect();
            }
            resolved_notes.push(note);
        }
        drop(resolved.insert(*anchor, resolved_notes));
    }
    resolved
}

/// Resolve one entity's occurrences using exact source identity and exact
/// duplicate representatives.
fn resolve_entity_occurrence(
    anchor: OpId,
    occurrences: &[OpId],
    representative: &HashMap<OpId, OpId>,
) -> Option<OpId> {
    let same_source: std::collections::BTreeSet<OpId> = occurrences
        .iter()
        .filter(|occurrence| occurrence.node == anchor.node && occurrence.boot == anchor.boot)
        .filter_map(|occurrence| occurrence_representative(*occurrence, representative))
        .collect();
    if same_source.len() == 1 {
        return same_source.into_iter().next();
    }
    if !same_source.is_empty() {
        return None;
    }

    let global: std::collections::BTreeSet<OpId> = occurrences
        .iter()
        .filter_map(|occurrence| occurrence_representative(*occurrence, representative))
        .collect();
    (global.len() == 1)
        .then(|| global.into_iter().next())
        .flatten()
}

/// Chase only exact-equivalence representatives. A cycle is unresolved.
fn occurrence_representative(
    mut occurrence: OpId,
    representative: &HashMap<OpId, OpId>,
) -> Option<OpId> {
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(occurrence) {
            return None;
        }
        match representative.get(&occurrence).copied() {
            Some(next) if next != occurrence => occurrence = next,
            Some(_) => return None,
            None => return Some(occurrence),
        }
    }
}

/// Whether `anchor` has a resolved exact provider parent.
///
/// Canonical note indexes can contain facts originally anchored on folded
/// sub-ops. Checking the note's stored parent prevents one bundled member's
/// `ProviderParent` fact from changing the parent domain of the visible anchor.
/// An empty target set means provider resolution failed, so source order remains
/// the conservative fallback.
pub(super) fn has_exact_provider_parent(anchor: OpId, notes: Option<&[Op]>) -> bool {
    notes.is_some_and(|facts| {
        facts.iter().any(|fact| {
            matches!(
                &fact.kind,
                editchain_core::OpKind::Note(note)
                    if note.relationship == NoteRelationship::ProviderParent
                        && !note.target_ids.is_empty()
                        && fact.parents.iter().any(|parent| *parent == anchor)
            )
        })
    })
}

/// Whether `anchor` has one exact subagent spawn parent.
///
/// A child rollout inherits the parent's session-start Git snapshot. Once its
/// exact activation is known, that inherited `BasedOn` fact is provenance, not
/// an additional display-graph parent: the execution branch starts at the
/// spawn occurrence.
pub(super) fn has_exact_spawn_parent(anchor: OpId, notes: Option<&[Op]>) -> bool {
    notes.is_some_and(|facts| {
        facts.iter().any(|fact| {
            matches!(
                &fact.kind,
                editchain_core::OpKind::Note(note)
                    if note.relationship == NoteRelationship::SpawnedBy
                        && !note.target_ids.is_empty()
                        && fact.parents.iter().any(|parent| *parent == anchor)
            )
        })
    })
}

/// Resolve an op/entity handle through representatives to one currently
/// present operation row. Cyclic representative maps are unresolved.
pub(super) fn canonical_present_op(
    mut id: OpId,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<OpId>,
) -> Option<OpId> {
    let mut seen = std::collections::HashSet::new();
    loop {
        if present.contains(&id) {
            return Some(id);
        }
        if !seen.insert(id) {
            return None;
        }
        match representative.get(&id).copied() {
            Some(next) if next != id => id = next,
            Some(_) | None => return None,
        }
    }
}

/// Resolve one operation id through folded representatives for scheduling.
fn canonical_ordering_op(
    mut id: OpId,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<NodeKey>,
) -> Option<NodeKey> {
    // At most one representative can be traversed per map entry; the extra
    // lookup permits the terminal row. Cycles remain unresolved without a
    // per-edge allocation for a visited set.
    for _ in 0..=representative.len() {
        let key = NodeKey::Op(id);
        if present.contains(&key) {
            return Some(key);
        }
        match representative.get(&id).copied() {
            Some(next) if next != id => id = next,
            Some(_) | None => return None,
        }
    }
    None
}

/// Canonical typed edges shared by scheduling, filtering, and geometry.
fn canonical_node_parents(
    parents: Vec<NodeKey>,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<NodeKey>,
    child: NodeKey,
) -> Vec<NodeKey> {
    let mut seen = std::collections::HashSet::new();
    parents
        .into_iter()
        .filter_map(|parent| {
            let resolved = match parent {
                NodeKey::Op(id) => canonical_ordering_op(id, representative, present),
                NodeKey::Git(_) => present.contains(&parent).then_some(parent),
            }?;
            (resolved != child && seen.insert(resolved)).then_some(resolved)
        })
        .collect()
}

/// Build canonical typed parents through the common node contract.
pub(super) fn ordering_parent_keys(
    node: &HistoryNode,
    git_links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
    notes: &HashMap<OpId, Vec<Op>>,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<NodeKey>,
) -> Vec<NodeKey> {
    canonical_node_parents(
        node.parent_nodes(git_links, notes),
        representative,
        present,
        node.key(),
    )
}

/// Collect the node keys of collapsed top-level rows (the "present" set used to
/// decide whether a parent resolves to a rendered row).
pub(super) fn row_node_keys(nodes: &[HistoryNode]) -> std::collections::HashSet<String> {
    nodes.iter().map(HistoryNode::node_key).collect()
}

/// Resolve each parent key to its canonical visible row key through the
/// representative map, dropping keys that cannot be resolved to a rendered row.
///
/// A parent that was folded into a bundle (a META sub-op, a normalized child, a
/// tool result, copied occurrence, or relationship fact) is redirected to the visible
/// row that represents it. A key that still fails to resolve — an op id absent
/// from the projection with no representative — is dropped so it can never reach
/// lane allocation or windowed edge geometry as a phantom. Non-op keys (git OID
/// hex) are external anchors and are kept for the caller's row lookup. The
/// result is deduplicated preserving first-occurrence order. This never rewrites
/// stored `Op.parents`; it is a layout/view-time lift only.
pub(super) fn canonicalize_parents(
    parents: Vec<String>,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<String>,
    child_key: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(parents.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for parent in parents {
        if let Some(key) = canonical_parent_key(&parent, representative, present) {
            if key != child_key && seen.insert(key.clone()) {
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

/// Index ordinary causal children without treating relationship facts as
/// transcript continuations.
fn causal_children_by_parent(ops: &[Op]) -> HashMap<OpId, Vec<OpId>> {
    let mut children: HashMap<OpId, Vec<OpId>> = HashMap::new();
    for op in ops.iter().filter(|op| !is_hidden_relation_fact(op)) {
        for parent in &op.parents {
            children.entry(*parent).or_default().push(op.id);
        }
    }
    children
}

/// Whether an exact structural note would collapse into a self-edge at
/// `anchor` after canonical endpoint lifting.
fn protected_relation_targets_anchor(
    note: &Op,
    anchor: OpId,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<String>,
) -> bool {
    let editchain_core::OpKind::Note(fact) = &note.kind else {
        return false;
    };
    is_protected_structural_relationship(fact.relationship)
        && fact
            .target_ids
            .iter()
            .any(|target| canonical_op_id(*target, representative, present) == Some(anchor))
}

/// Find one unambiguous visible row immediately downstream of a folded anchor.
///
/// Traversal may cross operations represented by `collapsed_anchor`, but stops
/// at the first distinct visible representative on every path. More than one
/// such row is ambiguous and deliberately produces no structural redirect.
fn unique_visible_successor(
    stored_anchor: OpId,
    collapsed_anchor: OpId,
    causal_children: &HashMap<OpId, Vec<OpId>>,
    representative: &HashMap<OpId, OpId>,
    present: &std::collections::HashSet<String>,
) -> Option<OpId> {
    let mut pending = vec![stored_anchor];
    let mut visited = std::collections::HashSet::from([stored_anchor]);
    let mut candidates = std::collections::BTreeSet::new();
    while let Some(parent) = pending.pop() {
        let Some(children) = causal_children.get(&parent) else {
            continue;
        };
        for child in children {
            if !visited.insert(*child) {
                continue;
            }
            let child_is_visible = present.contains(&child.to_string());
            match canonical_op_id(*child, representative, present) {
                Some(candidate) if candidate != collapsed_anchor => {
                    let _: bool = candidates.insert(candidate);
                    if candidates.len() > 1 {
                        return None;
                    }
                }
                Some(_) if !child_is_visible => pending.push(*child),
                None => pending.push(*child),
                Some(_) => {}
            }
        }
    }
    candidates.into_iter().next()
}

/// Chase an `OpId` through the representative map until it reaches a visible row.
///
/// Returns `None` when the id is neither a row nor mapped (directly or through a
/// chain) to a row — the caller drops such ids instead of emitting phantom keys.
pub(super) fn canonical_op_id(
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

/// Whether a relationship contributes a virtual visible graph edge.
///
/// `ToolResultOf` deliberately is not graph-bearing: it correlates one result
/// envelope with one or more calls while `ProviderParent` independently carries
/// that envelope's conversation ancestry.
pub(super) fn is_visible_edge_relationship(relationship: NoteRelationship) -> bool {
    matches!(
        relationship,
        NoteRelationship::ForkOf
            | NoteRelationship::SubagentOf
            | NoteRelationship::ReconnectsTo
            | NoteRelationship::ProviderParent
            | NoteRelationship::SpawnedBy
    )
}

/// Branch/lifecycle edges whose endpoint rows must survive semantic filters.
/// Ordinary provider-parent and tool-correlation edges can be spliced exactly
/// like stored source parents and therefore do not pin every conversation row.
fn is_protected_structural_relationship(relationship: NoteRelationship) -> bool {
    matches!(
        relationship,
        NoteRelationship::ForkOf
            | NoteRelationship::SubagentOf
            | NoteRelationship::ReconnectsTo
            | NoteRelationship::SpawnedBy
    )
}

/// Recognize unversioned relationship notes emitted by retired importers.
///
/// The old Claude and Codex post-passes wrote evidence-free imported META notes.
/// Some were exact, some selected endpoints from timestamp/prefix/content
/// heuristics, and the stored records do not identify which resolver produced
/// which result. Existing immutable chains retain those notes, but the current
/// projection cannot treat an unversioned, evidence-free fact as provenance.
/// Versioned exact facts carry a non-empty evidence payload and remain active.
///
/// This compatibility rule is intentionally schema-based: it does not inspect
/// timestamps, op-id lanes, content similarity, or neighboring records.
fn is_legacy_unversioned_import_relationship(op: &Op) -> bool {
    let editchain_core::OpKind::Note(note) = &op.kind else {
        return false;
    };
    if !op
        .tags
        .matches_all(editchain_core::Tags::META | editchain_core::Tags::IMPORT)
        || !matches!(note.content, Payload::Empty)
    {
        return false;
    }
    matches!(
        note.relationship,
        NoteRelationship::ForkOf | NoteRelationship::SubagentOf | NoteRelationship::ReconnectsTo
    )
}

/// Whether a relation fact is indexed for identity/correlation resolution or
/// visible edges.
pub(super) fn is_projected_relation_fact(op: &Op) -> bool {
    if is_legacy_unversioned_import_relationship(op) {
        return false;
    }
    matches!(
        &op.kind,
        editchain_core::OpKind::Note(note)
            if is_visible_edge_relationship(note.relationship)
                || matches!(
                    note.relationship,
                    NoteRelationship::OccurrenceOf
                        | NoteRelationship::Contains
                        | NoteRelationship::ToolResultOf
                )
    )
}

/// Whether a relation fact is bookkeeping rather than a prose history row.
pub(super) fn is_hidden_relation_fact(op: &Op) -> bool {
    if is_legacy_unversioned_import_relationship(op) {
        return true;
    }
    matches!(
        &op.kind,
        editchain_core::OpKind::Note(note)
            if is_visible_edge_relationship(note.relationship)
                || matches!(
                    note.relationship,
                    NoteRelationship::OccurrenceOf
                        | NoteRelationship::LogicalParent
                        | NoteRelationship::ForkedFrom
                        | NoteRelationship::Contains
                        | NoteRelationship::ToolResultOf
                        | NoteRelationship::ProviderEvidence
                )
    )
}
