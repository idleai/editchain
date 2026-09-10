//! Regression tests for the semantic-collapse invariant: relationship anchors
//! and targets (`ForkOf` / `SubagentOf` / `ReconnectsTo`) must resolve to canonical
//! VISIBLE rows even when their source ops are folded into a collapsed bundle,
//! and unresolved endpoints must never generate phantom intervals or inflate
//! the lane count.

// Crate-level dependency markers (used by Cargo for feature resolution).
use serde as _;
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

/// An exact provider-occurrence note with importer-owned payload evidence.
fn fingerprinted_occurrence_note(
    node: u64,
    seq: u64,
    parent: OpId,
    target: OpId,
    fingerprint: &str,
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
            relationship: NoteRelationship::OccurrenceOf,
            content: Payload::Inline(
                format!(r#"{{"confidence":"exact","payloadFingerprint":"{fingerprint}"}}"#)
                    .into_bytes(),
            ),
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

/// Exact provider event identity and payload evidence suppress copied
/// occurrences despite copy-local raw envelope drift, then use each branch's
/// own `ProviderParent` endpoint. No prefix, length, timestamp, or cross-stream
/// sequence comparison participates.
#[test]
fn exact_occurrences_form_a_provider_fork_without_self_edges() {
    let shared_entity = OpId::new(NodeId(90), 7, 1);
    let left_entity = OpId::new(NodeId(90), 7, 2);
    let right_entity = OpId::new(NodeId(90), 7, 3);

    let mut shared_left = import_op(1, 1, 10, 1_000);
    let mut shared_right = import_op(2, 1, 20, 1_000);
    if let (OpKind::Import(left), OpKind::Import(right)) =
        (&mut shared_left.kind, &mut shared_right.kind)
    {
        left.raw_hash = Some([1; 32]);
        right.raw_hash = Some([2; 32]);
    }
    let mut left = import_op(1, 2, 10, 2_000);
    left.parents = ParentSet::One(shared_left.id);
    let mut right = import_op(2, 2, 20, 3_000);
    right.parents = ParentSet::One(shared_right.id);

    let ops = vec![
        shared_left.clone(),
        shared_right.clone(),
        left.clone(),
        right.clone(),
        fingerprinted_occurrence_note(
            11,
            1,
            shared_left.id,
            shared_entity,
            "1111111111111111111111111111111111111111111111111111111111111111",
        ),
        fingerprinted_occurrence_note(
            12,
            1,
            shared_right.id,
            shared_entity,
            "1111111111111111111111111111111111111111111111111111111111111111",
        ),
        relation_note(13, 1, left.id, left_entity, NoteRelationship::OccurrenceOf),
        relation_note(
            14,
            1,
            left.id,
            shared_entity,
            NoteRelationship::ProviderParent,
        ),
        relation_note(
            15,
            1,
            right.id,
            right_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_note(
            16,
            1,
            right.id,
            shared_entity,
            NoteRelationship::ProviderParent,
        ),
    ];
    let projection = HistoryProjection::from_ops(ops);

    assert_eq!(projection.nodes().len(), 3);
    assert_eq!(
        projection.visible_op_id(shared_right.id),
        Some(shared_left.id)
    );
    for child in [&left, &right] {
        let row = projection
            .nodes()
            .into_iter()
            .find(|row| row.node_key() == child.id.to_string())
            .unwrap();
        assert_eq!(
            projection.lifted_parent_keys(&row),
            vec![shared_left.id.to_string()]
        );
        assert!(!projection
            .lifted_parent_keys(&row)
            .contains(&child.id.to_string()));
    }
    assert!(projection
        .graph_layout()
        .edges
        .iter()
        .all(|edge| edge.child != edge.parent));
}

/// A copied occurrence can name a different provider parent when one physical
/// transcript captured an incomplete/alternate parallel tool-result path. Once
/// the occurrences collapse to one visible row, only the surviving physical
/// occurrence supplies that row's incoming provider ancestry. The suppressed
/// copy must not turn an ordinary chain row into a fan-in merge.
#[test]
fn suppressed_copy_does_not_union_its_parent_into_the_shared_row() {
    let root_entity = OpId::new(NodeId(90), 7, 1);
    let alternate_entity = OpId::new(NodeId(90), 7, 2);
    let continuation_entity = OpId::new(NodeId(90), 7, 3);
    let root_fingerprint = "1111111111111111111111111111111111111111111111111111111111111111";
    let continuation_fingerprint =
        "2222222222222222222222222222222222222222222222222222222222222222";

    let root_left = import_op(1, 1, 10, 1_000);
    let root_right = import_op(2, 1, 20, 1_000);
    let mut alternate = import_op(2, 2, 20, 2_000);
    alternate.parents = ParentSet::One(root_right.id);
    let mut continuation_left = import_op(1, 2, 10, 3_000);
    continuation_left.parents = ParentSet::One(root_left.id);
    let mut continuation_right = import_op(2, 3, 20, 3_000);
    continuation_right.parents = ParentSet::One(alternate.id);

    let projection = HistoryProjection::from_ops(vec![
        root_left.clone(),
        root_right.clone(),
        alternate.clone(),
        continuation_left.clone(),
        continuation_right.clone(),
        fingerprinted_occurrence_note(11, 1, root_left.id, root_entity, root_fingerprint),
        fingerprinted_occurrence_note(12, 1, root_right.id, root_entity, root_fingerprint),
        relation_note(
            13,
            1,
            alternate.id,
            alternate_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_note(
            14,
            1,
            alternate.id,
            root_entity,
            NoteRelationship::ProviderParent,
        ),
        fingerprinted_occurrence_note(
            15,
            1,
            continuation_left.id,
            continuation_entity,
            continuation_fingerprint,
        ),
        relation_note(
            16,
            1,
            continuation_left.id,
            root_entity,
            NoteRelationship::ProviderParent,
        ),
        fingerprinted_occurrence_note(
            17,
            1,
            continuation_right.id,
            continuation_entity,
            continuation_fingerprint,
        ),
        relation_note(
            18,
            1,
            continuation_right.id,
            alternate_entity,
            NoteRelationship::ProviderParent,
        ),
    ]);

    assert_eq!(projection.visible_op_id(root_right.id), Some(root_left.id));
    assert_eq!(
        projection.visible_op_id(continuation_right.id),
        Some(continuation_left.id)
    );
    let continuation_row = projection
        .nodes()
        .into_iter()
        .find(|row| row.node_key() == continuation_left.id.to_string())
        .unwrap();
    assert_eq!(
        projection.lifted_parent_keys(&continuation_row),
        vec![root_left.id.to_string()]
    );
}

#[test]
fn provider_event_revisions_resolve_parents_within_their_source() {
    let shared_entity = OpId::new(NodeId(90), 8, 1);
    let left_entity = OpId::new(NodeId(90), 8, 2);
    let right_entity = OpId::new(NodeId(90), 8, 3);

    let mut shared_left = import_op(1, 1, 10, 1_000);
    let mut shared_right = import_op(2, 1, 20, 1_000);
    if let (OpKind::Import(left), OpKind::Import(right)) =
        (&mut shared_left.kind, &mut shared_right.kind)
    {
        left.raw_hash = Some([1; 32]);
        right.raw_hash = Some([2; 32]);
    }
    let left = import_op(1, 2, 10, 2_000);
    let right = import_op(2, 2, 20, 3_000);
    let projection = HistoryProjection::from_ops(vec![
        shared_left.clone(),
        shared_right.clone(),
        left.clone(),
        right.clone(),
        relation_note(
            21,
            1,
            shared_left.id,
            shared_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_note(
            22,
            1,
            shared_right.id,
            shared_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_note(23, 1, left.id, left_entity, NoteRelationship::OccurrenceOf),
        relation_note(
            24,
            1,
            left.id,
            shared_entity,
            NoteRelationship::ProviderParent,
        ),
        relation_note(
            25,
            1,
            right.id,
            right_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_note(
            26,
            1,
            right.id,
            shared_entity,
            NoteRelationship::ProviderParent,
        ),
    ]);

    assert_eq!(projection.nodes().len(), 4);
    assert_eq!(
        projection.visible_op_id(shared_left.id),
        Some(shared_left.id)
    );
    assert_eq!(
        projection.visible_op_id(shared_right.id),
        Some(shared_right.id)
    );
    for (child, parent) in [(&left, &shared_left), (&right, &shared_right)] {
        let row = projection
            .nodes()
            .into_iter()
            .find(|row| row.node_key() == child.id.to_string())
            .unwrap();
        assert_eq!(
            projection.lifted_parent_keys(&row),
            vec![parent.id.to_string()]
        );
    }
}

/// Immutable notes from the retired Claude fork detector remain stored but are
/// inert in current projection. In particular they cannot delete a whole source
/// prefix or introduce a canonical self-parent.
#[test]
fn legacy_inferred_claude_fork_note_is_inert() {
    let trunk = msg_op(1, 1, 10, 1_000, None);
    let branch_root = msg_op(2, 1, 20, 1_000, None);
    let branch = msg_op(2, 2, 20, 2_000, Some(branch_root.id));
    let mut legacy = relation_note(
        2,
        (2 << 16) | 0xFFFE,
        branch.id,
        branch.id,
        NoteRelationship::ForkOf,
    );
    legacy.tags = Tags::META | Tags::IMPORT;

    let projection = HistoryProjection::from_ops(vec![
        trunk.clone(),
        branch_root.clone(),
        branch.clone(),
        legacy,
    ]);
    let keys: Vec<String> = projection
        .nodes()
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();

    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&trunk.id.to_string()));
    assert!(keys.contains(&branch_root.id.to_string()));
    assert!(keys.contains(&branch.id.to_string()));
    assert!(projection
        .graph_layout()
        .edges
        .iter()
        .all(|edge| edge.child != edge.parent));
}

/// Activity + windowed layout: after omitting and splicing an undated row and
/// adding a virtual `SubagentOf` edge, every emitted edge must resolve to a row
/// that is present in the Activity layout, and the splice must reconnect across the
/// hidden undated row.
#[test]
fn filtered_layout_resolves_folded_relationship_endpoints() {
    let parent_import = import_op(1, 1, 10, 1_000);
    let spawn_marker = child_message_op(2, 1, parent_import.id, "spawned subagent");
    let sub_import = import_op(3, 1, 20, 3_000);
    let sub_first = child_message_op(4, 1, sub_import.id, "sub work");
    // An undated intermediate row on the subagent backbone is omitted by the
    // Activity view, which must splice its dated neighbors back together.
    let mut sub_middle = import_op(3, 2, 20, 3_500);
    sub_middle.parents = ParentSet::One(sub_import.id);
    sub_middle.clock = Clock::UnixMs(0);
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

    let nodes = projection.activity_nodes();
    let keys: Vec<String> = nodes
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    assert!(
        !keys.contains(&sub_middle.id.to_string()),
        "undated row hidden"
    );

    // Splice reconnects the later subagent row to the kept anchor row.
    let later = nodes
        .iter()
        .find(|n| n.node_key() == sub_later.id.to_string())
        .expect("later subagent row kept");
    assert_eq!(
        later.parent_keys(&projection.git().links, projection.relationship_notes()),
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

    let evidence = [fork_note.id, sub_note.id, dup_sub_note.id];
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
    let graph = projection.resolved_graph(&projection.nodes());
    let relations = graph.relations(editchain_project::NodeKey::Op(b1.id));
    assert_eq!(relations.len(), 2);
    assert_eq!(relations.first().unwrap().evidence, vec![evidence[0]]);
    assert_eq!(
        relations.last().unwrap().evidence,
        vec![evidence[1], evidence[2]]
    );
    assert!(relations.iter().all(|relation| graph
        .parents(editchain_project::NodeKey::Op(b1.id))
        .contains(&relation.parent)));
}
