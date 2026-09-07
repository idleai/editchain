//! Deterministic Activity-view semantics on top of the canonical projection.
//!
//! This module owns the seven fixed-view behaviors the unified history service
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
//! - **Plan-repeat bundling** ([`bundle_activity_plan_repeats`]): adjacent Plan
//!   narrative rows with the same normalized heading collapse into one
//!   expandable [`HistoryNode::PlanBundle`]. This is presentation grouping,
//!   not source deduplication: every reasoning record remains inspectable.
//! - **Execute-run bundling** ([`bundle_activity_execute_runs`]): maximal
//!   contiguous runs of at least two safe low-signal execute rows from one exact
//!   turn/response collapse into one synthetic [`HistoryNode::ExecuteBundle`]
//!   whose members stay expandable through the existing sub-ops model, so
//!   paging indices and virtualization work unchanged. Raw profiles never
//!   invoke bundling and stay exact/ordered.
//! - **Claude response-fragment contraction**
//!   ([`bundle_claude_response_tool_fragments`]): safe execute records in one
//!   connected provider response fold into one expandable response row when
//!   every record carries the same exact Anthropic `message.id`. This works even
//!   when unrelated sessions interleave in display order. No timestamp, text,
//!   tool-name, or proximity matching participates.
//! - **Conversational work grouping** ([`bundle_activity_work_groups`]): every
//!   maximal linear span of non-chat activity contracts into one expandable
//!   work summary. Existing execute/plan bundles remain nested members. Rows
//!   incident to any fork, merge, subagent/reconnect, or produced-commit edge
//!   are hard boundaries, so branching always remains outside a work group.
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

use crate::meta::{
    claude_assistant_message_id, is_context_compaction_import,
    sub_op_is_world_state_or_turn_context, NodeMeta,
};
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
            continuation.override_parent_keys(&[checkpoint.to_string()]);
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
/// Runs are maximal contiguous display-order spans within one group and one
/// exact execution unit: a provider-neutral turn, or (for older session-scoped
/// Claude imports) one Anthropic `message.id`. They are never gathered across
/// intervening rows or built from timestamps. A run is rejected when it sits
/// immediately adjacent (same turn) to an [`ActivityKind::Change`] row on
/// either side; verify rows are unconditional breakers. Folded members remain
/// expandable through the bundle's `sub_ops`, and surviving rows' parents are
/// rewired to the bundle's key so the layout stays connected.
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
            execution_unit: node.turn_id().map(ExecuteUnit::Turn).or_else(|| {
                node_anchor_op(node)
                    .and_then(claude_assistant_message_id)
                    .map(ExecuteUnit::ClaudeResponse)
            }),
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
        let Some(execution_unit) = facts[idx].execution_unit.as_ref() else {
            idx = idx.saturating_add(1);
            continue;
        };
        let group = facts[idx].group.as_str();
        let mut end = idx;
        while end.saturating_add(1) < nodes.len()
            && eligible[end.saturating_add(1)]
            && facts[end.saturating_add(1)].execution_unit.as_ref() == Some(execution_unit)
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
    contract_runs(nodes, runs, build_execute_bundle)
}

/// Fold connected Claude execute fragments into their exact response row.
///
/// Claude Code persists separate JSONL events for the content blocks of one
/// Anthropic response. Adjacent execute blocks are already handled by
/// [`bundle_activity_execute_runs`], but independent sessions can interleave in
/// chronological display order. Claude also records a result batch as a child
/// of the response's first content block, so leaving the remaining blocks as
/// separate rows manufactures a parallel side chain.
///
/// Membership is exact: execute rows must share display group and non-empty
/// Anthropic `message.id`, and must be connected by already-materialized causal
/// edges. A unique causal root anchors the response bundle. Every reference to
/// another member is rewritten through that root in the Activity row's explicit
/// parent override, so immutable provider facts remain untouched and mainline
/// continuations cannot become roots. A terminal execute sibling may also fold
/// into its same-response narrative parent because it has no descendants.
/// Structural endpoints, heavy state rows, disconnected ID reuse, ambiguous
/// roots, and negative outcomes stay visible. Original rows and sub-ops remain
/// expandable. Raw profiles never invoke this pass.
#[must_use]
pub fn bundle_claude_response_tool_fragments<S: std::hash::BuildHasher>(
    mut nodes: Vec<HistoryNode>,
    structural_keys: &HashSet<String, S>,
) -> Vec<HistoryNode> {
    if nodes.len() < 2 {
        return nodes;
    }

    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    let index_of: HashMap<String, usize> = keys
        .iter()
        .enumerate()
        .map(|(index, key)| (key.clone(), index))
        .collect();
    let groups: Vec<String> = nodes.iter().map(HistoryNode::group).collect();
    let response_ids: Vec<Option<String>> = nodes.iter().map(claude_response_id_of_node).collect();
    let eligible: Vec<bool> = nodes
        .iter()
        .zip(&keys)
        .zip(&response_ids)
        .map(|((node, key), response_id)| {
            response_id.is_some() && claude_execute_response_eligible(node, key, structural_keys)
        })
        .collect();

    // Build weakly connected components only across exact same-response causal
    // edges. Coincidental/disconnected reuse of an id is never gathered.
    let mut neighbors: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (child_index, node) in nodes.iter().enumerate() {
        for parent_key in stored_parent_keys(node) {
            let Some(&parent_index) = index_of.get(&parent_key) else {
                continue;
            };
            if child_index != parent_index
                && eligible.get(child_index).copied().unwrap_or(false)
                && eligible.get(parent_index).copied().unwrap_or(false)
                && groups.get(child_index) == groups.get(parent_index)
                && response_ids.get(child_index) == response_ids.get(parent_index)
            {
                if let Some(child_neighbors) = neighbors.get_mut(child_index) {
                    child_neighbors.push(parent_index);
                }
                if let Some(parent_neighbors) = neighbors.get_mut(parent_index) {
                    parent_neighbors.push(child_index);
                }
            }
        }
    }

    let components = claude_response_components(&nodes, &keys, &eligible, &neighbors);
    let mut replacement = HashMap::new();
    for component in &components {
        let Some(representative) = keys.get(component.anchor_index).cloned() else {
            continue;
        };
        for &index in &component.indices {
            if let Some(key) = keys.get(index) {
                drop(replacement.insert(key.clone(), representative.clone()));
            }
        }
    }
    for node in &mut nodes {
        rewrite_activity_parent_keys(node, &replacement);
    }

    let mut slots: Vec<Option<HistoryNode>> = nodes.into_iter().map(Some).collect();
    for component in components {
        let members: Vec<HistoryNode> = component
            .indices
            .iter()
            .filter_map(|&index| slots.get_mut(index).and_then(Option::take))
            .collect();
        if members.is_empty() {
            continue;
        }
        let Some(representative) = keys.get(component.anchor_index).cloned() else {
            continue;
        };
        let execute_members = flatten_execute_member_nodes(members.into_iter());
        let contracted = build_execute_bundle_with_anchor(execute_members, Some(&representative));
        if let Some(slot) = slots.get_mut(component.output_index) {
            *slot = Some(contracted);
        }
    }
    fold_terminal_claude_execute_into_narrative(
        slots.into_iter().flatten().collect(),
        structural_keys,
    )
}

/// One exact, connected response component selected for contraction.
struct ClaudeResponseComponent {
    indices: Vec<usize>,
    output_index: usize,
    anchor_index: usize,
}

/// Whether an execute row is safe to fold into an exact Claude response.
#[must_use]
fn claude_execute_response_eligible<S: std::hash::BuildHasher>(
    node: &HistoryNode,
    key: &str,
    structural_keys: &HashSet<String, S>,
) -> bool {
    if structural_keys.contains(key)
        || node.visibility() != Visibility::Primary
        || matches!(
            node.outcome(),
            Outcome::Warning | Outcome::Failure | Outcome::Cancelled
        )
        || node_owns_state_sub_op(node)
    {
        return false;
    }
    matches!(
        node,
        HistoryNode::CollapsedImport { .. } | HistoryNode::ExecuteBundle { .. }
    ) && node.activity_kind() == ActivityKind::Execute
        && node.record_role() == RecordRole::Action
}

/// Find contractible weak components in the exact-response subgraph.
#[must_use]
fn claude_response_components(
    nodes: &[HistoryNode],
    keys: &[String],
    eligible: &[bool],
    neighbors: &[Vec<usize>],
) -> Vec<ClaudeResponseComponent> {
    let mut visited = vec![false; nodes.len()];
    let mut components = Vec::new();
    for (start, is_eligible) in eligible.iter().copied().enumerate().take(nodes.len()) {
        if visited.get(start).copied().unwrap_or(true) || !is_eligible {
            continue;
        }
        let mut stack = vec![start];
        let mut indices = Vec::new();
        if let Some(seen) = visited.get_mut(start) {
            *seen = true;
        }
        while let Some(index) = stack.pop() {
            indices.push(index);
            for &neighbor in neighbors.get(index).into_iter().flatten() {
                if let Some(seen) = visited.get_mut(neighbor).filter(|seen| !**seen) {
                    *seen = true;
                    stack.push(neighbor);
                }
            }
        }
        indices.sort_unstable();
        if let Some(component) = contractible_claude_component(nodes, keys, indices) {
            components.push(component);
        }
    }
    components
}

/// Validate one connected response component and choose its stable anchor.
#[must_use]
fn contractible_claude_component(
    nodes: &[HistoryNode],
    keys: &[String],
    indices: Vec<usize>,
) -> Option<ClaudeResponseComponent> {
    if indices.len() < 2 {
        return None;
    }
    if indices
        .iter()
        .any(|&index| nodes.get(index).is_none() || keys.get(index).is_none())
    {
        return None;
    }
    let member_keys: HashSet<&str> = indices
        .iter()
        .filter_map(|&index| keys.get(index).map(String::as_str))
        .collect();
    let roots: Vec<usize> = indices
        .iter()
        .copied()
        .filter(|&index| {
            nodes.get(index).is_some_and(|node| {
                !stored_parent_keys(node)
                    .iter()
                    .any(|parent| member_keys.contains(parent.as_str()))
            })
        })
        .collect();
    let [anchor_index] = roots.as_slice() else {
        return None;
    };
    Some(ClaudeResponseComponent {
        output_index: *indices.first()?,
        indices,
        anchor_index: *anchor_index,
    })
}

/// Rewrite one Activity node through exact response representatives.
fn rewrite_activity_parent_keys(node: &mut HistoryNode, replacement: &HashMap<String, String>) {
    let old = stored_parent_keys(node);
    let mut rewritten: Vec<String> = old
        .iter()
        .map(|key| replacement.get(key).cloned().unwrap_or_else(|| key.clone()))
        .collect();
    let mut seen = HashSet::with_capacity(rewritten.len());
    rewritten.retain(|key| seen.insert(key.clone()));
    if rewritten != old {
        node.override_parent_keys(&rewritten);
    }
}

/// Fold terminal execute siblings into an exact same-response narrative row.
#[must_use]
fn fold_terminal_claude_execute_into_narrative<S: std::hash::BuildHasher>(
    nodes: Vec<HistoryNode>,
    structural_keys: &HashSet<String, S>,
) -> Vec<HistoryNode> {
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    let index_of: HashMap<String, usize> = keys
        .iter()
        .enumerate()
        .map(|(index, key)| (key.clone(), index))
        .collect();
    let response_ids: Vec<Option<String>> = nodes.iter().map(claude_response_id_of_node).collect();
    let mut child_counts = vec![0usize; nodes.len()];
    for node in &nodes {
        for parent in stored_parent_keys(node) {
            if let Some(&parent_index) = index_of.get(&parent) {
                if let Some(count) = child_counts.get_mut(parent_index) {
                    *count = count.saturating_add(1);
                }
            }
        }
    }
    let mut parent_of = vec![None; nodes.len()];
    for (child_index, child) in nodes.iter().enumerate() {
        let Some(&child_count) = child_counts.get(child_index) else {
            continue;
        };
        let Some(child_key) = keys.get(child_index) else {
            continue;
        };
        if child_count != 0 || !claude_execute_response_eligible(child, child_key, structural_keys)
        {
            continue;
        }
        let parent_keys = stored_parent_keys(child);
        let [parent_key] = parent_keys.as_slice() else {
            continue;
        };
        let Some(&parent_index) = index_of.get(parent_key) else {
            continue;
        };
        let Some(parent) = nodes.get(parent_index) else {
            continue;
        };
        let parent_child_count = child_counts.get(parent_index).copied().unwrap_or(0);
        let child_response = response_ids.get(child_index).and_then(Option::as_ref);
        let parent_response = response_ids.get(parent_index).and_then(Option::as_ref);
        if parent_index <= child_index
            || parent_child_count < 2
            || structural_keys.contains(parent_key)
            || !matches!(parent, HistoryNode::CollapsedImport { .. })
            || parent.record_role() != RecordRole::Narrative
            || parent.visibility() != Visibility::Primary
            || parent.group() != child.group()
            || child_response.is_none()
            || child_response != parent_response
        {
            continue;
        }
        if let Some(owner) = parent_of.get_mut(child_index) {
            *owner = Some(parent_index);
        }
    }
    let mut children_by_parent: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (child_index, parent_index) in parent_of.into_iter().enumerate() {
        if let Some(parent_index) = parent_index {
            if let Some(children) = children_by_parent.get_mut(parent_index) {
                children.push(child_index);
            }
        }
    }
    let mut slots: Vec<Option<HistoryNode>> = nodes.into_iter().map(Some).collect();
    for (parent_index, child_indices) in children_by_parent.into_iter().enumerate() {
        if child_indices.is_empty() {
            continue;
        }
        let children: Vec<HistoryNode> = child_indices
            .into_iter()
            .filter_map(|index| slots.get_mut(index).and_then(Option::take))
            .collect();
        let Some(Some(parent)) = slots.get_mut(parent_index) else {
            continue;
        };
        if let HistoryNode::CollapsedImport { sub_ops, .. } = parent {
            for child in children {
                match child {
                    HistoryNode::CollapsedImport {
                        op,
                        sub_ops: child_sub_ops,
                        ..
                    } => {
                        sub_ops.push(op);
                        sub_ops.extend(child_sub_ops);
                    }
                    HistoryNode::ExecuteBundle { members, .. } => sub_ops.extend(members),
                    HistoryNode::EditOperation { .. }
                    | HistoryNode::PlanBundle { .. }
                    | HistoryNode::WorkGroup { .. }
                    | HistoryNode::GitCommit(_) => {}
                }
            }
        }
    }
    slots.into_iter().flatten().collect()
}

/// Flatten already-bundled and ordinary execute rows into original members in
/// display order.
#[must_use]
fn flatten_execute_member_nodes(members: impl Iterator<Item = HistoryNode>) -> Vec<HistoryNode> {
    let mut flattened = Vec::new();
    for member in members {
        match member {
            HistoryNode::ExecuteBundle { member_nodes, .. } => flattened.extend(member_nodes),
            HistoryNode::CollapsedImport { .. } => flattened.push(member),
            HistoryNode::EditOperation { .. }
            | HistoryNode::PlanBundle { .. }
            | HistoryNode::WorkGroup { .. }
            | HistoryNode::GitCommit(_) => {}
        }
    }
    flattened
}

/// Collapse maximal contiguous runs of at least two Plan narrative rows that
/// repeat the same normalized heading into [`HistoryNode::PlanBundle`] nodes.
///
/// Grouping is deliberately conservative and display-local: members must be
/// adjacent, belong to the same group and optional turn, be primary narrative
/// Plan rows, carry no negative outcome, and participate in no structural
/// fork/subagent/reconnect edge. Heading comparison only removes paired
/// Markdown emphasis and normalizes whitespace; it remains case-sensitive and
/// never gathers across an intervening row. Every original row and bundled
/// metadata sub-op stays expandable through [`HistoryNode::sub_ops`].
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "run scan indices are bounds-checked by the while conditions against nodes.len()"
)]
pub fn bundle_activity_plan_repeats<S: std::hash::BuildHasher>(
    nodes: Vec<HistoryNode>,
    structural_keys: &HashSet<String, S>,
) -> Vec<HistoryNode> {
    let facts: Vec<PlanMemberFacts> = nodes
        .iter()
        .map(|node| PlanMemberFacts {
            key: node.node_key(),
            group: node.group(),
            turn: node.turn_id(),
            heading: normalized_plan_heading(&node.summary()),
        })
        .collect();
    let eligible: Vec<bool> = nodes
        .iter()
        .zip(&facts)
        .map(|(node, fact)| {
            node.activity_kind() == ActivityKind::Plan
                && node.record_role() == RecordRole::Narrative
                && node.visibility() == Visibility::Primary
                && !matches!(
                    node.outcome(),
                    Outcome::Warning | Outcome::Failure | Outcome::Cancelled
                )
                && fact.heading.is_some()
                && !structural_keys.contains(&fact.key)
                && node_anchor_op(node).is_some()
        })
        .collect();
    let mut runs = Vec::new();
    let mut idx = 0usize;
    while idx < nodes.len() {
        if !eligible[idx] {
            idx = idx.saturating_add(1);
            continue;
        }
        let group = facts[idx].group.as_str();
        let turn = facts[idx].turn;
        let heading = facts[idx].heading.as_deref();
        let mut end = idx;
        while end.saturating_add(1) < nodes.len()
            && eligible[end.saturating_add(1)]
            && facts[end.saturating_add(1)].group == group
            && facts[end.saturating_add(1)].turn == turn
            && facts[end.saturating_add(1)].heading.as_deref() == heading
        {
            end = end.saturating_add(1);
        }
        if end.saturating_sub(idx).saturating_add(1) >= 2 {
            runs.push((idx, end));
        }
        idx = end.saturating_add(1);
    }
    contract_runs(nodes, runs, build_plan_bundle)
}

/// Group maximal connected linear spans of work between user/agent chats.
///
/// A work candidate is any non-Git row except a primary user/agent
/// Conversation row. System/lifecycle records, plans, exploration, execution,
/// changes, verification, and diagnostics therefore collapse together until a
/// conversational boundary. Existing execute-run and plan-repeat bundles are
/// retained as direct member nodes, which gives the renderer one outer work
/// disclosure and one existing inner disclosure level.
///
/// Branching is forbidden inside a group. Explicit structural endpoints from
/// `structural_keys` are excluded, and this pass independently protects every
/// row incident to ordinary causal fan-out or fan-in. Remaining members must
/// be adjacent on one exact parent path, in one display group. Even a
/// single-row interval becomes a work group so the top-level Activity view is
/// consistently chat / work / chat rather than leaking isolated bookkeeping.
#[must_use]
pub fn bundle_activity_work_groups<S: std::hash::BuildHasher>(
    nodes: Vec<HistoryNode>,
    structural_keys: &HashSet<String, S>,
) -> Vec<HistoryNode> {
    if nodes.is_empty() {
        return nodes;
    }
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    let present: HashSet<&str> = keys.iter().map(String::as_str).collect();
    let parents: Vec<Vec<String>> = nodes
        .iter()
        .map(|node| {
            stored_parent_keys(node)
                .into_iter()
                .filter(|parent| present.contains(parent.as_str()))
                .collect()
        })
        .collect();
    let branch_boundaries = causal_branch_boundaries(&keys, &parents, structural_keys);
    let eligible: Vec<bool> = nodes
        .iter()
        .zip(&keys)
        .map(|(node, key)| {
            node.git_oid().is_none()
                && !is_user_or_agent_chat(node)
                && !branch_boundaries.contains(key)
                && node_anchor_op(node).is_some()
        })
        .collect();

    let groups: Vec<String> = nodes.iter().map(HistoryNode::group).collect();
    let mut runs = Vec::new();
    let mut index = 0usize;
    while index < nodes.len() {
        if !eligible.get(index).copied().unwrap_or(false) {
            index = index.saturating_add(1);
            continue;
        }
        let start = index;
        let mut end = index;
        while let Some(next) = end.checked_add(1).filter(|next| *next < nodes.len()) {
            let connected = parents.get(end).is_some_and(|node_parents| {
                node_parents.len() == 1
                    && keys.get(next).is_some_and(|next_key| {
                        node_parents
                            .first()
                            .is_some_and(|parent| parent == next_key)
                    })
            });
            if !eligible.get(next).copied().unwrap_or(false)
                || groups.get(next) != groups.get(start)
                || !connected
            {
                break;
            }
            end = next;
        }
        runs.push((start, end));
        index = end.saturating_add(1);
    }
    contract_runs(nodes, runs, build_work_group)
}

/// Protect every row incident to explicit structure or ordinary causal
/// fan-in/fan-out. The returned set is the complete set of legal group breaks.
#[must_use]
fn causal_branch_boundaries<S: std::hash::BuildHasher>(
    keys: &[String],
    parents: &[Vec<String>],
    structural_keys: &HashSet<String, S>,
) -> HashSet<String> {
    let mut boundaries: HashSet<String> = structural_keys.iter().cloned().collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (child, node_parents) in keys.iter().zip(parents) {
        if node_parents.len() > 1 {
            let _: bool = boundaries.insert(child.clone());
            boundaries.extend(node_parents.iter().cloned());
        }
        for parent in node_parents {
            children
                .entry(parent.as_str())
                .or_default()
                .push(child.as_str());
        }
    }
    for (parent, child_keys) in children {
        if child_keys.len() > 1 {
            let _: bool = boundaries.insert(parent.to_owned());
            boundaries.extend(child_keys.into_iter().map(str::to_owned));
        }
    }
    boundaries
}

/// Primary narrative prose is a user/agent chat boundary. Provider transport
/// copies and conversation-like lifecycle records are supporting/trace rows,
/// so they may still live inside a group without relying on provider-specific
/// author labels being present.
#[must_use]
fn is_user_or_agent_chat(node: &HistoryNode) -> bool {
    node.activity_kind() == ActivityKind::Conversation
        && node.record_role() == RecordRole::Narrative
        && node.visibility() == Visibility::Primary
}

/// Per-node facts precomputed once for the run scan, so membership checks do
/// no repeated string formatting or sub-op JSON parsing.
struct MemberFacts {
    /// Node key (op id or commit OID hex).
    key: String,
    /// Display group (session/repo/ops), precomputed once per node.
    group: String,
    /// Exact execution identity (runs never cross turns/responses).
    execution_unit: Option<ExecuteUnit>,
    /// Whether the node owns a `world_state`/`turn_context` sub-op.
    owns_state_subop: bool,
}

/// Exact scope in which adjacent execute rows may form one Activity bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecuteUnit {
    /// Provider-neutral imported turn identity.
    Turn(TurnId),
    /// Anthropic response identity retained by Claude assistant content blocks.
    ClaudeResponse(String),
}

/// One exact Claude response id for an ordinary row or every member of an
/// execute bundle. Mixed/non-Claude bundles have no response identity.
#[must_use]
fn claude_response_id_of_node(node: &HistoryNode) -> Option<String> {
    match node {
        HistoryNode::CollapsedImport { op, .. } => claude_assistant_message_id(op),
        HistoryNode::ExecuteBundle { member_nodes, .. } => {
            let mut response_id: Option<String> = None;
            for member in member_nodes {
                let current = node_anchor_op(member).and_then(claude_assistant_message_id)?;
                if response_id.as_ref().is_some_and(|known| known != &current) {
                    return None;
                }
                response_id = Some(current);
            }
            response_id
        }
        HistoryNode::EditOperation { .. }
        | HistoryNode::PlanBundle { .. }
        | HistoryNode::WorkGroup { .. }
        | HistoryNode::GitCommit(_) => None,
    }
}

/// Precomputed identity and heading facts for one Plan-repeat candidate.
struct PlanMemberFacts {
    key: String,
    group: String,
    turn: Option<TurnId>,
    heading: Option<String>,
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
fn contract_runs(
    nodes: Vec<HistoryNode>,
    runs: Vec<(usize, usize)>,
    build: fn(Vec<HistoryNode>) -> HistoryNode,
) -> Vec<HistoryNode> {
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
            result.push(build(members));
        } else if let Some(node) = slots[idx].take() {
            result.push(node);
        }
    }
    if !bundle_of_member.is_empty() {
        for node in &mut result {
            // Bundles read their parents through an intra-run filter and git
            // rows never reference op members; only stored op parents of kept
            // rows need the rewrite.
            if matches!(node, HistoryNode::GitCommit(_)) {
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
            node.override_parent_keys(&keys);
        }
    }
    result
}

/// The stored causal parent keys of an op-backed row (empty otherwise).
#[must_use]
fn stored_parent_keys(node: &HistoryNode) -> Vec<String> {
    match node {
        HistoryNode::EditOperation {
            parent_override: Some(keys),
            ..
        }
        | HistoryNode::CollapsedImport {
            parent_override: Some(keys),
            ..
        }
        | HistoryNode::ExecuteBundle {
            parent_override: Some(keys),
            ..
        }
        | HistoryNode::PlanBundle {
            parent_override: Some(keys),
            ..
        }
        | HistoryNode::WorkGroup {
            parent_override: Some(keys),
            ..
        } => keys.clone(),
        HistoryNode::EditOperation {
            op,
            parent_override: None,
            ..
        }
        | HistoryNode::CollapsedImport {
            op,
            parent_override: None,
            ..
        } => op.parents.iter().map(ToString::to_string).collect(),
        HistoryNode::ExecuteBundle {
            member_nodes,
            parent_override: None,
            ..
        }
        | HistoryNode::PlanBundle {
            member_nodes,
            parent_override: None,
            ..
        }
        | HistoryNode::WorkGroup {
            member_nodes,
            parent_override: None,
            ..
        } => {
            let member_keys: HashSet<String> =
                member_nodes.iter().map(HistoryNode::node_key).collect();
            let mut seen = HashSet::new();
            let mut parents = Vec::new();
            for member in member_nodes {
                for parent in stored_parent_keys(member) {
                    if !member_keys.contains(&parent) && seen.insert(parent.clone()) {
                        parents.push(parent);
                    }
                }
            }
            parents
        }
        HistoryNode::GitCommit(_) => Vec::new(),
    }
}

/// Build one synthetic summary node from a run's member rows (newest-first).
#[must_use]
fn build_execute_bundle(members: Vec<HistoryNode>) -> HistoryNode {
    build_execute_bundle_with_anchor(members, None)
}

/// Build an execute bundle, optionally retaining a specific member as its
/// graph identity while keeping the bundle in its newest member's display slot.
#[must_use]
fn build_execute_bundle_with_anchor(
    members: Vec<HistoryNode>,
    anchor_key: Option<&str>,
) -> HistoryNode {
    let newest = members.first();
    let anchor_member = anchor_key
        .and_then(|key| members.iter().find(|member| member.node_key() == key))
        .or(newest);
    let anchor = anchor_member
        .and_then(node_anchor_op)
        .cloned()
        .unwrap_or_else(empty_anchor_op);
    let source_time = newest.map_or(EffectiveTime::Unknown, HistoryNode::effective_time);
    let author = newest.map_or_else(|| "system".to_string(), member_author_label);
    let kind = dominant_member_kind(&members).to_string();
    let meta = bundle_meta(&members);
    let sub_ops = flattened_bundle_members(&members);
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
        parent_override: None,
        member_nodes: members,
        members: sub_ops,
        summary,
        kind,
        author,
        meta,
    }
}

/// Build one Plan-repeat summary node from adjacent members (newest-first).
#[must_use]
fn build_plan_bundle(members: Vec<HistoryNode>) -> HistoryNode {
    let newest = members.first();
    let anchor = newest
        .and_then(node_anchor_op)
        .cloned()
        .unwrap_or_else(empty_anchor_op);
    let source_time = newest.map_or(EffectiveTime::Unknown, HistoryNode::effective_time);
    let author = newest.map_or_else(|| "system".to_string(), member_author_label);
    let summary = newest.map_or_else(String::new, HistoryNode::summary);
    let kind = newest.map_or_else(|| "reflection".to_string(), HistoryNode::kind);
    let meta = NodeMeta {
        record_role: RecordRole::Narrative,
        activity_kind: ActivityKind::Plan,
        visibility: Visibility::Primary,
        outcome: Outcome::Unknown,
        turn_id: newest.and_then(HistoryNode::turn_id),
    };
    let sub_ops = flattened_bundle_members(&members);
    HistoryNode::PlanBundle {
        anchor: std::sync::Arc::new(anchor),
        source_time,
        parent_override: None,
        member_nodes: members,
        members: sub_ops,
        summary,
        kind,
        author,
        meta,
    }
}

/// Build one outer work summary while retaining existing inner bundle nodes.
#[must_use]
fn build_work_group(members: Vec<HistoryNode>) -> HistoryNode {
    let newest = members.first();
    let anchor = newest
        .and_then(node_anchor_op)
        .cloned()
        .unwrap_or_else(empty_anchor_op);
    let source_time = newest.map_or(EffectiveTime::Unknown, HistoryNode::effective_time);
    let summary = work_group_summary(&members);
    let represented = flattened_work_group_members(&members);
    let meta = NodeMeta {
        record_role: RecordRole::Action,
        activity_kind: ActivityKind::Work,
        visibility: Visibility::Primary,
        outcome: aggregate_work_outcome(&members),
        turn_id: common_turn_id(&members),
    };
    HistoryNode::WorkGroup {
        anchor: std::sync::Arc::new(anchor),
        source_time,
        parent_override: None,
        member_nodes: members,
        members: represented,
        summary,
        meta,
    }
}

/// All source operations represented by a work group's direct member nodes.
#[must_use]
fn flattened_work_group_members(members: &[HistoryNode]) -> Vec<std::sync::Arc<Op>> {
    let mut represented = Vec::new();
    let mut seen = HashSet::new();
    for member in members {
        let candidates: Vec<std::sync::Arc<Op>> = match member {
            HistoryNode::EditOperation { op, .. } => vec![op.clone()],
            HistoryNode::CollapsedImport { op, sub_ops, .. } => std::iter::once(op.clone())
                .chain(sub_ops.iter().cloned())
                .collect(),
            HistoryNode::ExecuteBundle { members, .. }
            | HistoryNode::PlanBundle { members, .. }
            | HistoryNode::WorkGroup { members, .. } => members.clone(),
            HistoryNode::GitCommit(_) => Vec::new(),
        };
        for op in candidates {
            if seen.insert(op.id) {
                represented.push(op);
            }
        }
    }
    represented
}

/// Deterministic, bounded summary from the whole work interval.
#[must_use]
fn work_group_summary(members: &[HistoryNode]) -> String {
    let mut counts: HashMap<ActivityKind, usize> = HashMap::new();
    for member in members {
        collect_activity_counts(member, &mut counts);
    }
    let total = counts.values().copied().sum::<usize>();
    let breakdown = work_breakdown(&counts);
    let overview = if breakdown.is_empty() {
        format!("{total} {}", activity_noun(total))
    } else {
        format!("{total} {} · {breakdown}", activity_noun(total))
    };
    let headline = work_headline(members);
    if headline.is_empty() || headline == overview {
        overview
    } else {
        format!("{} — {overview}", bounded_summary(&headline, 96))
    }
}

/// Count original activity rows recursively through existing inner bundles.
fn collect_activity_counts(node: &HistoryNode, counts: &mut HashMap<ActivityKind, usize>) {
    match node {
        HistoryNode::ExecuteBundle { member_nodes, .. }
        | HistoryNode::PlanBundle { member_nodes, .. }
        | HistoryNode::WorkGroup { member_nodes, .. } => {
            for member in member_nodes {
                collect_activity_counts(member, counts);
            }
        }
        HistoryNode::EditOperation { .. }
        | HistoryNode::CollapsedImport { .. }
        | HistoryNode::GitCommit(_) => {
            let count = counts.entry(node.activity_kind()).or_default();
            *count = count.saturating_add(1);
        }
    }
}

/// Pick one meaningful member summary, preferring durable/high-signal work.
#[must_use]
fn work_headline(members: &[HistoryNode]) -> String {
    const PRIORITY: [ActivityKind; 11] = [
        ActivityKind::Change,
        ActivityKind::Verify,
        ActivityKind::Diagnose,
        ActivityKind::Plan,
        ActivityKind::Explore,
        ActivityKind::Execute,
        ActivityKind::Coordinate,
        ActivityKind::External,
        ActivityKind::System,
        ActivityKind::Conversation,
        ActivityKind::Unknown,
    ];
    for activity in PRIORITY {
        if let Some(summary) = members
            .iter()
            .find(|member| member.activity_kind() == activity)
            .map(HistoryNode::summary)
            .filter(|summary| !summary.trim().is_empty())
        {
            return summary.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    String::new()
}

/// Stable semantic breakdown ordered by how useful it is in a collapsed row.
#[must_use]
fn work_breakdown(counts: &HashMap<ActivityKind, usize>) -> String {
    const ORDER: [(ActivityKind, &str, &str); 11] = [
        (ActivityKind::Change, "change", "changes"),
        (ActivityKind::Verify, "verification", "verifications"),
        (ActivityKind::Diagnose, "diagnosis", "diagnoses"),
        (ActivityKind::Plan, "plan", "plans"),
        (ActivityKind::Explore, "exploration", "explorations"),
        (ActivityKind::Execute, "run", "runs"),
        (ActivityKind::Coordinate, "coordination", "coordinations"),
        (ActivityKind::External, "external event", "external events"),
        (ActivityKind::System, "system event", "system events"),
        (
            ActivityKind::Conversation,
            "supporting chat",
            "supporting chats",
        ),
        (ActivityKind::Unknown, "other", "other"),
    ];
    ORDER
        .iter()
        .filter_map(|(kind, singular, plural)| {
            let count = counts.get(kind).copied().unwrap_or(0);
            (count > 0).then(|| format!("{count} {}", if count == 1 { *singular } else { *plural }))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

const fn activity_noun(count: usize) -> &'static str {
    if count == 1 {
        "activity"
    } else {
        "activities"
    }
}

/// Character-safe summary bound; UI rows remain one line and retain the
/// aggregate breakdown even when the chosen member headline is very long.
#[must_use]
fn bounded_summary(summary: &str, max_chars: usize) -> String {
    let mut chars = summary.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", prefix.trim_end())
    } else {
        prefix
    }
}

/// Preserve a turn id only when every member carries that same exact id.
#[must_use]
fn common_turn_id(members: &[HistoryNode]) -> Option<TurnId> {
    let first = members.first()?.turn_id()?;
    members
        .iter()
        .all(|member| member.turn_id() == Some(first))
        .then_some(first)
}

/// Fold structured outcomes without inventing success from missing evidence.
#[must_use]
fn aggregate_work_outcome(members: &[HistoryNode]) -> Outcome {
    if members
        .iter()
        .any(|member| member.outcome() == Outcome::Failure)
    {
        Outcome::Failure
    } else if members
        .iter()
        .any(|member| member.outcome() == Outcome::Cancelled)
    {
        Outcome::Cancelled
    } else if members
        .iter()
        .any(|member| member.outcome() == Outcome::Warning)
    {
        Outcome::Warning
    } else if !members.is_empty()
        && members
            .iter()
            .all(|member| member.outcome() == Outcome::Success)
    {
        Outcome::Success
    } else {
        Outcome::Unknown
    }
}

/// Flatten each original bundle member's anchor op and attached metadata ops.
#[must_use]
fn flattened_bundle_members(members: &[HistoryNode]) -> Vec<std::sync::Arc<Op>> {
    let mut sub_ops = Vec::new();
    for member in members {
        let op = std::sync::Arc::new(
            node_anchor_op(member)
                .cloned()
                .unwrap_or_else(empty_anchor_op),
        );
        sub_ops.push(op);
        sub_ops.extend(member.sub_ops().iter().cloned());
    }
    sub_ops
}

/// Normalize only presentation-equivalent Plan headings. Semantic differences
/// (including case and punctuation) remain distinct.
#[must_use]
fn normalized_plan_heading(summary: &str) -> Option<String> {
    let mut heading = summary.trim();
    loop {
        let stripped = if heading.len() >= 4
            && ((heading.starts_with("**") && heading.ends_with("**"))
                || (heading.starts_with("__") && heading.ends_with("__")))
        {
            heading.get(2..heading.len().saturating_sub(2))
        } else if heading.len() >= 2
            && ((heading.starts_with('*') && heading.ends_with('*'))
                || (heading.starts_with('_') && heading.ends_with('_')))
        {
            heading.get(1..heading.len().saturating_sub(1))
        } else {
            None
        };
        let Some(stripped) = stripped else {
            break;
        };
        heading = stripped.trim();
    }
    let normalized = heading.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
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
        HistoryNode::ExecuteBundle { anchor, .. }
        | HistoryNode::PlanBundle { anchor, .. }
        | HistoryNode::WorkGroup { anchor, .. } => Some(anchor.as_ref()),
        HistoryNode::GitCommit(_) => None,
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
        HistoryNode::ExecuteBundle { .. }
        | HistoryNode::PlanBundle { .. }
        | HistoryNode::WorkGroup { .. }
        | HistoryNode::GitCommit(_) => "system".to_string(),
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
