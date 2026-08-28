//! Regression tests for the semantic-collapse invariant: relationship anchors
//! and targets (`ForkOf` / `SubagentOf` / `ReconnectsTo`) must resolve to canonical
//! VISIBLE rows even when their source ops are folded into a collapsed bundle,
//! and unresolved endpoints must never generate phantom intervals or inflate
//! the lane count.

// Crate-level dependency markers (used by Cargo for feature resolution).
use regex as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, ImportOp, MessageOp, NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind,
    ParentSet, Payload, ScopeRef, SessionId, Tags, ToolOp, ToolStage,
};
use editchain_project::HistoryProjection;

/// A raw import op (the linear backbone row) scoped to a session.
fn import_op(node: u64, seq: u64, session: u64, clock_ms: u64) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::Session(SessionId(session)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(format!("raw {node}:{seq}").into_bytes()),
            raw_hash: None,
        }),
    }
}

/// A normalized message child of a raw import op (folded into the import row).
fn child_message_op(node: u64, seq: u64, parent: OpId, text: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(0)),
        tags: Tags::AGENT | Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.as_bytes().to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// A normalized Tool child of a raw import op (folded into the import row).
fn child_tool_op(node: u64, seq: u64, parent: OpId, name: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(0)),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(name.as_bytes().to_vec()),
            stage: ToolStage::Start,
            content: Payload::Empty,
        }),
    }
}

/// A structural relationship note.
fn relation_note(
    node: u64,
    seq: u64,
    parent: OpId,
    target: OpId,
    relationship: NoteRelationship,
) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(0)),
        tags: Tags::NOTE,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![target],
            relationship,
            content: Payload::Empty,
        }),
    }
}

/// A standalone message op (its own row; used for fork-prologue scenarios).
fn msg_op(node: u64, seq: u64, session: u64, clock_ms: u64, parent: Option<OpId>) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::Session(SessionId(session)),
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(format!("msg {node}:{seq}").into_bytes()),
            content_type: Payload::Inline(b"text/plain".to_vec()),
        }),
    }
}

/// A standalone Tool op (its own row; used for include-kind filter scenarios
/// where the structural anchor/target rows are tool-kind rows that "messages
/// only" would otherwise exclude).
fn tool_op_row(node: u64, seq: u64, session: u64, clock_ms: u64, parent: Option<OpId>) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::Session(SessionId(session)),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Empty,
        }),
    }
}

/// The Codex failure shape: a `SubagentOf` note whose ANCHOR (the subagent's
/// first op) and TARGET (the parent thread's normalized structural start
/// marker) are both folded into their raw import rows. The virtual edge must
/// resolve to the two visible import rows.
#[test]
#[expect(
    clippy::panic,
    reason = "The explicit panic reports an impossible fixture variant at the assertion site"
)]
fn subagent_of_folded_anchor_and_target_resolve_to_visible_rows() {
    let parent_import = import_op(1, 1, 10, 1_000);
    // The parent thread's "started" marker: a normalized child of the import,
    // folded into `parent_import`'s CollapsedImport row.
    let spawn_marker = child_message_op(2, 1, parent_import.id, "spawned subagent");
    let sub_import = import_op(3, 1, 20, 2_000);
    // The subagent thread's first op is also a folded normalized child.
    let sub_first = child_message_op(4, 1, sub_import.id, "sub work");
    let note = relation_note(
        5,
        1,
        sub_first.id,
        spawn_marker.id,
        NoteRelationship::SubagentOf,
    );

    let projection = HistoryProjection::from_ops(vec![
        parent_import.clone(),
        spawn_marker.clone(),
        sub_import.clone(),
        sub_first.clone(),
        note.clone(),
    ]);

    // Only the two raw import rows render; every folded child and the
    // structural note fold out.
    let nodes = projection.nodes();
    let keys: Vec<String> = nodes
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    assert_eq!(keys.len(), 2, "got {keys:?}");
    assert!(!keys.contains(&spawn_marker.id.to_string()));
    assert!(!keys.contains(&sub_first.id.to_string()));
    assert!(!keys.contains(&note.id.to_string()));

    // The canonical notes index is keyed by the visible anchor row and its
    // target rewrites to the visible target row.
    let rel_notes = projection.relationship_notes();
    let note_list = rel_notes
        .get(&sub_import.id)
        .expect("note keyed under canonical anchor");
    assert_eq!(note_list.len(), 1);
    let note = note_list.first().expect("one canonical relationship note");
    let OpKind::Note(n) = &note.kind else {
        panic!("note");
    };
    // The notes map keeps the STORED target (the folded marker op); the lift to
    // the visible spawn anchor row happens at edge-construction time.
    assert_eq!(n.target_ids, vec![spawn_marker.id], "target kept as stored");

    // The virtual edge draws from the subagent row to the parent row.
    let layout = projection.graph_layout();
    assert!(
        layout.edges.iter().any(|e| {
            e.child == sub_import.id.to_string() && e.parent == parent_import.id.to_string()
        }),
        "SubagentOf edge must resolve to visible rows; got {:#?}",
        layout
            .edges
            .iter()
            .map(|e| (e.child.as_str(), e.parent.as_str()))
            .collect::<Vec<_>>()
    );
    // One connected component: both rows share lane 0 — no phantom lane.
    let max_lane = layout.rows.iter().map(|r| r.lane).max().unwrap_or(0);
    assert_eq!(max_lane, 0, "no relationship endpoint may inflate lanes");
}

/// The Codex failure shape for reconnects: a `ReconnectsTo` note whose anchor
/// is a folded Tool op (the collab completion marker) and whose target is a
/// folded Tool/Note op (the subagent's last op). Both must resolve to their
/// raw import rows so the reconnect edge renders.
#[test]
fn reconnects_to_folded_tool_endpoints_resolve_to_visible_rows() {
    let sub_import = import_op(1, 1, 10, 1_000);
    let sub_last = child_tool_op(2, 1, sub_import.id, "Bash");
    let collab_import = import_op(3, 1, 20, 3_000);
    // The collab tool op (agentsStates completion marker) folds into its import.
    let collab_tool = child_tool_op(4, 1, collab_import.id, "Task");
    let note = relation_note(
        5,
        1,
        collab_tool.id,
        sub_last.id,
        NoteRelationship::ReconnectsTo,
    );

    let projection = HistoryProjection::from_ops(vec![
        sub_import.clone(),
        sub_last.clone(),
        collab_import.clone(),
        collab_tool.clone(),
        note.clone(),
    ]);

    let layout = projection.graph_layout();
    // The collab row (newer) reconnects down to the subagent row (older).
    assert!(
        layout.edges.iter().any(|e| {
            e.child == collab_import.id.to_string() && e.parent == sub_import.id.to_string()
        }),
        "ReconnectsTo edge must resolve to visible rows; got {:#?}",
        layout
            .edges
            .iter()
            .map(|e| (e.child.as_str(), e.parent.as_str()))
            .collect::<Vec<_>>()
    );
    let max_lane = layout.rows.iter().map(|r| r.lane).max().unwrap_or(0);
    assert_eq!(max_lane, 0, "no relationship endpoint may inflate lanes");
}

/// An unresolved relationship target (an op id absent from the projection with
/// no representative) must be dropped — it may not create a phantom interval
/// that stops a later disjoint chain from reusing the base lane.
#[test]
fn unresolved_relationship_target_does_not_inflate_lanes() {
    // Chain A (newest rows 0-1).
    let a1 = import_op(1, 1, 10, 3_000);
    let mut a2 = import_op(1, 2, 10, 4_000);
    a2.parents = ParentSet::One(a1.id);
    // A note anchored on the visible row A2 whose target does not exist here.
    let external = OpId::new(NodeId(99), 0, 1);
    let note = relation_note(2, 9, a2.id, external, NoteRelationship::ReconnectsTo);

    // Chain B (older rows 2-3): disjoint from A, so it must share lane 0.
    let b1 = import_op(3, 1, 30, 1_000);
    let mut b2 = import_op(3, 2, 30, 2_000);
    b2.parents = ParentSet::One(b1.id);

    let projection = HistoryProjection::from_ops(vec![
        a1.clone(),
        a2.clone(),
        note.clone(),
        b1.clone(),
        b2.clone(),
    ]);
    let layout = projection.graph_layout();

    // The unresolvable target is dropped: no edge may reference it.
    assert!(
        layout
            .edges
            .iter()
            .all(|e| { e.child != external.to_string() && e.parent != external.to_string() }),
        "unresolved relationship target must not appear in edges"
    );
    // Both disjoint chains share the base lane: A and B both sit on lane 0.
    let max_lane = layout.rows.iter().map(|r| r.lane).max().unwrap_or(0);
    assert_eq!(max_lane, 0, "phantom target inflated the lane count");
}

/// A structural note whose anchor and target are both absent is inert input:
/// collapse must ignore it without panicking or manufacturing a visible row,
/// edge, component, or lane.
#[test]
fn fully_unresolved_relationship_note_is_ignored() {
    let missing_anchor = OpId::new(NodeId(98), 0, 1);
    let missing_target = OpId::new(NodeId(99), 0, 1);
    let note = relation_note(
        100,
        1,
        missing_anchor,
        missing_target,
        NoteRelationship::SubagentOf,
    );

    let projection = HistoryProjection::from_ops(vec![note]);
    assert!(projection.nodes().is_empty());
    assert!(projection.relationship_notes().is_empty());
    let layout = projection.graph_layout();
    assert!(layout.rows.is_empty());
    assert!(layout.edges.is_empty());
}

/// The fork-prologue fold drops the branch's duplicated pre-boundary rows. A
/// surviving branch node whose stored parent is a dropped prologue row must
/// resolve to the trunk boundary at the split — not to a phantom that stops a
/// later disjoint chain from reusing the base lane.
#[test]
fn fork_prologue_drop_does_not_inflate_later_chain_lanes() {
    // Trunk session A: roota -> a2 -> a3 -> a4 (a3 = divergence boundary).
    let roota = msg_op(1, 1, 10, 1_000, None);
    let a2 = msg_op(1, 2, 10, 2_000, Some(roota.id));
    let a3 = msg_op(1, 3, 10, 3_000, Some(a2.id));
    let a4 = msg_op(1, 4, 10, 4_000, Some(a3.id));
    // Branch B duplicates the prologue (b_roota, b_a2) then diverges at b3.
    // The fork runs AFTER the trunk in time, so the branch rows are newer and
    // the branch boundary's edge points DOWN to the trunk boundary.
    let b_roota = msg_op(2, 1, 20, 5_000, None);
    let b_a2 = msg_op(2, 2, 20, 6_000, Some(b_roota.id));
    let b3 = msg_op(2, 3, 20, 7_000, Some(b_a2.id));
    let b4 = msg_op(2, 4, 20, 8_000, Some(b3.id));
    let fork = relation_note(7, 0xFF0, b3.id, a3.id, NoteRelationship::ForkOf);
    // A later disjoint chain C must reuse the base lane.
    let c1 = msg_op(3, 1, 30, 100, None);
    let c2 = msg_op(3, 2, 30, 200, Some(c1.id));

    let projection = HistoryProjection::from_ops(vec![
        roota.clone(),
        a2.clone(),
        a3.clone(),
        a4.clone(),
        b_roota.clone(),
        b_a2.clone(),
        b3.clone(),
        b4.clone(),
        fork.clone(),
        c1.clone(),
        c2.clone(),
    ]);
    let layout = projection.graph_layout();

    // The branch boundary b3's stored parent (b_a2, a dropped prologue row)
    // resolves to the trunk boundary a3 — one edge to a visible row.
    assert!(
        layout
            .edges
            .iter()
            .any(|e| { e.child == b3.id.to_string() && e.parent == a3.id.to_string() }),
        "fork boundary must resolve to the trunk boundary; got {:#?}",
        layout
            .edges
            .iter()
            .map(|e| (e.child.as_str(), e.parent.as_str()))
            .collect::<Vec<_>>()
    );
    let lane_of = |op_id: OpId| {
        layout
            .rows
            .iter()
            .find(|r| r.node == op_id.to_string())
            .map_or(usize::MAX, |r| r.lane)
    };
    // The fork still renders on distinct lanes (branch b3 stays on the trunk
    // lane, the trunk continuation a4 diverges onto its own lane)...
    assert_ne!(
        lane_of(b3.id),
        lane_of(a4.id),
        "fork and trunk continuation diverge"
    );
    assert_eq!(
        lane_of(b4.id),
        lane_of(b3.id),
        "fork continuation stays with its branch"
    );
    // ...and the later disjoint chain REUSES the base lane 0 instead of being
    // pushed onto a fresh lane by a phantom dropped prologue row.
    assert_eq!(lane_of(c1.id), 0, "later disjoint chain must reuse lane 0");
    assert_eq!(lane_of(c2.id), 0, "later disjoint chain must reuse lane 0");
}

/// The fork-prologue fold must restrict prologue detection and representative
/// rewiring to the fork boundary's exact source chain `(OpId.node, OpId.boot)`.
/// One session may hold several source chains at once — the fork branch, the
/// trunk it forked from, and an unrelated chain sharing the same session id.
/// Lower-seq rows on those other chains must never be elided or redirected to
/// the trunk boundary just because they share the session with the branch, while
/// the intended same-chain duplicated-prologue fold still runs.
#[test]
fn fork_prologue_fold_ignores_unrelated_chains_in_same_session() {
    // ONE session (10) carries three distinct source chains: trunk T (node 1),
    // fork branch B (node 2, duplicating the trunk prologue), and an unrelated
    // chain U (node 3).
    let t1 = msg_op(1, 1, 10, 1_000, None);
    let t2 = msg_op(1, 2, 10, 2_000, Some(t1.id));
    let t3 = msg_op(1, 3, 10, 3_000, Some(t2.id));
    let t4 = msg_op(1, 4, 10, 4_000, Some(t3.id));
    let b1 = msg_op(2, 1, 10, 5_000, None);
    let b2 = msg_op(2, 2, 10, 6_000, Some(b1.id));
    let b3 = msg_op(2, 3, 10, 7_000, Some(b2.id));
    let b4 = msg_op(2, 4, 10, 8_000, Some(b3.id));
    let u1 = msg_op(3, 1, 10, 9_000, None);
    let u2 = msg_op(3, 2, 10, 10_000, Some(u1.id));
    let fork = relation_note(7, 0xFF0, b3.id, t3.id, NoteRelationship::ForkOf);

    let projection = HistoryProjection::from_ops(vec![
        t1.clone(),
        t2.clone(),
        t3.clone(),
        t4.clone(),
        b1.clone(),
        b2.clone(),
        b3.clone(),
        b4.clone(),
        u1.clone(),
        u2.clone(),
        fork,
    ]);

    // Only the branch's own duplicate prologue folds away: every trunk and
    // unrelated row stays a visible row, and the branch boundary + continuation
    // stay.
    let keys: Vec<String> = projection
        .nodes()
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    let visible = |id: OpId| keys.iter().any(|k| k == &id.to_string());
    assert!(
        visible(t1.id) && visible(t2.id) && visible(t3.id) && visible(t4.id),
        "trunk chain must survive the fold; got {keys:?}"
    );
    assert!(
        visible(u1.id) && visible(u2.id),
        "unrelated chain must survive the fold; got {keys:?}"
    );
    assert!(
        !visible(b1.id) && !visible(b2.id),
        "branch duplicate prologue must fold; got {keys:?}"
    );
    assert!(
        visible(b3.id) && visible(b4.id),
        "branch must stay; got {keys:?}"
    );

    // The branch boundary's stored parent (b2, a dropped prologue row) resolves
    // to the trunk boundary t3 — one edge to a visible row — and no edge ever
    // references a dropped id or rewires an unrelated/trunk row onto the trunk
    // boundary.
    let layout = projection.graph_layout();
    assert!(
        layout
            .edges
            .iter()
            .any(|e| { e.child == b3.id.to_string() && e.parent == t3.id.to_string() }),
        "fork boundary must resolve to the trunk boundary; got {:#?}",
        layout
            .edges
            .iter()
            .map(|e| (e.child.as_str(), e.parent.as_str()))
            .collect::<Vec<_>>()
    );
    for edge in &layout.edges {
        assert_ne!(
            edge.child,
            b1.id.to_string(),
            "dropped id in edges: {edge:?}"
        );
        assert_ne!(
            edge.child,
            b2.id.to_string(),
            "dropped id in edges: {edge:?}"
        );
        assert_ne!(
            edge.parent,
            b1.id.to_string(),
            "dropped id in edges: {edge:?}"
        );
        assert_ne!(
            edge.parent,
            b2.id.to_string(),
            "dropped id in edges: {edge:?}"
        );
    }
    // Trunk and unrelated causality stays intact: no row was rewired onto the
    // trunk boundary.
    assert!(
        layout
            .edges
            .iter()
            .any(|e| { e.child == t2.id.to_string() && e.parent == t1.id.to_string() }),
        "trunk prologue causality must stay intact"
    );
    assert!(
        layout
            .edges
            .iter()
            .any(|e| { e.child == u2.id.to_string() && e.parent == u1.id.to_string() }),
        "unrelated chain causality must stay intact"
    );
}

/// Filtered + windowed layout: after the default (hide-undated, splice) filter
/// and a virtual `SubagentOf` edge, every emitted edge must resolve to a row that
/// is present in the filtered layout, and the splice must reconnect across the
/// hidden undated row.
#[test]
fn filtered_layout_resolves_folded_relationship_endpoints() {
    let parent_import = import_op(1, 1, 10, 1_000);
    let spawn_marker = child_message_op(2, 1, parent_import.id, "spawned subagent");
    let sub_import = import_op(3, 1, 20, 3_000);
    let sub_first = child_message_op(4, 1, sub_import.id, "sub work");
    // An intermediate row on the subagent backbone whose summary matches the
    // hide pattern; it is not an endpoint (it has a parent and a child), so the
    // pattern-based hide truncation removes it and splice reconnects across it.
    let mut sub_middle = import_op(3, 2, 20, 3_500);
    sub_middle.parents = ParentSet::One(sub_import.id);
    if let OpKind::Import(i) = &mut sub_middle.kind {
        i.raw_ref = Payload::Inline(b"HIDE_ME".to_vec());
    }
    let mut sub_later = import_op(3, 3, 20, 4_000);
    sub_later.parents = ParentSet::One(sub_middle.id);
    let note = relation_note(
        5,
        1,
        sub_first.id,
        spawn_marker.id,
        NoteRelationship::SubagentOf,
    );

    let projection = HistoryProjection::from_ops(vec![
        parent_import.clone(),
        spawn_marker,
        sub_import.clone(),
        sub_first,
        sub_middle.clone(),
        sub_later.clone(),
        note,
    ]);

    let filter = editchain_project::filter::ChainFilter::new(
        "HIDE_ME".to_string(),
        String::new(),
        String::new(),
        false,
        true,
    );
    let nodes = projection.filtered_nodes(&filter);
    let keys: Vec<String> = nodes
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    assert!(
        !keys.contains(&sub_middle.id.to_string()),
        "pattern row hidden"
    );

    // Splice reconnects the later subagent row to the kept anchor row.
    let later = nodes
        .iter()
        .find(|n| n.node_key() == sub_later.id.to_string())
        .expect("later subagent row kept");
    assert_eq!(
        later.parent_keys(&projection.git.links, projection.relationship_notes()),
        vec![sub_import.id.to_string()],
        "splice must reconnect across the hidden undated row"
    );

    // Windowed edge geometry over the filtered rows: the virtual SubagentOf
    // edge resolves to the visible parent row, and every emitted endpoint is a
    // row present in the filtered layout (no phantom keys).
    let ctx = projection.layout_context(&nodes);
    let edges = ctx.edges_for_window(0, nodes.len());
    assert!(
        edges.iter().any(|e| {
            e.child == sub_import.id.to_string() && e.parent == parent_import.id.to_string()
        }),
        "filtered windowed layout must draw the SubagentOf edge to a visible row"
    );
    let present: std::collections::HashSet<String> = keys.iter().cloned().collect();
    for edge in &edges {
        assert!(
            present.contains(&edge.child),
            "child {} must be a visible row",
            edge.child
        );
        assert!(
            present.contains(&edge.parent),
            "parent {} must be a visible row",
            edge.parent
        );
    }
    let max_lane = ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0);
    assert_eq!(max_lane, 0, "no relationship endpoint may inflate lanes");
}

/// Include-kind ("messages only") filtering must preserve structural relation
/// anchor and target rows even when their kind (tool) matches the exclusion:
/// the rows that carry or point at a structural note are graph-topology-critical
/// and keep branch geometry visible in the filtered view. The relation's parent
/// must be one of the row's final parents AND be drawn in the filtered layout.
#[test]
fn include_kind_filter_preserves_structural_anchor_and_target_rows() {
    // The parent thread's spawn marker is a Tool op row (kind "tool"); the
    // subagent's first op is also a Tool op row (kind "tool"). Both would be
    // excluded by an INCLUSIVE "^message$" kind constraint unless the filter
    // preserves structural anchors/targets.
    let spawn = tool_op_row(1, 1, 10, 1_000, None);
    let sub_first = tool_op_row(2, 1, 20, 2_000, None);
    let note = relation_note(5, 1, sub_first.id, spawn.id, NoteRelationship::SubagentOf);

    let projection = HistoryProjection::from_ops(vec![spawn.clone(), sub_first.clone(), note]);

    let filter = editchain_project::filter::ChainFilter::new(
        String::new(),
        String::new(),
        "^message$".to_string(),
        false,
        true,
    );
    let nodes = projection.filtered_nodes(&filter);
    let keys: Vec<String> = nodes
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    assert!(
        keys.contains(&sub_first.id.to_string()),
        "structural anchor row must survive messages-only filtering; got {keys:?}"
    );
    assert!(
        keys.contains(&spawn.id.to_string()),
        "structural target row must survive messages-only filtering; got {keys:?}"
    );

    // The relation.parent is one of the anchor row's final parents in the
    // filtered view.
    let sub = nodes
        .iter()
        .find(|n| n.node_key() == sub_first.id.to_string())
        .expect("subagent first op kept");
    let parents = sub.parent_keys(&projection.git.links, projection.relationship_notes());
    assert!(
        parents.contains(&spawn.id.to_string()),
        "relation.parent {} must be in row.parents {parents:?}",
        spawn.id
    );

    // The filtered layout draws the SubagentOf edge, and every emitted edge
    // endpoint is a row present in the filtered layout (no phantom keys).
    let ctx = projection.layout_context(&nodes);
    let edges = ctx.edges_for_window(0, nodes.len());
    assert!(
        edges
            .iter()
            .any(|e| { e.child == sub_first.id.to_string() && e.parent == spawn.id.to_string() }),
        "filtered windowed layout must draw the SubagentOf edge to a visible row; got {:#?}",
        edges
            .iter()
            .map(|e| (e.child.as_str(), e.parent.as_str()))
            .collect::<Vec<_>>()
    );
    let present: std::collections::HashSet<String> = keys.iter().cloned().collect();
    for edge in &edges {
        assert!(
            present.contains(&edge.child),
            "child {} must be a visible row",
            edge.child
        );
        assert!(
            present.contains(&edge.parent),
            "parent {} must be a visible row",
            edge.parent
        );
    }
}

/// The canonical representative map covers every folded op: relationship notes
/// are keyed by visible anchors, and every edge the projection draws (layout,
/// lifted parents) resolves to a visible row — never to a folded op id.
#[test]
fn canonical_notes_reference_only_visible_rows() {
    let parent_import = import_op(1, 1, 10, 1_000);
    let marker = child_message_op(2, 1, parent_import.id, "marker");
    let sub_import = import_op(3, 1, 20, 2_000);
    let sub_first = child_message_op(4, 1, sub_import.id, "sub");
    let note = relation_note(5, 1, sub_first.id, marker.id, NoteRelationship::SubagentOf);

    let projection = HistoryProjection::from_ops(vec![
        parent_import.clone(),
        marker,
        sub_import.clone(),
        sub_first,
        note,
    ]);
    let visible: std::collections::HashSet<String> = projection
        .nodes()
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    // Every note is keyed by a canonical VISIBLE anchor row.
    let rel_notes = projection.relationship_notes();
    for anchor in rel_notes.keys() {
        assert!(
            visible.contains(&anchor.to_string()),
            "anchor {anchor} must be a visible row"
        );
    }
    // Lifting the anchor rows' parents resolves every folded target to a visible
    // row (the raw marker op is folded into `parent_import`'s row).
    let lifted = projection.lifted_parent_keys(
        &projection
            .nodes()
            .into_iter()
            .find(|n| n.node_key() == sub_import.id.to_string())
            .expect("sub import row"),
    );
    assert_eq!(lifted, vec![parent_import.id.to_string()]);
}

/// `parent_relations_for` must emit every distinct structural `(parent, kind)`
/// match for a row — not stop at the first note kind that matches a canonical
/// parent. A canonical edge can carry several relationship kinds at once (e.g.
/// Subagent + Fork) when notes with different relationships target the same
/// visible row, and exact duplicate relations are emitted exactly once.
#[test]
fn parent_relations_for_emits_every_distinct_kind_per_parent() {
    // Two notes anchor on the branch row B1, both targeting the SAME visible row
    // T1: one ForkOf (the branch forks off T1) and one SubagentOf (the branch was
    // spawned by T1). A third note duplicates the SubagentOf relationship exactly,
    // so the dedup path is exercised. The canonical parent edge T1 therefore
    // carries both kinds, once each.
    let t1 = import_op(1, 1, 10, 1_000);
    let b1 = import_op(2, 1, 20, 2_000);
    let fork_note = relation_note(5, 1, b1.id, t1.id, NoteRelationship::ForkOf);
    let sub_note = relation_note(6, 2, b1.id, t1.id, NoteRelationship::SubagentOf);
    let dup_sub_note = relation_note(7, 3, b1.id, t1.id, NoteRelationship::SubagentOf);

    let projection = HistoryProjection::from_ops(vec![
        t1.clone(),
        b1.clone(),
        fork_note,
        sub_note,
        dup_sub_note,
    ]);
    let row = projection
        .nodes()
        .into_iter()
        .find(|n| n.node_key() == b1.id.to_string())
        .expect("B1 renders as its own row");
    let relations = projection.parent_relations_for(&row, &[t1.id.to_string()]);

    // Deterministic order (parent order, then note order) with exact duplicates
    // removed: Fork first, Subagent second.
    assert_eq!(
        relations,
        vec![
            editchain_project::ParentRelation {
                parent: t1.id.to_string(),
                kind: editchain_project::RelationKind::Fork,
            },
            editchain_project::ParentRelation {
                parent: t1.id.to_string(),
                kind: editchain_project::RelationKind::Subagent,
            },
        ],
        "every distinct (parent, kind) must be emitted once"
    );
}
