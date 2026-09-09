//! Regression tests for topology-preserving chronological display ordering.
//!
//! The projection must never apply a global timestamp sort after scheduling:
//! when causal clocks across sessions are inconsistent (a child carries an
//! OLDER timestamp than its parent), a plain newest-first timestamp sort can
//! lift the parent above the child, and the layout then refuses to draw the
//! connector (`LayoutContext` only draws edges whose parent row is strictly
//! below the child row). These tests deliberately invert parent/child clocks —
//! including through folded `SubagentOf` / `ReconnectsTo` structural endpoints
//! and across independent chains — and assert every layout parent row stays
//! below its child while cross-lane transition geometry still renders.

// Crate-level dependency markers (used by Cargo for feature resolution).
use serde as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, ImportOp, MessageOp, NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind,
    ParentSet, Payload, ScopeRef, SessionId, Tags, ToolOp, ToolStage,
};
use editchain_project::{HistoryNode, HistoryProjection};

/// A standalone message op (its own row).
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

/// A raw import op (the linear backbone row).
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

/// Assert that every layout edge over the full row list has its parent strictly
/// below its child, that every edge endpoint is a visible row, and that each
/// edge path descends monotonically from the child row to the parent row (the
/// connector geometry survives).
#[expect(
    clippy::indexing_slicing,
    reason = "Each pair comes from windows(2), so both indexed elements exist"
)]
fn assert_edges_point_downward(projection: &HistoryProjection, nodes: &[HistoryNode]) {
    let ctx = projection.layout_context(nodes);
    let edges = ctx.edges_for_window(0, nodes.len());
    assert!(!edges.is_empty(), "expected at least one layout edge");
    for edge in &edges {
        let child_row = ctx.row_of.get(&edge.child).copied().unwrap_or(usize::MAX);
        let parent_row = ctx.row_of.get(&edge.parent).copied().unwrap_or(usize::MAX);
        assert!(
            ctx.row_of.contains_key(&edge.child) && ctx.row_of.contains_key(&edge.parent),
            "edge endpoint must be a visible row: {edge:?}"
        );
        assert!(
            parent_row > child_row,
            "parent {} (row {parent_row}) must be below child {} (row {child_row})",
            edge.parent,
            edge.child
        );
        assert_eq!(
            edge.points.first().map(|p| p.row),
            Some(child_row),
            "edge path must start at the child row: {edge:?}"
        );
        assert_eq!(
            edge.points.last().map(|p| p.row),
            Some(parent_row),
            "edge path must end at the parent row: {edge:?}"
        );
        for pair in edge.points.windows(2) {
            assert!(
                pair[1].row >= pair[0].row,
                "edge path must never go back up: {edge:?}"
            );
        }
    }
}

/// The minimal inversion: a parent with a NEWER clock and a child with an OLDER
/// clock. A global newest-first timestamp sort would lift the parent above the
/// child and the connector would vanish; the schedule must keep the child on
/// top.
#[test]
fn inverted_timestamps_keep_parent_below_child() {
    let parent = msg_op(1, 1, 10, 100_000, None);
    let child = msg_op(1, 2, 10, 1_000, Some(parent.id));

    let projection = HistoryProjection::from_ops(vec![parent.clone(), child.clone()]);
    let nodes = projection.nodes();
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    assert_eq!(
        keys,
        vec![child.id.to_string(), parent.id.to_string()],
        "inverted clocks must not reorder the parent above its child"
    );

    // The paged window stays in lockstep with the canonical row list.
    let window_keys: Vec<String> = projection
        .window(0, nodes.len())
        .iter()
        .map(HistoryNode::node_key)
        .collect();
    assert_eq!(window_keys, keys);

    assert_edges_point_downward(&projection, &nodes);
}

/// A merge whose parents carry NEWER clocks than the merge child. All parents
/// render below the child and the secondary-parent edge still draws its
/// cross-lane horizontal jog (transition geometry survives).
#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Each pair comes from windows(2), so both indexed elements exist"
)]
fn inverted_timestamps_merge_preserves_cross_lane_transitions() {
    let a = msg_op(1, 1, 10, 100_000, None);
    let b = msg_op(2, 1, 10, 90_000, None);
    let m = Op {
        id: OpId::new(NodeId(3), 0, 1),
        parents: ParentSet::Two(a.id, b.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(500),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"merge".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    let projection = HistoryProjection::from_ops(vec![a.clone(), b.clone(), m.clone()]);
    let nodes = projection.nodes();
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    // The merge child renders above both parents; among the independent parents
    // the newer clock renders above the older one.
    assert_eq!(
        keys,
        vec![m.id.to_string(), a.id.to_string(), b.id.to_string()],
        "merge child must stay above both parents"
    );

    assert_edges_point_downward(&projection, &nodes);

    let ctx = projection.layout_context(&nodes);
    let edges = ctx.edges_for_window(0, nodes.len());
    let has_jog = edges
        .iter()
        .any(|e| e.points.windows(2).any(|w| w[0].lane != w[1].lane));
    assert!(has_jog, "merge edge must include a lane jog: {edges:?}");
    assert!(
        !ctx.row_transitions.is_empty(),
        "merge must emit per-row lane transitions"
    );
}

/// Independent chains: chain A is clock-consistent (child newer), chain B is
/// inverted (child older than its parent). Both keep parents below children,
/// independent roots still interleave newest-first by clock, and the order is
/// deterministic across repeated builds.
#[test]
fn inverted_timestamps_independent_chains_interleave_deterministically() {
    let a1 = msg_op(1, 1, 10, 1_000, None);
    let a2 = msg_op(1, 2, 10, 100_000, Some(a1.id));
    let b1 = msg_op(2, 1, 20, 100_000, None);
    let b2 = msg_op(2, 2, 20, 1_000, Some(b1.id));

    let projection =
        HistoryProjection::from_ops(vec![a1.clone(), a2.clone(), b1.clone(), b2.clone()]);
    let nodes = projection.nodes();
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    let row = |id: OpId| {
        keys.iter()
            .position(|k| *k == id.to_string())
            .expect("row id present in node keys")
    };
    // Both chains keep the child above the parent, even though B's child clock
    // is OLDER than B's parent clock.
    assert!(
        row(a2.id) < row(a1.id),
        "chain A child must be above its parent"
    );
    assert!(
        row(b2.id) < row(b1.id),
        "chain B child must be above its parent"
    );
    // Among the two eligible roots the newer clock renders above.
    assert!(
        row(b1.id) < row(a1.id),
        "newest eligible root must render above"
    );
    // The exact schedule is deterministic: child-then-parent per chain, newest
    // eligible root between the two chains.
    assert_eq!(
        keys,
        vec![
            a2.id.to_string(),
            b2.id.to_string(),
            b1.id.to_string(),
            a1.id.to_string(),
        ]
    );

    assert_edges_point_downward(&projection, &nodes);

    let rebuilt = HistoryProjection::from_ops(vec![a1, a2, b1, b2]).nodes();
    assert_eq!(
        keys,
        rebuilt
            .iter()
            .map(HistoryNode::node_key)
            .collect::<Vec<_>>()
    );
}

/// The observed Codex failure shape with inverted clocks: two `SubagentOf`
/// virtual edges from folded subagent endpoints back to one folded parent
/// marker. Both subagent rows (older clocks) render above the parent row, every
/// edge points downward, and the fork still draws a cross-lane transition.
#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Each pair comes from windows(2), so both indexed elements exist"
)]
fn inverted_timestamps_folded_subagentof_endpoints_keep_edges_downward() {
    let parent_import = import_op(1, 1, 10, 100_000);
    let spawn1 = child_message_op(2, 1, parent_import.id, "spawned subagent 1");
    let spawn2 = child_message_op(2, 2, parent_import.id, "spawned subagent 2");
    let sub1_import = import_op(3, 1, 20, 500);
    let sub1_first = child_message_op(4, 1, sub1_import.id, "sub work 1");
    let sub2_import = import_op(5, 1, 30, 400);
    let sub2_first = child_message_op(6, 1, sub2_import.id, "sub work 2");
    let note1 = relation_note(7, 1, sub1_first.id, spawn1.id, NoteRelationship::SubagentOf);
    let note2 = relation_note(7, 2, sub2_first.id, spawn2.id, NoteRelationship::SubagentOf);

    let projection = HistoryProjection::from_ops(vec![
        parent_import.clone(),
        spawn1,
        sub1_import.clone(),
        sub1_first,
        spawn2,
        sub2_import.clone(),
        sub2_first,
        note1,
        note2,
    ]);
    let nodes = projection.nodes();
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    // Only the three raw import rows render; folded children and notes fold out.
    assert_eq!(keys.len(), 3, "got {keys:?}");

    let row = |id: OpId| {
        keys.iter()
            .position(|k| *k == id.to_string())
            .expect("row id present in node keys")
    };
    // Both subagent rows (older clocks) render above the parent thread row.
    assert!(
        row(sub1_import.id) < row(parent_import.id),
        "subagent 1 must render above the parent row: {keys:?}"
    );
    assert!(
        row(sub2_import.id) < row(parent_import.id),
        "subagent 2 must render above the parent row: {keys:?}"
    );
    // Newer clock first among the two independent subagent branches.
    assert!(
        row(sub1_import.id) < row(sub2_import.id),
        "newer subagent clock must render above: {keys:?}"
    );

    assert_edges_point_downward(&projection, &nodes);

    // Both virtual SubagentOf edges render to the visible parent row, and the
    // fork off the shared parent draws a cross-lane transition.
    let ctx = projection.layout_context(&nodes);
    let edges = ctx.edges_for_window(0, nodes.len());
    assert!(
        edges.iter().any(|e| {
            e.child == sub1_import.id.to_string() && e.parent == parent_import.id.to_string()
        }),
        "SubagentOf edge 1 must render: {edges:?}"
    );
    assert!(
        edges.iter().any(|e| {
            e.child == sub2_import.id.to_string() && e.parent == parent_import.id.to_string()
        }),
        "SubagentOf edge 2 must render: {edges:?}"
    );
    let has_jog = edges
        .iter()
        .any(|e| e.points.windows(2).any(|w| w[0].lane != w[1].lane));
    assert!(
        has_jog,
        "forked virtual edges must include a lane jog: {edges:?}"
    );
    assert!(
        !ctx.row_transitions.is_empty(),
        "fork must emit per-row lane transitions"
    );
}

/// The observed Codex failure shape for reconnects with inverted clocks: a
/// folded `ReconnectsTo` completion marker (child end, OLDER clock) reconnects
/// down to the folded subagent endpoint (parent end, NEWER clock). The edge
/// must still render with the parent row below the child row.
#[test]
fn inverted_timestamps_folded_reconnects_to_endpoint_keeps_edge_downward() {
    let sub_import = import_op(1, 1, 10, 100_000);
    let sub_last = child_tool_op(2, 1, sub_import.id, "Bash");
    let collab_import = import_op(3, 1, 20, 500);
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
        sub_last,
        collab_import.clone(),
        collab_tool,
        note,
    ]);
    let nodes = projection.nodes();
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    assert_eq!(keys.len(), 2, "got {keys:?}");
    let row = |id: OpId| {
        keys.iter()
            .position(|k| *k == id.to_string())
            .expect("row id present in node keys")
    };
    // The reconnect child (older clock) must still render above its target.
    assert!(
        row(collab_import.id) < row(sub_import.id),
        "collab row must be above the sub row: {keys:?}"
    );

    assert_edges_point_downward(&projection, &nodes);

    let ctx = projection.layout_context(&nodes);
    let edges = ctx.edges_for_window(0, nodes.len());
    assert!(
        edges.iter().any(|e| {
            e.child == collab_import.id.to_string() && e.parent == sub_import.id.to_string()
        }),
        "ReconnectsTo edge must render: {edges:?}"
    );
}

/// Unknown time remains unknown even around inverted clocks, while topology
/// still keeps each present parent below its child.
#[test]
fn undated_header_stays_unknown_with_inverted_clocks() {
    let a_header = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::None,
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::META,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"header".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    // Session A's chain is INVERTED: the child carries the older clock.
    let a1 = msg_op(1, 2, 10, 100_000, None);
    let a2 = msg_op(1, 3, 10, 1_000, Some(a1.id));
    // Session B is globally newer.
    let b1 = msg_op(2, 5_000, 20, 5_000, None);

    let projection =
        HistoryProjection::from_ops(vec![a_header.clone(), a1.clone(), a2.clone(), b1.clone()]);
    let nodes = projection.nodes();
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    let row = |id: OpId| {
        keys.iter()
            .position(|k| *k == id.to_string())
            .expect("row id present in node keys")
    };
    let header = nodes
        .iter()
        .find(|n| n.node_key() == a_header.id.to_string())
        .expect("header row");

    // No session/global timestamp is borrowed.
    assert_eq!(header.timestamp_ms(), 0);
    let b1_node = nodes
        .iter()
        .find(|n| n.node_key() == b1.id.to_string())
        .expect("b1 row");
    assert!(
        header.timestamp_ms() < b1_node.timestamp_ms(),
        "unknown time must not inherit the newer session's date"
    );

    // The inverted chain keeps the child above the parent, and the header stays
    // below its own session's chain.
    assert!(
        row(a2.id) < row(a1.id),
        "inverted chain must keep child above parent"
    );
    assert!(
        row(a1.id) < row(a_header.id),
        "header must render below its own session's chain: {keys:?}"
    );

    assert_edges_point_downward(&projection, &nodes);
    let window_keys: Vec<String> = projection
        .window(0, nodes.len())
        .iter()
        .map(HistoryNode::node_key)
        .collect();
    assert_eq!(window_keys, keys);
}
