//! Deterministic Activity-view semantics on top of the canonical projection.
//!
//! This module owns the four fixed-view behaviors the unified history service
//! emits for its Activity profile (and only for that profile):
//!
//! - **Inline context checkpoints**
//!   ([`inline_context_compaction_checkpoints`]): a raw Codex context-compaction
//!   row remains visible but is inserted into its source stream's existing
//!   path when legacy/imported topology stored it beside the continuation.
//! - **Work-unit markers** ([`annotate_activity_rows`]): every row carries an
//!   opaque unit id plus view-stable `is_start`/`is_end`/`count`/`title`, so a
//!   client renders unit boundaries without inferring across paged windows.
//!   Units are logical and view-wide: every row sharing an id forms one unit
//!   even when other units' rows interleave, so interleaved chains never
//!   fragment into per-segment boundary noise.
//! - **Conservative promotion** ([`ActivityRowAnnotation::promoted`]):
//!   negative-outcome rows, change/verify rows, and each unit's deterministically
//!   known newest narrative row are flagged as significant; promoted rows are
//!   never folded away by bundling.
//! - **Execute-run bundling** ([`bundle_activity_execute_runs`]): maximal
//!   contiguous runs of at least two safe low-signal execute rows collapse into
//!   one synthetic [`HistoryNode::ExecuteBundle`] whose members stay expandable
//!   through the existing sub-ops model, so paging indices and virtualization
//!   work unchanged. Raw profiles never invoke bundling and stay exact/ordered.
//!
//! Conservative guards keep evidence visible: runs never cross turn or group
//! boundaries, are built from display-order contiguity only (never timestamps),
//! exclude rows owning `world_state`/`turn_context` state sub-ops, never sit
//! adjacent (same turn) to a change row, treat verify/change rows as
//! unconditional breakers and promotion exclusions, and never fold rows that
//! carry fork/subagent/reconnect structural edges.

use std::collections::{HashMap, HashSet};

use editchain_core::{
    Clock, NodeId, Op, OpId, OpKind, ParentSet, ScopeRef, Tags, TurnId, UnknownOp,
};

use crate::meta::{is_context_compaction_import, sub_op_is_world_state_or_turn_context, NodeMeta};
use crate::taxonomy::{ActivityKind, Outcome, RecordRole, Visibility};
use crate::{EffectiveTime, HistoryNode};

/// Keep raw context-compaction checkpoints visible while making them inline in
/// the Activity view's source-stream path.
///
/// Some previously imported Codex histories store a `type: "compacted"` raw
/// row and the next raw row as siblings of one causal parent. That exact shape
/// makes a context checkpoint look like a one-row fork even though it records
/// no execution branch. This Activity-only pass rewrites the next visible raw
/// row to point through the checkpoint when all of these structural facts hold:
///
/// - the checkpoint is identified by its raw JSON envelope, never its summary;
/// - it and the immediately following visible raw row have the same source
///   `(node, boot)`, session scope, and sole stored parent;
/// - the checkpoint does not already have a stored child; and
/// - neither endpoint participates in a fork/subagent/reconnect relationship.
///
/// Already-linear checkpoints and ambiguous/structural shapes are unchanged.
/// The caller supplies cloned Activity rows, so canonical/Raw topology remains
/// byte-faithful.
#[must_use]
pub fn inline_context_compaction_checkpoints<S: std::hash::BuildHasher>(
    mut nodes: Vec<HistoryNode>,
    structural_keys: &HashSet<String, S>,
) -> Vec<HistoryNode> {
    let mut streams: HashMap<(u64, u32), Vec<RawRowFacts>> = HashMap::new();
    let mut stored_parents = HashSet::new();
    for (index, node) in nodes.iter().enumerate() {
        if let Some(op) = node_anchor_op(node) {
            stored_parents.extend(op.parents.iter().copied());
        }
        if let Some(facts) = raw_row_facts(node, index) {
            streams
                .entry((facts.id.node.0, facts.id.boot))
                .or_default()
                .push(facts);
        }
    }

    let mut rewrites = Vec::new();
    for stream in streams.values_mut() {
        stream.sort_unstable_by_key(|facts| facts.id.seq);
        for pair in stream.windows(2) {
            let [checkpoint, continuation] = pair else {
                continue;
            };
            if !checkpoint.is_compaction
                || checkpoint.scope != continuation.scope
                || checkpoint.sole_parent.is_none()
                || checkpoint.sole_parent != continuation.sole_parent
                || stored_parents.contains(&checkpoint.id)
                || structural_keys.contains(&checkpoint.id.to_string())
                || structural_keys.contains(&continuation.id.to_string())
            {
                continue;
            }
            let Some(parent) = checkpoint.sole_parent else {
                continue;
            };
            if parent.node != checkpoint.id.node || parent.boot != checkpoint.id.boot {
                continue;
            }
            rewrites.push((continuation.index, checkpoint.id));
        }
    }

    for (index, checkpoint) in rewrites {
        if let Some(continuation) = nodes.get_mut(index) {
            continuation.set_parent_keys(&[checkpoint.to_string()]);
        }
    }
    nodes
}

/// Exact source-stream facts needed by context-checkpoint inlining.
#[derive(Debug, Clone, Copy)]
struct RawRowFacts {
    index: usize,
    id: OpId,
    scope: ScopeRef,
    sole_parent: Option<OpId>,
    is_compaction: bool,
}

/// Extract facts only from raw import rows; normalized or synthetic rows can
/// never become a context-checkpoint continuation.
#[must_use]
fn raw_row_facts(node: &HistoryNode, index: usize) -> Option<RawRowFacts> {
    let HistoryNode::CollapsedImport { op, .. } = node else {
        return None;
    };
    if !matches!(&op.kind, OpKind::Import(_)) {
        return None;
    }
    let mut parents = op.parents.iter().copied();
    let first_parent = parents.next();
    let sole_parent = first_parent.filter(|_| parents.next().is_none());
    Some(RawRowFacts {
        index,
        id: op.id,
        scope: op.scope,
        sole_parent,
        is_compaction: is_context_compaction_import(op.as_ref()),
    })
}

/// Stable metadata for the work unit one view row belongs to.
///
/// Mirrored to the protocol as `WorkUnitDto`; `id` is opaque and only compared
/// for equality. Units are view-wide logical groups: every row sharing an id
/// forms one unit even when other units' rows interleave in display order, so
/// exactly one `is_start`/`is_end` pair and full-view `count`/`title` are
/// stable across paged windows by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkUnitMarker {
    /// Opaque work-unit id (a turn identity for turn-scoped rows, else the
    /// row's group key).
    pub id: String,
    /// Whether this row is the FIRST row of the unit in display order.
    pub is_start: bool,
    /// Whether this row is the LAST row of the unit in display order.
    pub is_end: bool,
    /// Deterministic unit title when evidence supports one (the unit's oldest
    /// primary narrative row summary, e.g. the initiating request of a turn).
    pub title: Option<String>,
    /// Total top-level rows in this unit for the current view (stable across
    /// windows because the view, not the window, defines the count).
    pub count: u64,
}

/// Per-row Activity-view annotation, parallel to the annotated node list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRowAnnotation {
    /// Conservative promotion marker (see module docs).
    pub promoted: bool,
    /// Work-unit metadata for this row's unit.
    pub work_unit: WorkUnitMarker,
}

/// Annotate every row with its deterministic work-unit marker and promotion
/// flag.
///
/// Units are view-wide: rows of one id need not be display-contiguous, and
/// interleaved rows are only annotated, never reordered or gathered. Each id
/// yields exactly one `is_start` (first display-order occurrence), one
/// `is_end` (last display-order occurrence), and a `count` of every top-level
/// row with that id in the full view. The result parallels `nodes` 1:1 and is
/// deterministic for a given node list (the caller supplies the exact
/// filtered/bundled list it will render, so boundaries, counts, and titles
/// never depend on window size or scroll position).
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "unit aggregate indices come from enumerate() over the ids/nodes vectors built in this pass, so they are bounded by construction"
)]
pub fn annotate_activity_rows(nodes: &[HistoryNode]) -> Vec<ActivityRowAnnotation> {
    // View-wide logical units: aggregate every row that shares a unit id,
    // regardless of interleaving, so one id yields exactly one boundary pair.
    struct UnitAgg {
        first: usize,
        last: usize,
        count: u64,
        title: Option<String>,
        final_narrative: Option<usize>,
    }
    let ids: Vec<String> = nodes.iter().map(unit_id).collect();
    let mut units: HashMap<&str, UnitAgg> = HashMap::with_capacity(ids.len());
    for (index, id) in ids.iter().enumerate() {
        let unit = units.entry(id.as_str()).or_insert_with(|| UnitAgg {
            first: index,
            last: index,
            count: 0,
            title: None,
            final_narrative: None,
        });
        unit.last = index;
        unit.count = unit.count.saturating_add(1);
        if is_narrative_row(&nodes[index]) {
            // The unit-final narrative/final decision is the newest primary
            // narrative row (first in display order); the first encounter wins
            // because the walk is newest-first.
            if unit.final_narrative.is_none() {
                unit.final_narrative = Some(index);
            }
            // The unit title is the oldest primary narrative row
            // (chronologically first = last in newest-first display order);
            // the last encounter wins for the same reason.
            unit.title = Some(nodes[index].summary());
        }
    }
    let mut out = Vec::with_capacity(nodes.len());
    for (index, id) in ids.iter().enumerate() {
        let unit = &units[id.as_str()];
        out.push(ActivityRowAnnotation {
            promoted: row_promoted(&nodes[index], unit.final_narrative == Some(index)),
            work_unit: WorkUnitMarker {
                id: id.clone(),
                is_start: index == unit.first,
                is_end: index == unit.last,
                title: unit.title.clone(),
                count: unit.count,
            },
        });
    }
    out
}

/// Collapse maximal contiguous runs of at least two safe low-signal execute
/// rows into synthetic [`HistoryNode::ExecuteBundle`] nodes.
///
/// A member is eligible only when it is [`ActivityKind::Execute`] with
/// [`Visibility::Primary`], has no warning/failure/cancelled outcome (unknown
/// outcome is eligible), is not promoted, carries no `world_state`/`turn_context`
/// sub-op, and participates in no structural fork/subagent/reconnect edge.
/// Runs are maximal contiguous display-order spans within one turn and one
/// group (never gathered across intervening rows, never built from
/// timestamps). A run is rejected when it sits immediately adjacent (same
/// turn) to an [`ActivityKind::Change`] row on either side; verify rows are
/// unconditional breakers. Folded members remain expandable through the
/// bundle's `sub_ops`, and surviving rows' parents are rewired to the bundle's
/// key so the layout stays connected.
///
/// The pass is O(V): per-node facts (key, group, turn, state-sub-op flag) are
/// precomputed once, so eligibility and run formation never re-format node
/// keys/groups or re-parse sub-op JSON per candidate.
#[must_use]
#[expect(
    clippy::implicit_hasher,
    clippy::indexing_slicing,
    reason = "structural row keys keep the default RandomState hasher like the projection note map; run scan indices are bounds-checked by the while conditions against nodes.len()"
)]
pub fn bundle_activity_execute_runs(
    nodes: Vec<HistoryNode>,
    annotations: &[ActivityRowAnnotation],
    structural_keys: &HashSet<String>,
) -> Vec<HistoryNode> {
    debug_assert_eq!(
        nodes.len(),
        annotations.len(),
        "annotations must parallel the node list 1:1"
    );
    let facts: Vec<MemberFacts> = nodes
        .iter()
        .zip(annotations)
        .map(|(node, _)| MemberFacts {
            key: node.node_key(),
            group: node.group(),
            turn: node.turn_id(),
            owns_state_subop: node_owns_state_sub_op(node),
        })
        .collect();
    let eligible: Vec<bool> = nodes
        .iter()
        .zip(annotations)
        .zip(&facts)
        .map(|((node, annotation), fact)| {
            node.activity_kind() == ActivityKind::Execute
                && node.visibility() == Visibility::Primary
                && !matches!(
                    node.outcome(),
                    Outcome::Warning | Outcome::Failure | Outcome::Cancelled
                )
                && !annotation.promoted
                && !structural_keys.contains(&fact.key)
                && !fact.owns_state_subop
        })
        .collect();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut idx = 0usize;
    while idx < nodes.len() {
        if !eligible[idx] {
            idx = idx.saturating_add(1);
            continue;
        }
        let Some(turn) = facts[idx].turn else {
            idx = idx.saturating_add(1);
            continue;
        };
        let group = facts[idx].group.as_str();
        let mut end = idx;
        while end.saturating_add(1) < nodes.len()
            && eligible[end.saturating_add(1)]
            && facts[end.saturating_add(1)].turn == Some(turn)
            && facts[end.saturating_add(1)].group == group
        {
            end = end.saturating_add(1);
        }
        if end.saturating_sub(idx).saturating_add(1) >= 2
            && !run_adjacent_to_change(&nodes, idx, end)
        {
            runs.push((idx, end));
        }
        idx = end.saturating_add(1);
    }
    contract_runs(nodes, runs)
}

/// Per-node facts precomputed once for the run scan, so membership checks do
/// no repeated string formatting or sub-op JSON parsing.
struct MemberFacts {
    /// Node key (op id or commit OID hex).
    key: String,
    /// Display group (session/repo/ops), precomputed once per node.
    group: String,
    /// Owning turn identity (runs never cross turns).
    turn: Option<TurnId>,
    /// Whether the node owns a `world_state`/`turn_context` sub-op.
    owns_state_subop: bool,
}

/// Rewire parents after contraction and rebuild the node list.
///
/// `nodes` is consumed slot-by-slot so members move into the bundle and kept
/// rows move without cloning payloads (same pattern as the ordered-node
/// scheduler).
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "slot indices come from run spans bounded by slots.len(); slicing is range-checked by the loop over the full list"
)]
fn contract_runs(nodes: Vec<HistoryNode>, runs: Vec<(usize, usize)>) -> Vec<HistoryNode> {
    if runs.is_empty() {
        return nodes;
    }
    let mut slots: Vec<Option<HistoryNode>> = nodes.into_iter().map(Some).collect();
    let mut result: Vec<HistoryNode> = Vec::with_capacity(slots.len());
    let mut bundle_of_member: HashMap<String, String> = HashMap::new();
    let mut run_iter = runs.into_iter().peekable();
    for idx in 0..slots.len() {
        if let Some((start, end)) = run_iter.next_if(|&(start, _)| start == idx) {
            let members: Vec<HistoryNode> = slots[start..=end]
                .iter_mut()
                .filter_map(Option::take)
                .collect();
            let bundle_key = members
                .first()
                .map_or_else(String::new, HistoryNode::node_key);
            for member in &members {
                drop(bundle_of_member.insert(member.node_key(), bundle_key.clone()));
            }
            result.push(build_bundle(members));
        } else if let Some(node) = slots[idx].take() {
            result.push(node);
        }
    }
    if !bundle_of_member.is_empty() {
        for node in &mut result {
            // Bundles read their parents through an intra-run filter and git
            // rows never reference op members; only stored op parents of kept
            // rows need the rewrite.
            if matches!(
                node,
                HistoryNode::ExecuteBundle { .. } | HistoryNode::GitCommit(_)
            ) {
                continue;
            }
            let old = stored_parent_keys(node);
            if old.is_empty() {
                continue;
            }
            let mut keys = old;
            let mut changed = false;
            for key in &mut keys {
                if let Some(replacement) = bundle_of_member.get(key.as_str()) {
                    *key = replacement.clone();
                    changed = true;
                }
            }
            if !changed {
                continue;
            }
            let mut seen = HashSet::with_capacity(keys.len());
            keys.retain(|key| seen.insert(key.clone()));
            node.set_parent_keys(&keys);
        }
    }
    result
}

/// The stored causal parent keys of an op-backed row (empty otherwise).
#[must_use]
fn stored_parent_keys(node: &HistoryNode) -> Vec<String> {
    match node {
        HistoryNode::EditOperation { op, .. } | HistoryNode::CollapsedImport { op, .. } => {
            op.parents.iter().map(ToString::to_string).collect()
        }
        HistoryNode::ExecuteBundle { .. } | HistoryNode::GitCommit(_) => Vec::new(),
    }
}

/// Build one synthetic summary node from a run's member rows (newest-first).
#[must_use]
fn build_bundle(members: Vec<HistoryNode>) -> HistoryNode {
    let newest = members.first();
    let anchor = newest
        .and_then(node_anchor_op)
        .cloned()
        .unwrap_or_else(empty_anchor_op);
    let source_time = newest.map_or(EffectiveTime::Unknown, HistoryNode::effective_time);
    let author = newest.map_or_else(|| "system".to_string(), member_author_label);
    let kind = dominant_member_kind(&members).to_string();
    let meta = bundle_meta(&members);
    let mut sub_ops: Vec<std::sync::Arc<Op>> = Vec::new();
    for member in &members {
        let op = std::sync::Arc::new(
            node_anchor_op(member)
                .cloned()
                .unwrap_or_else(empty_anchor_op),
        );
        sub_ops.push(op.clone());
        sub_ops.extend(member.sub_ops().iter().cloned());
    }
    let count = members.len();
    let noun = if kind == "command" {
        "commands"
    } else {
        "tool steps"
    };
    let summary = if members
        .iter()
        .all(|member| member.outcome() == Outcome::Success)
    {
        format!("{count} {noun} (success)")
    } else {
        format!("{count} {noun}")
    };
    HistoryNode::ExecuteBundle {
        anchor: std::sync::Arc::new(anchor),
        source_time,
        member_nodes: members,
        members: sub_ops,
        summary,
        kind,
        author,
        meta,
    }
}

/// A defensive no-op anchor for a bundle that somehow has no op member (never
/// produced by the eligibility predicate, which requires execute rows).
#[must_use]
fn empty_anchor_op() -> Op {
    Op {
        id: OpId::new(NodeId(0), 0, 0),
        parents: ParentSet::None,
        actor: editchain_core::ActorId(0),
        clock: Clock::UnixMs(0),
        scope: ScopeRef::None,
        tags: Tags::NONE,
        kind: OpKind::Unknown(UnknownOp {
            kind_discriminant: 0,
            raw_bytes: editchain_core::Payload::Empty,
        }),
    }
}

/// The underlying op of an op-backed row.
#[must_use]
fn node_anchor_op(node: &HistoryNode) -> Option<&Op> {
    match node {
        HistoryNode::EditOperation { op, .. } | HistoryNode::CollapsedImport { op, .. } => {
            Some(op.as_ref())
        }
        HistoryNode::ExecuteBundle { .. } | HistoryNode::GitCommit(_) => None,
    }
}

/// Deterministic semantic metadata for a bundle: Execute/Primary, `Success`
/// only when every member is structured-successful (a "clean run" is
/// otherwise labeled neutrally), sharing the members' turn identity.
#[must_use]
fn bundle_meta(members: &[HistoryNode]) -> NodeMeta {
    let outcome = if members
        .iter()
        .all(|member| member.outcome() == Outcome::Success)
    {
        Outcome::Success
    } else {
        Outcome::Unknown
    };
    NodeMeta {
        record_role: RecordRole::Action,
        activity_kind: ActivityKind::Execute,
        visibility: Visibility::Primary,
        outcome,
        turn_id: members.first().and_then(HistoryNode::turn_id),
    }
}

/// Dominant member kind tag ("command" when commands outnumber tools, else
/// "tool") used for the bundle's styling tag and summary noun.
#[must_use]
fn dominant_member_kind(members: &[HistoryNode]) -> &'static str {
    let mut tools = 0usize;
    let mut commands = 0usize;
    for member in members {
        match member.kind().as_str() {
            "tool" => tools = tools.saturating_add(1),
            "command" => commands = commands.saturating_add(1),
            _ => {}
        }
    }
    if commands > tools {
        "command"
    } else {
        "tool"
    }
}

/// Author label for one member row (mirrors the service's tag-derived label).
#[must_use]
fn member_author_label(node: &HistoryNode) -> String {
    match node {
        HistoryNode::CollapsedImport { author, .. } => author.clone(),
        HistoryNode::EditOperation { op, .. } => {
            if op.tags.matches_any(Tags::HUMAN) {
                "human".to_string()
            } else if op.tags.matches_any(Tags::AGENT) {
                "agent".to_string()
            } else {
                "system".to_string()
            }
        }
        HistoryNode::ExecuteBundle { .. } | HistoryNode::GitCommit(_) => "system".to_string(),
    }
}

/// Whether a member row owns a heavy per-turn state sub-op (`world_state` or
/// `turn_context`), which must keep its own row anchor.
#[must_use]
fn node_owns_state_sub_op(node: &HistoryNode) -> bool {
    node.sub_ops()
        .iter()
        .any(|op| sub_op_is_world_state_or_turn_context(op.as_ref()))
}

/// Whether a candidate run sits immediately adjacent (same turn) to a change
/// row on either side; such runs stay unfolded.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "run start/end are caller-checked against nodes.len() before any run is formed"
)]
fn run_adjacent_to_change(nodes: &[HistoryNode], start: usize, end: usize) -> bool {
    let turn = nodes[start].turn_id();
    let previous_is_change = start
        .checked_sub(1)
        .and_then(|index| nodes.get(index))
        .is_some_and(|node| node.activity_kind() == ActivityKind::Change && node.turn_id() == turn);
    let next_is_change = nodes
        .get(end.saturating_add(1))
        .is_some_and(|node| node.activity_kind() == ActivityKind::Change && node.turn_id() == turn);
    previous_is_change || next_is_change
}

/// Opaque work-unit identity: the turn identity for turn-scoped rows, else the
/// row's group key (so git/ops groups form their own units).
#[must_use]
fn unit_id(node: &HistoryNode) -> String {
    match node.turn_id() {
        Some(turn) => format!("{}/turn:{}", node.group(), turn.0),
        None => node.group(),
    }
}

/// Whether a row is a primary narrative row (prose the unit renders).
#[must_use]
fn is_narrative_row(node: &HistoryNode) -> bool {
    node.record_role() == RecordRole::Narrative && node.visibility() == Visibility::Primary
}

/// Conservative promotion predicate (see module docs).
#[must_use]
fn row_promoted(node: &HistoryNode, is_unit_final_narrative: bool) -> bool {
    matches!(
        node.outcome(),
        Outcome::Warning | Outcome::Failure | Outcome::Cancelled
    ) || matches!(
        node.activity_kind(),
        ActivityKind::Change | ActivityKind::Verify
    ) || is_unit_final_narrative
}
