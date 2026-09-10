//! Tests for the Activity-view work-unit markers, promotion, and execute-run
//! bundling semantics.

#![expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_arguments,
    reason = "Test helpers and assertions; panics/indexing are acceptable in tests"
)]

// Crate-level dependency markers (used by Cargo for feature resolution).
use serde as _;
use serde_json as _;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use editchain_core::{
    ActorId, Clock, FileEdit, FileOp, FileStage, ImportOp, MessageOp, NodeId, Op, OpId, OpKind,
    ParentSet, PathId, Payload, ScopeRef, SessionId, Tags, ToolOp, ToolStage, TurnId,
};
use editchain_project::activity::{
    annotate_activity_rows, bundle_activity_execute_runs, bundle_activity_plan_repeats,
    bundle_activity_work_groups, bundle_claude_response_tool_fragments,
    inline_context_compaction_checkpoints, ActivityRowAnnotation,
};
use editchain_project::meta::NodeMeta;
use editchain_project::taxonomy::{ActivityKind, ChainState, Outcome, RecordRole, Visibility};
use editchain_project::{EffectiveTime, HistoryNode, HistoryProjection};

struct IdentityPresentation;

impl editchain_project::activity_view::ActivityPresentation for IdentityPresentation {
    type Row = editchain_project::NodeKey;

    fn activity(&self, node: &HistoryNode) -> Self::Row {
        node.key()
    }

    fn details(&self, node: &HistoryNode) -> Vec<Self::Row> {
        node.sub_ops()
            .iter()
            .map(|op| editchain_project::NodeKey::Op(op.id))
            .collect()
    }
}

#[test]
fn complete_activity_view_shares_tree_coordinates_and_source_owners() {
    let mut ops = linear_chain(
        1,
        "request",
        &[
            row_spec(40, 2, "tool", Some("completed"), 7),
            row_spec(40, 3, "tool", Some("completed"), 7),
            row_spec(40, 4, "tool", Some("completed"), 7),
            row_spec(40, 5, "file", None, 7),
            row_spec(40, 6, "message", None, 7),
        ],
    );
    let tool_id = OpId::new(NodeId(40), 0, 40);
    ops.push(world_state_import_op(40, 70, tool_id));
    let projection = HistoryProjection::from_ops(ops.clone());
    let view = projection.build_activity_view(|_| true, &IdentityPresentation);
    let entries = view.entries();
    assert_eq!(entries.len(), 3, "answer / work / request");
    assert!(view.layout().is_none(), "layout remains deferred");
    let group = &entries[1];
    assert_eq!(group.node().activity_kind(), ActivityKind::Work);
    assert_eq!(group.node().represented_activity_count(), 4);
    assert_eq!(
        group.children(0).count(),
        3,
        "edit, state-bearing tool, and nested execute bundle"
    );
    let nested = group
        .descendants()
        .iter()
        .position(|row| {
            *row.content() == editchain_project::NodeKey::Op(OpId::new(NodeId(40), 0, 30))
        })
        .expect("nested bundle");
    let nested_row = &group.descendants()[nested];
    assert_eq!(nested_row.depth(), 1);
    assert_eq!(
        nested_row.descendant_count(),
        2,
        "the two earlier tools stay bundled"
    );
    assert_eq!(group.children(nested + 1).count(), 2);
    for (index, row) in group.descendants().iter().enumerate().skip(nested + 1) {
        assert_eq!(row.parent_relative(), nested + 1);
        assert_eq!(row.depth(), 2);
        assert_eq!(group.children(index + 1).count(), 0);
    }
    assert_eq!(view.starts(), &[0, 1, 8, 9]);
    assert_eq!(view.expanded_total(), 9);
    assert_eq!(view.sub_op_counts(), vec![0, 6, 0]);
    assert_eq!(
        view.expansion_spans(),
        vec![
            editchain_project::activity_view::ExpansionSpan {
                row: 1,
                descendant_count: 6
            },
            editchain_project::activity_view::ExpansionSpan {
                row: 3,
                descendant_count: 1
            },
            editchain_project::activity_view::ExpansionSpan {
                row: 5,
                descendant_count: 2
            },
        ]
    );
    for op in &ops {
        assert!(
            view.source_disposition(editchain_project::NodeKey::Op(op.id))
                .is_some(),
            "accepted source {} has an owner",
            op.id
        );
    }
    assert_eq!(
        view.source_row(editchain_project::NodeKey::Op(tool_id)),
        Some(1)
    );
    let layout = view.ensure_layout();
    for key in view.graph().keys() {
        assert_eq!(
            layout
                .parents
                .get(&key.to_string())
                .cloned()
                .unwrap_or_default(),
            view.graph()
                .parents(*key)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
    }
    assert!(std::ptr::eq(layout, view.ensure_layout()));
    assert_eq!(
        projection.ops(),
        &ops,
        "view construction preserves every source envelope"
    );
}

/// Raw JSONL for an import row with an optional structured tool/command status.
fn raw_line(status: Option<&str>) -> String {
    match status {
        Some(status) => serde_json::json!({
            "type": "response_item",
            "payload": { "item": { "status": status } }
        })
        .to_string(),
        None => r#"{"type":"response_item","payload":{}}"#.to_string(),
    }
}

/// A raw import op (session-scoped, IMPORT tag) with optional parent/status.
fn import_op(node: u64, seq: u64, parent: Option<OpId>, status: Option<&str>) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq * 1_000),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(raw_line(status).into_bytes()),
            raw_hash: None,
        }),
    }
}

/// A session-scoped Claude assistant content block with an exact Anthropic
/// response identity (matching historical imports that predate turn scopes).
fn claude_assistant_import(node: u64, seq: u64, parent: OpId, message_id: &str) -> Op {
    let mut op = import_op(node, seq, Some(parent), None);
    if let OpKind::Import(import) = &mut op.kind {
        import.raw_ref = Payload::Inline(
            serde_json::json!({
                "type": "assistant",
                "message": { "id": message_id }
            })
            .to_string()
            .into_bytes(),
        );
    }
    op
}

/// A META raw import op carrying a `world_state` record (sub-op fodder), kept
/// on the same source chain so the projection bundles it onto an anchor row.
fn world_state_import_op(node: u64, seq: u64, parent: OpId) -> Op {
    let mut op = Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq * 1_000),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::IMPORT | Tags::META,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(br#"{"type":"world_state","payload":{}}"#.to_vec()),
            raw_hash: None,
        }),
    };
    op.tags |= Tags::META;
    op
}

/// A turn-scoped normalized tool child of `parent`.
fn tool_child(node: u64, seq: u64, parent: OpId, name: &str, turn: u64) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq * 1_000),
        scope: ScopeRef::Turn(TurnId(turn)),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(name.as_bytes().to_vec()),
            stage: ToolStage::Start,
            content: Payload::Empty,
        }),
    }
}

/// A turn-scoped normalized message child of `parent`.
fn message_child(node: u64, seq: u64, parent: OpId, text: &str, turn: u64) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq * 1_000),
        scope: ScopeRef::Turn(TurnId(turn)),
        tags: Tags::HUMAN | Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.as_bytes().to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// A turn-scoped normalized file child of `parent` (Change activity evidence).
fn file_child(node: u64, seq: u64, parent: OpId, turn: u64) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq * 1_000),
        scope: ScopeRef::Turn(TurnId(turn)),
        tags: Tags::AGENT | Tags::FILE,
        kind: OpKind::File(FileOp {
            path: PathId(42),
            stage: FileStage::Applied,
            base: None,
            after: None,
            edit: FileEdit::None,
        }),
    }
}

/// One linear session chain row descriptor (causal order oldest -> newest).
#[derive(Clone, Copy)]
struct RowSpec {
    node: u64,
    seq: u64,
    kind: &'static str,
    status: Option<&'static str>,
    turn: u64,
}

const fn row_spec(
    node: u64,
    seq: u64,
    kind: &'static str,
    status: Option<&'static str>,
    turn: u64,
) -> RowSpec {
    RowSpec {
        node,
        seq,
        kind,
        status,
        turn,
    }
}

/// Build a linear session chain: a user-message root import followed by the
/// given row imports (each parented to the previous import, oldest causal
/// first), plus the turn-scoped normalized child that classifies each row.
fn linear_chain(root_seq: u64, root_text: &str, rows: &[RowSpec]) -> Vec<Op> {
    let mut ops = Vec::new();
    let first_turn = rows.first().map_or(1, |row| row.turn);
    let root_import = import_op(1, root_seq, None, None);
    ops.push(root_import.clone());
    ops.push(message_child(
        101,
        root_seq + 100,
        root_import.id,
        root_text,
        first_turn,
    ));
    let mut previous_import = root_import;
    for row in rows {
        let seq = row.seq.saturating_mul(10);
        let current_import = import_op(row.node, seq, Some(previous_import.id), row.status);
        ops.push(current_import.clone());
        let child_seq = seq.saturating_add(500);
        let child = match row.kind {
            "message" => message_child(
                row.node + 200,
                child_seq,
                current_import.id,
                "text",
                row.turn,
            ),
            "tool" => tool_child(
                row.node + 200,
                child_seq,
                current_import.id,
                "Bash",
                row.turn,
            ),
            "file" => file_child(row.node + 200, child_seq, current_import.id, row.turn),
            other => panic!("unknown row kind {other}"),
        };
        ops.push(child);
        previous_import = current_import;
    }
    ops
}

/// Project ops and return the Activity-filtered node list + annotations.
fn activity_view(ops: Vec<Op>) -> (Vec<HistoryNode>, Vec<ActivityRowAnnotation>) {
    let projection = HistoryProjection::from_ops(ops);
    let nodes = projection.activity_nodes();
    let annotations = annotate_activity_rows(&nodes);
    (nodes, annotations)
}

#[test]
fn context_compaction_stays_visible_and_is_inlined_without_changing_raw() {
    // Legacy shape: root has two children in one physical source stream — the
    // compaction checkpoint and the immediately following continuation. Raw
    // retains those exact siblings; Activity routes continuation through the
    // visible checkpoint so all three render on one lane.
    let root = import_op(7, 1, None, None);
    let root_message = message_child(107, 101, root.id, "request", 1);
    let mut compacted = import_op(7, 2, Some(root.id), None);
    compacted.tags |= Tags::STRUCTURAL;
    compacted.kind = OpKind::Import(ImportOp {
        raw_ref: Payload::Inline(
            br#"{"type":"compacted","payload":{"message":"","replacement_history":[]}}"#.to_vec(),
        ),
        raw_hash: None,
    });
    let continuation = import_op(7, 3, Some(root.id), None);
    let continuation_message = message_child(207, 103, continuation.id, "continued", 1);
    let projection = HistoryProjection::from_ops(vec![
        root.clone(),
        root_message,
        compacted.clone(),
        continuation.clone(),
        continuation_message,
    ]);

    let raw = projection.nodes();
    let raw_continuation = raw
        .iter()
        .find(|node| node.node_key() == continuation.id.to_string())
        .unwrap_or_else(|| panic!("raw continuation missing"));
    assert_eq!(
        raw_continuation.parent_keys(projection.git().links(), projection.relationship_notes()),
        vec![root.id.to_string()],
        "Raw keeps the imported sibling topology"
    );

    let filtered = projection.activity_nodes();
    let structural = projection.structural_row_keys(&filtered);
    let activity = inline_context_compaction_checkpoints(filtered, &structural);
    assert_eq!(activity.len(), 3, "the checkpoint remains a visible row");
    let checkpoint = activity
        .iter()
        .find(|node| node.node_key() == compacted.id.to_string())
        .unwrap_or_else(|| panic!("Activity checkpoint missing"));
    assert_eq!(checkpoint.visibility(), Visibility::Primary);
    assert_eq!(checkpoint.activity_kind(), ActivityKind::Plan);
    assert_eq!(
        checkpoint.parent_keys(projection.git().links(), projection.relationship_notes()),
        vec![root.id.to_string()]
    );
    let activity_continuation = activity
        .iter()
        .find(|node| node.node_key() == continuation.id.to_string())
        .unwrap_or_else(|| panic!("Activity continuation missing"));
    assert_eq!(
        activity_continuation
            .parent_keys(projection.git().links(), projection.relationship_notes()),
        vec![compacted.id.to_string()],
        "Activity inserts the continuation after the visible checkpoint"
    );
    let layout = projection.graph_layout_filtered(&activity);
    assert!(
        layout.rows.iter().all(|row| row.lane == 0),
        "an inline checkpoint must not allocate a branch lane"
    );
}

#[test]
fn context_compaction_does_not_rewire_a_structural_continuation() {
    let root = import_op(8, 1, None, None);
    let root_message = message_child(108, 101, root.id, "request", 1);
    let mut compacted = import_op(8, 2, Some(root.id), None);
    compacted.kind = OpKind::Import(ImportOp {
        raw_ref: Payload::Inline(br#"{"type":"compacted","payload":{"message":""}}"#.to_vec()),
        raw_hash: None,
    });
    let continuation = import_op(8, 3, Some(root.id), None);
    let continuation_message = message_child(208, 103, continuation.id, "continued", 1);
    let projection = HistoryProjection::from_ops(vec![
        root.clone(),
        root_message,
        compacted,
        continuation.clone(),
        continuation_message,
    ]);
    let filtered = projection.activity_nodes();
    let structural = HashSet::from([editchain_project::NodeKey::Op(continuation.id)]);
    let activity = inline_context_compaction_checkpoints(filtered, &structural);
    let kept = activity
        .iter()
        .find(|node| node.node_key() == continuation.id.to_string())
        .unwrap_or_else(|| panic!("structural continuation missing"));
    assert_eq!(
        kept.parent_keys(projection.git().links(), projection.relationship_notes()),
        vec![root.id.to_string()],
        "structural fork/subagent/reconnect endpoints are never rewritten"
    );
}

/// Run the Activity bundling pass over a node list with fresh annotations.
fn bundle(nodes: Vec<HistoryNode>) -> Vec<HistoryNode> {
    let annotations = annotate_activity_rows(&nodes);
    bundle_activity_execute_runs(nodes, &annotations, &HashSet::new())
}

/// Unwrap an execute-run bundle from a node list.
fn unwrap_bundle(nodes: &[HistoryNode]) -> &HistoryNode {
    nodes
        .iter()
        .find(|node| matches!(node, HistoryNode::ExecuteBundle { .. }))
        .unwrap_or_else(|| panic!("expected an ExecuteBundle row"))
}

/// Build a manually classified collapsed row (used where display-order or
/// metadata control is needed beyond what the projection emits).
fn manual_collapsed(
    op: Op,
    activity_kind: ActivityKind,
    record_role: RecordRole,
    outcome: Outcome,
    turn: Option<TurnId>,
    summary: &str,
    kind: &str,
) -> HistoryNode {
    HistoryNode::CollapsedImport {
        op: Arc::new(op),
        source_time: EffectiveTime::Observed(0),
        parent_override: None,
        content: editchain_project::content::SelectedContent::summary(summary.to_string()),
        kind: kind.to_string(),
        author: "agent".to_string(),
        sub_ops: Vec::new(),
        meta: NodeMeta {
            record_role,
            activity_kind,
            visibility: Visibility::Primary,
            outcome,
            chain_state: ChainState::Active,
            turn_id: turn,
        },
    }
}

#[test]
fn work_groups_contract_all_linear_activity_between_chat_rows() {
    let request_op = import_op(41, 1, None, None);
    let execute_op = import_op(42, 2, Some(request_op.id), Some("completed"));
    let change_op = import_op(43, 3, Some(execute_op.id), None);
    let response_op = import_op(44, 4, Some(change_op.id), None);
    let response = manual_collapsed(
        response_op,
        ActivityKind::Conversation,
        RecordRole::Narrative,
        Outcome::Unknown,
        Some(TurnId(7)),
        "Implemented the requested change",
        "message",
    );
    let change = manual_collapsed(
        change_op,
        ActivityKind::Change,
        RecordRole::Artifact,
        Outcome::Success,
        Some(TurnId(7)),
        "file: src/history.rs",
        "file",
    );
    let execute = manual_collapsed(
        execute_op,
        ActivityKind::Execute,
        RecordRole::Action,
        Outcome::Success,
        Some(TurnId(7)),
        "cargo test",
        "command",
    );
    let request = manual_collapsed(
        request_op,
        ActivityKind::Conversation,
        RecordRole::Narrative,
        Outcome::Unknown,
        Some(TurnId(7)),
        "Please group the work",
        "message",
    );

    let grouped =
        bundle_activity_work_groups(vec![response, change, execute, request], &HashSet::new());
    assert_eq!(grouped.len(), 3, "chat / work / chat top-level shape");
    assert_eq!(grouped[0].summary(), "Implemented the requested change");
    assert_eq!(grouped[2].summary(), "Please group the work");
    let HistoryNode::WorkGroup { member_nodes, .. } = &grouped[1] else {
        panic!("middle row must be a work group");
    };
    assert_eq!(member_nodes.len(), 2);
    assert_eq!(grouped[1].activity_kind(), ActivityKind::Work);
    assert_eq!(grouped[1].summary(), "1 edit · 1 tool call");

    let empty_links = BTreeMap::new();
    let empty_notes = HashMap::new();
    assert_eq!(
        grouped[1].parent_keys(&empty_links, &empty_notes),
        vec![grouped[2].node_key()],
        "the group exposes only its oldest member's external parent"
    );
    assert_eq!(
        grouped[0].parent_keys(&empty_links, &empty_notes),
        vec![grouped[1].node_key()],
        "the newer chat points at the one contracted group node"
    );
}

#[test]
fn session_start_rows_remain_metadata_instead_of_becoming_a_work_group() {
    let mut session_meta = import_op(45, 1, None, None);
    session_meta.tags |= Tags::META;
    if let OpKind::Import(import) = &mut session_meta.kind {
        import.raw_ref = Payload::Inline(br#"{"type":"session_meta","payload":{}}"#.to_vec());
    }
    let mut task_started = import_op(45, 2, Some(session_meta.id), None);
    if let OpKind::Import(import) = &mut task_started.kind {
        import.raw_ref =
            Payload::Inline(br#"{"type":"event_msg","payload":{"type":"task_started"}}"#.to_vec());
    }
    let mut session_title = import_op(46, 3, Some(session_meta.id), None);
    session_title.tags |= Tags::META;
    if let OpKind::Import(import) = &mut session_title.kind {
        import.raw_ref = Payload::Inline(
            br#"{"type":"session_title","provider":"codex","title":"r8"}"#.to_vec(),
        );
    }

    let projection = HistoryProjection::from_ops(vec![
        session_meta.clone(),
        task_started.clone(),
        session_title.clone(),
    ]);
    let nodes = projection.activity_nodes();
    let structural = projection.structural_row_keys(&nodes);
    let grouped = bundle_activity_work_groups(nodes, &structural);

    assert_eq!(grouped.len(), 2);
    assert!(grouped
        .iter()
        .all(|node| !matches!(node, HistoryNode::WorkGroup { .. })));
    assert!(grouped
        .iter()
        .any(|node| node.node_key() == task_started.id.to_string()));
    let metadata_row = grouped
        .iter()
        .find(|node| node.node_key() == session_meta.id.to_string())
        .expect("session metadata row");
    assert_eq!(
        metadata_row
            .sub_ops()
            .iter()
            .map(|op| op.id)
            .collect::<Vec<_>>(),
        vec![session_title.id],
        "the title remains folded into the standalone session boundary"
    );
}

#[test]
fn work_group_flattens_a_single_existing_bundle_into_direct_members() {
    let request_op = import_op(51, 1, None, None);
    let plan_old_op = import_op(52, 2, Some(request_op.id), None);
    let plan_new_op = import_op(53, 3, Some(plan_old_op.id), None);
    let response_op = import_op(54, 4, Some(plan_new_op.id), None);
    let rows = vec![
        manual_collapsed(
            response_op,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(8)),
            "Done",
            "message",
        ),
        manual_collapsed(
            plan_new_op,
            ActivityKind::Plan,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(8)),
            "Implementation plan",
            "reflection",
        ),
        manual_collapsed(
            plan_old_op,
            ActivityKind::Plan,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(8)),
            "Implementation plan",
            "reflection",
        ),
        manual_collapsed(
            request_op,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(8)),
            "Plan this",
            "message",
        ),
    ];
    let inner = bundle_activity_plan_repeats(rows, &HashSet::new());
    let grouped = bundle_activity_work_groups(inner, &HashSet::new());
    let HistoryNode::WorkGroup { member_nodes, .. } = &grouped[1] else {
        panic!("expected outer work group");
    };
    assert_eq!(
        member_nodes.len(),
        2,
        "the original plans are direct members"
    );
    assert!(member_nodes
        .iter()
        .all(|member| !matches!(member, HistoryNode::PlanBundle { .. })));
    assert_eq!(
        member_nodes
            .iter()
            .map(HistoryNode::summary)
            .collect::<Vec<_>>(),
        vec!["Implementation plan", "Implementation plan"]
    );
    assert_eq!(grouped[1].summary(), "2 planning steps");
}

#[test]
fn causal_branch_endpoints_always_remain_outside_work_groups() {
    let root_op = import_op(61, 1, None, None);
    let branch_point_op = import_op(62, 2, Some(root_op.id), Some("completed"));
    let left_op = import_op(63, 3, Some(branch_point_op.id), Some("completed"));
    let right_op = import_op(64, 4, Some(branch_point_op.id), Some("completed"));
    let rows = vec![
        manual_collapsed(
            left_op,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            Some(TurnId(9)),
            "left branch",
            "tool",
        ),
        manual_collapsed(
            right_op,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            Some(TurnId(9)),
            "right branch",
            "tool",
        ),
        manual_collapsed(
            branch_point_op,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            Some(TurnId(9)),
            "branch point",
            "tool",
        ),
        manual_collapsed(
            root_op,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(9)),
            "start",
            "message",
        ),
    ];
    let grouped = bundle_activity_work_groups(rows, &HashSet::new());
    assert!(
        grouped
            .iter()
            .all(|node| !matches!(node, HistoryNode::WorkGroup { .. })),
        "both fork children and their branch point are hard group boundaries"
    );
}

#[test]
fn adjacent_repeated_plan_headings_form_one_expandable_linear_bundle() {
    let root = import_op(20, 1, None, None);
    let plan_oldest = import_op(20, 2, Some(root.id), None);
    let plan_middle = import_op(20, 3, Some(plan_oldest.id), None);
    let plan_newest = import_op(20, 4, Some(plan_middle.id), None);
    let continuation = import_op(20, 5, Some(plan_newest.id), None);
    let nodes = vec![
        manual_collapsed(
            continuation.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "continuing",
            "message",
        ),
        manual_collapsed(
            plan_newest.clone(),
            ActivityKind::Plan,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "**Planning build and dry-run import steps**",
            "reflection",
        ),
        manual_collapsed(
            plan_middle.clone(),
            ActivityKind::Plan,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "Planning   build and dry-run import steps",
            "reflection",
        ),
        manual_collapsed(
            plan_oldest.clone(),
            ActivityKind::Plan,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "__Planning build and dry-run import steps__",
            "reflection",
        ),
        manual_collapsed(
            root.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let grouped = bundle_activity_plan_repeats(nodes, &HashSet::new());
    assert_eq!(grouped.len(), 3, "three Plan rows contract to one slot");
    let HistoryNode::PlanBundle {
        anchor,
        member_nodes,
        members,
        summary,
        ..
    } = &grouped[1]
    else {
        panic!("expected a PlanBundle in the repeated-heading slot");
    };
    assert_eq!(anchor.id, plan_newest.id);
    assert_eq!(summary, "**Planning build and dry-run import steps**");
    assert_eq!(member_nodes.len(), 3);
    assert_eq!(members.len(), 3);
    assert_eq!(grouped[1].record_role(), RecordRole::Narrative);
    assert_eq!(grouped[1].activity_kind(), ActivityKind::Plan);
    assert_eq!(grouped[1].outcome(), Outcome::Unknown);
    assert_eq!(
        members.iter().map(|op| op.id).collect::<Vec<_>>(),
        vec![plan_newest.id, plan_middle.id, plan_oldest.id],
        "every real reasoning operation remains expandable in display order"
    );
    assert_eq!(
        grouped[0].parent_keys(&BTreeMap::new(), &HashMap::new()),
        vec![grouped[1].node_key()],
        "the newer continuation still points at the bundle anchor"
    );
    assert_eq!(
        grouped[1].parent_keys(&BTreeMap::new(), &HashMap::new()),
        vec![root.id.to_string()],
        "the bundle inherits only the run's external parent"
    );
}

#[test]
fn plan_repeat_grouping_never_crosses_content_group_or_structural_boundaries() {
    let first = import_op(21, 1, None, None);
    let second = import_op(21, 2, Some(first.id), None);
    let separator = import_op(21, 3, Some(second.id), None);
    let third = import_op(21, 4, Some(separator.id), None);
    let plan = |op, summary: &str| {
        manual_collapsed(
            op,
            ActivityKind::Plan,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            summary,
            "reflection",
        )
    };
    let nodes = vec![
        plan(third, "Same heading"),
        manual_collapsed(
            separator,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool",
            "tool",
        ),
        plan(second.clone(), "Same heading"),
        plan(first, "Different heading"),
    ];
    let grouped = bundle_activity_plan_repeats(nodes, &HashSet::new());
    assert!(
        grouped
            .iter()
            .all(|node| !matches!(node, HistoryNode::PlanBundle { .. })),
        "an intervening row and a different heading both break a run"
    );

    let structural_pair = vec![
        plan(import_op(22, 2, None, None), "Structural heading"),
        plan(import_op(22, 1, None, None), "Structural heading"),
    ];
    let structural = HashSet::from([structural_pair[0].key()]);
    let preserved = bundle_activity_plan_repeats(structural_pair, &structural);
    assert!(
        preserved
            .iter()
            .all(|node| !matches!(node, HistoryNode::PlanBundle { .. })),
        "a structural endpoint remains an independent graph row"
    );

    let mut other_session = import_op(23, 1, None, None);
    other_session.scope = ScopeRef::Session(SessionId(11));
    let cross_session = vec![
        plan(import_op(23, 2, None, None), "Session heading"),
        plan(other_session, "Session heading"),
    ];
    let preserved = bundle_activity_plan_repeats(cross_session, &HashSet::new());
    assert!(
        preserved
            .iter()
            .all(|node| !matches!(node, HistoryNode::PlanBundle { .. })),
        "identical headings in different sessions never group"
    );
}

#[test]
fn bundles_maximal_run_of_three_tool_rows_into_one_expandable_node() {
    // user request -> tool ok -> tool ok -> tool ok -> agent answer (one turn).
    let ops = linear_chain(
        1,
        "user request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "tool", Some("completed"), 1),
            row_spec(5, 5, "message", None, 1),
        ],
    );
    let (nodes, _) = activity_view(ops);
    assert_eq!(nodes.len(), 5);
    let bundled = bundle(nodes);
    assert_eq!(bundled.len(), 3);
    assert_eq!(bundled[0].activity_kind(), ActivityKind::Conversation);
    assert_eq!(bundled[2].activity_kind(), ActivityKind::Conversation);
    let bundle_node = unwrap_bundle(&bundled);
    assert_eq!(bundle_node.activity_kind(), ActivityKind::Execute);
    assert_eq!(bundle_node.visibility(), Visibility::Primary);
    assert_eq!(bundle_node.record_role(), RecordRole::Action);
    assert_eq!(bundle_node.outcome(), Outcome::Success);
    let HistoryNode::ExecuteBundle {
        member_nodes,
        members,
        anchor,
        summary,
        ..
    } = bundle_node
    else {
        panic!("expected ExecuteBundle");
    };
    // All members structured-successful -> success label; every original row stays
    // retrievable through the existing sub-ops model, newest-first.
    assert_eq!(summary, "3 tool steps (success)");
    assert_eq!(members.len(), 3);
    assert_eq!(member_nodes.len(), 3);
    assert_eq!(bundle_node.sub_ops().len(), 3);
    assert_eq!(bundle_node.node_key(), anchor.id.to_string());
    let member_keys: Vec<String> = bundle_node
        .sub_ops()
        .iter()
        .map(|op| op.id.to_string())
        .collect();
    assert_eq!(member_keys, vec!["4:0:40", "3:0:30", "2:0:20"]);
    assert_eq!(member_keys[0], bundle_node.node_key());
}

#[test]
fn bundles_parallel_claude_tool_blocks_from_one_exact_response() {
    // Claude persists parallel tool_use content blocks as separate UUID events
    // sharing one Anthropic message id. Its provider graph leaves one call as a
    // short sibling branch once results are folded. Activity contracts those
    // adjacent blocks into one response-level work row; Raw remains untouched.
    let root = import_op(30, 1, None, None);
    let first = claude_assistant_import(30, 2, root.id, "msg-response-1");
    let second = claude_assistant_import(30, 3, first.id, "msg-response-1");
    let mut continuation = import_op(30, 4, Some(first.id), None);
    continuation.scope = root.scope;
    let nodes = vec![
        manual_collapsed(
            continuation.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "answer",
            "message",
        ),
        manual_collapsed(
            second,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read two",
            "tool",
        ),
        manual_collapsed(
            first,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read one",
            "tool",
        ),
        manual_collapsed(
            root.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];
    let annotations = annotate_activity_rows(&nodes);
    let bundled = bundle_activity_execute_runs(nodes, &annotations, &HashSet::new());

    assert_eq!(bundled.len(), 3);
    let bundle = unwrap_bundle(&bundled);
    assert_eq!(bundle.sub_ops().len(), 2);
    let empty_links = BTreeMap::new();
    let empty_notes = HashMap::new();
    assert_eq!(
        bundle.parent_keys(&empty_links, &empty_notes),
        vec![root.id.to_string()]
    );
    let HistoryNode::CollapsedImport { op, .. } = &bundled[0] else {
        panic!("expected continuation row");
    };
    assert_eq!(
        bundled[0].parent_keys(&empty_links, &empty_notes),
        vec![bundle.node_key()],
        "the response continuation follows the bundle"
    );
    assert_eq!(op.as_ref(), &continuation, "source envelope is retained");
}

#[test]
fn different_claude_response_ids_do_not_bundle_without_a_turn() {
    let root = import_op(31, 1, None, None);
    let first = claude_assistant_import(31, 2, root.id, "msg-response-1");
    let second = claude_assistant_import(31, 3, first.id, "msg-response-2");
    let nodes = vec![
        manual_collapsed(
            second,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read two",
            "tool",
        ),
        manual_collapsed(
            first,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read one",
            "tool",
        ),
        manual_collapsed(
            root,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];
    let annotations = annotate_activity_rows(&nodes);
    let bundled = bundle_activity_execute_runs(nodes, &annotations, &HashSet::new());

    assert_eq!(bundled.len(), 3);
    assert!(bundled
        .iter()
        .all(|node| !matches!(node, HistoryNode::ExecuteBundle { .. })));
}

#[test]
fn exact_claude_tool_fragment_folds_across_an_unrelated_interleaved_row() {
    let root = import_op(32, 1, None, None);
    let first = claude_assistant_import(32, 2, root.id, "msg-response-1");
    let second = claude_assistant_import(32, 3, first.id, "msg-response-1");
    let mut continuation = import_op(32, 4, Some(first.id), None);
    continuation.scope = root.scope;
    let mut unrelated = import_op(99, 3, None, None);
    unrelated.scope = ScopeRef::Session(SessionId(99));
    let nodes = vec![
        manual_collapsed(
            continuation.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "answer",
            "message",
        ),
        manual_collapsed(
            second.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read two",
            "tool",
        ),
        manual_collapsed(
            unrelated,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "other session",
            "message",
        ),
        manual_collapsed(
            first.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read one",
            "tool",
        ),
        manual_collapsed(
            root,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let annotations = annotate_activity_rows(&nodes);
    let run_bundled = bundle_activity_execute_runs(nodes, &annotations, &HashSet::new());
    assert!(run_bundled
        .iter()
        .all(|node| !matches!(node, HistoryNode::ExecuteBundle { .. })));
    let bundled = bundle_claude_response_tool_fragments(run_bundled, &HashSet::new());

    assert_eq!(bundled.len(), 4);
    assert!(bundled
        .iter()
        .all(|node| node.node_key() != second.id.to_string()));
    let response = bundled
        .iter()
        .find(|node| node.node_key() == first.id.to_string())
        .unwrap_or_else(|| panic!("response parent missing"));
    assert!(response.sub_ops().iter().any(|op| op.id == second.id));
    let kept_continuation = bundled
        .iter()
        .find(|node| node.node_key() == continuation.id.to_string())
        .unwrap_or_else(|| panic!("continuation missing"));
    let HistoryNode::CollapsedImport {
        op: continuation_op,
        ..
    } = kept_continuation
    else {
        panic!("expected collapsed continuation");
    };
    assert_eq!(
        continuation_op
            .parents
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec![first.id.to_string()]
    );
}

#[test]
fn connected_claude_tool_chain_folds_without_touching_its_result_path() {
    // Real Claude shape: one response emits a chain of tool-use content blocks,
    // while the later batched result points to the response's first block. An
    // unrelated session can be interleaved between those response records.
    let root = import_op(37, 1, None, None);
    let first = claude_assistant_import(37, 2, root.id, "msg-response-1");
    let second = claude_assistant_import(37, 3, first.id, "msg-response-1");
    let third = claude_assistant_import(37, 4, second.id, "msg-response-1");
    let result = import_op(37, 5, Some(first.id), None);
    let continuation = import_op(37, 6, Some(result.id), None);
    let mut unrelated = import_op(97, 3, None, None);
    unrelated.scope = ScopeRef::Session(SessionId(97));
    let nodes = vec![
        manual_collapsed(
            continuation,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "continued after results",
            "message",
        ),
        manual_collapsed(
            result.clone(),
            ActivityKind::Execute,
            RecordRole::Result,
            Outcome::Success,
            None,
            "three results",
            "tool",
        ),
        manual_collapsed(
            third,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read three",
            "tool",
        ),
        manual_collapsed(
            unrelated,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "other session",
            "message",
        ),
        manual_collapsed(
            second,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read two",
            "tool",
        ),
        manual_collapsed(
            first.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read one",
            "tool",
        ),
        manual_collapsed(
            root.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let bundled = bundle_claude_response_tool_fragments(nodes, &HashSet::new());

    assert_eq!(bundled.len(), 5);
    let response = unwrap_bundle(&bundled);
    assert_eq!(response.node_key(), first.id.to_string());
    let HistoryNode::ExecuteBundle { member_nodes, .. } = response else {
        panic!("expected exact-response execute bundle");
    };
    assert_eq!(member_nodes.len(), 3);
    assert_eq!(
        response.parent_keys(&BTreeMap::new(), &HashMap::new()),
        vec![root.id.to_string()]
    );
    let result_row = bundled
        .iter()
        .find(|node| node.node_key() == result.id.to_string())
        .unwrap_or_else(|| panic!("result path missing"));
    assert_eq!(
        result_row.parent_keys(&BTreeMap::new(), &HashMap::new()),
        vec![response.node_key()],
        "the result path must continue through the one response bundle"
    );
}

#[test]
fn connected_claude_tool_chain_rewrites_a_non_root_continuation() {
    let root = import_op(39, 1, None, None);
    let first = claude_assistant_import(39, 2, root.id, "msg-response-1");
    let second = claude_assistant_import(39, 3, first.id, "msg-response-1");
    let continuation = import_op(39, 4, Some(second.id), None);
    let nodes = vec![
        manual_collapsed(
            continuation.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "continued",
            "message",
        ),
        manual_collapsed(
            second,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read two",
            "tool",
        ),
        manual_collapsed(
            first.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read one",
            "tool",
        ),
        manual_collapsed(
            root,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let bundled = bundle_claude_response_tool_fragments(nodes, &HashSet::new());

    assert_eq!(bundled.len(), 3);
    let response = unwrap_bundle(&bundled);
    assert_eq!(response.node_key(), first.id.to_string());
    let continuation_row = bundled
        .iter()
        .find(|node| node.node_key() == continuation.id.to_string())
        .unwrap_or_else(|| panic!("continuation missing"));
    assert_eq!(
        continuation_row.parent_keys(&BTreeMap::new(), &HashMap::new()),
        vec![response.node_key()],
        "the derived-view override must keep a non-root continuation connected"
    );
}

#[test]
fn disconnected_claude_rows_with_the_same_message_id_stay_separate() {
    let root = import_op(38, 1, None, None);
    let first = claude_assistant_import(38, 2, root.id, "msg-reused");
    let second = claude_assistant_import(38, 3, root.id, "msg-reused");
    let nodes = vec![
        manual_collapsed(
            second.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read two",
            "tool",
        ),
        manual_collapsed(
            first.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read one",
            "tool",
        ),
        manual_collapsed(
            root,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let bundled = bundle_claude_response_tool_fragments(nodes, &HashSet::new());

    assert_eq!(bundled.len(), 3);
    assert!(bundled
        .iter()
        .any(|node| node.node_key() == first.id.to_string()));
    assert!(bundled
        .iter()
        .any(|node| node.node_key() == second.id.to_string()));
}

#[test]
fn terminal_claude_execute_bundle_merges_into_its_continued_response_parent() {
    let root = import_op(36, 1, None, None);
    let first = claude_assistant_import(36, 2, root.id, "msg-response-1");
    let second = claude_assistant_import(36, 3, first.id, "msg-response-1");
    let third = claude_assistant_import(36, 4, second.id, "msg-response-1");
    let continuation = import_op(36, 5, Some(first.id), None);
    let mut unrelated = import_op(98, 3, None, None);
    unrelated.scope = ScopeRef::Session(SessionId(98));
    let nodes = vec![
        manual_collapsed(
            continuation.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "answer",
            "message",
        ),
        manual_collapsed(
            third,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read three",
            "tool",
        ),
        manual_collapsed(
            second,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read two",
            "tool",
        ),
        manual_collapsed(
            unrelated,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "other session",
            "message",
        ),
        manual_collapsed(
            first.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read one",
            "tool",
        ),
        manual_collapsed(
            root,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let annotations = annotate_activity_rows(&nodes);
    let run_bundled = bundle_activity_execute_runs(nodes, &annotations, &HashSet::new());
    let bundled = bundle_claude_response_tool_fragments(run_bundled, &HashSet::new());

    assert_eq!(bundled.len(), 4);
    let response = unwrap_bundle(&bundled);
    assert_eq!(response.node_key(), first.id.to_string());
    let HistoryNode::ExecuteBundle { member_nodes, .. } = response else {
        panic!("expected merged execute bundle");
    };
    assert_eq!(member_nodes.len(), 3);
    let kept_continuation = bundled
        .iter()
        .find(|node| node.node_key() == continuation.id.to_string())
        .unwrap_or_else(|| panic!("continuation missing"));
    let HistoryNode::CollapsedImport {
        op: continuation_op,
        ..
    } = kept_continuation
    else {
        panic!("expected collapsed continuation");
    };
    assert_eq!(continuation_op.parents, ParentSet::One(first.id));
}

#[test]
fn exact_claude_tool_fragment_folds_into_same_response_narrative() {
    let root = import_op(33, 1, None, None);
    let narrative = claude_assistant_import(33, 2, root.id, "msg-response-1");
    let tool = claude_assistant_import(33, 3, narrative.id, "msg-response-1");
    let continuation = import_op(33, 4, Some(narrative.id), None);
    let nodes = vec![
        manual_collapsed(
            continuation,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "continued",
            "message",
        ),
        manual_collapsed(
            tool.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Bash",
            "tool",
        ),
        manual_collapsed(
            narrative.clone(),
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "I will inspect it.",
            "message",
        ),
        manual_collapsed(
            root,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let bundled = bundle_claude_response_tool_fragments(nodes, &HashSet::new());

    assert_eq!(bundled.len(), 3);
    let response = bundled
        .iter()
        .find(|node| node.node_key() == narrative.id.to_string())
        .unwrap_or_else(|| panic!("narrative response missing"));
    assert_eq!(response.activity_kind(), ActivityKind::Conversation);
    assert!(response.sub_ops().iter().any(|op| op.id == tool.id));
}

#[test]
fn claude_response_fragment_with_structural_topology_stays_visible() {
    let root = import_op(34, 1, None, None);
    let parent = claude_assistant_import(34, 2, root.id, "msg-response-1");
    let tool = claude_assistant_import(34, 3, parent.id, "msg-response-1");
    let continuation = import_op(34, 4, Some(parent.id), None);
    let nodes = vec![
        manual_collapsed(
            continuation,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "continued",
            "message",
        ),
        manual_collapsed(
            tool.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Task",
            "tool",
        ),
        manual_collapsed(
            parent,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "delegating",
            "message",
        ),
        manual_collapsed(
            root,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let bundled = bundle_claude_response_tool_fragments(
        nodes,
        &HashSet::from([editchain_project::NodeKey::Op(tool.id)]),
    );

    assert_eq!(bundled.len(), 4);
    assert!(bundled
        .iter()
        .any(|node| node.node_key() == tool.id.to_string()));
}

#[test]
fn mainline_single_claude_tool_block_stays_visible() {
    let root = import_op(35, 1, None, None);
    let parent = claude_assistant_import(35, 2, root.id, "msg-response-1");
    let tool = claude_assistant_import(35, 3, parent.id, "msg-response-1");
    let continuation = import_op(35, 4, Some(tool.id), None);
    let nodes = vec![
        manual_collapsed(
            continuation,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "continued through tool",
            "message",
        ),
        manual_collapsed(
            tool.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            None,
            "tool: Read",
            "tool",
        ),
        manual_collapsed(
            parent,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "reading",
            "message",
        ),
        manual_collapsed(
            root,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            None,
            "request",
            "message",
        ),
    ];

    let bundled = bundle_claude_response_tool_fragments(nodes, &HashSet::new());

    assert_eq!(bundled.len(), 4);
    assert!(bundled
        .iter()
        .any(|node| node.node_key() == tool.id.to_string()));
}

#[test]
fn singletons_never_bundle_and_messages_split_runs() {
    // tool -> message -> tool -> message -> tool (same turn): every execute row
    // is a display singleton, so nothing folds and the profile stays flat.
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "message", None, 1),
            row_spec(4, 4, "tool", Some("completed"), 1),
            row_spec(5, 5, "message", None, 1),
            row_spec(6, 6, "tool", Some("completed"), 1),
        ],
    );
    let (nodes, _) = activity_view(ops);
    assert_eq!(nodes.len(), 6);
    let bundled = bundle(nodes);
    assert!(
        bundled
            .iter()
            .all(|node| !matches!(node, HistoryNode::ExecuteBundle { .. })),
        "singleton execute rows never bundle"
    );
    assert_eq!(bundled.len(), 6);
}

#[test]
fn bundles_two_adjacent_runs_without_gathering_across_messages() {
    // tool, tool, message, tool, tool (same turn, contiguous display runs of
    // two on each side of the message).
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "message", None, 1),
            row_spec(5, 5, "tool", Some("completed"), 1),
            row_spec(6, 6, "tool", Some("completed"), 1),
        ],
    );
    let (nodes, _) = activity_view(ops);
    assert_eq!(nodes.len(), 6);
    let bundled = bundle(nodes);
    assert_eq!(bundled.len(), 4);
    let first_bundle = unwrap_bundle(&bundled);
    assert_eq!(first_bundle.sub_ops().len(), 2);
}

#[test]
fn never_bundles_across_turn_boundaries() {
    // Two turns in one session chain: turn 2 rows render above turn 1 rows, so
    // each turn forms its own maximal run and no bundle mixes members from
    // different turns.
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "message", None, 1),
            row_spec(5, 5, "message", None, 2),
            row_spec(6, 6, "tool", Some("completed"), 2),
            row_spec(7, 7, "tool", Some("completed"), 2),
            row_spec(8, 8, "message", None, 2),
        ],
    );
    let (nodes, _) = activity_view(ops);
    let bundled = bundle(nodes);
    let bundle_nodes: Vec<&HistoryNode> = bundled
        .iter()
        .filter(|node| matches!(node, HistoryNode::ExecuteBundle { .. }))
        .collect();
    assert_eq!(bundle_nodes.len(), 2);
    // display order newest-first: turn 2's run renders first, turn 1's second.
    let turn_ids: Vec<u64> = bundle_nodes
        .iter()
        .map(|node| node.turn_id().map_or(0, |turn_id| turn_id.0))
        .collect();
    assert_eq!(turn_ids, vec![2, 1]);
}

#[test]
fn interleaved_turn_rows_are_not_gathered_across_display_rows() {
    // Two independent same-session chains with interleaved clocks: display order
    // alternates turn A / turn B rows, so same-turn execute rows are never
    // adjacent and nothing bundles.
    let a_root = import_op(1, 1, None, None);
    let b_root = import_op(3, 3, None, None);
    let a_tool = import_op(2, 2, Some(a_root.id), Some("completed"));
    let b_tool = import_op(4, 4, Some(b_root.id), Some("completed"));
    let ops = vec![
        a_root.clone(),
        message_child(101, 1010, a_root.id, "a request", 1),
        a_tool.clone(),
        tool_child(201, 1020, a_tool.id, "Bash", 1),
        b_root.clone(),
        message_child(301, 1030, b_root.id, "b request", 2),
        b_tool.clone(),
        tool_child(401, 1040, b_tool.id, "Bash", 2),
    ];
    let (nodes, _) = activity_view(ops);
    assert_eq!(nodes.len(), 4);
    let bundled = bundle(nodes);
    assert!(
        bundled
            .iter()
            .all(|node| !matches!(node, HistoryNode::ExecuteBundle { .. })),
        "non-contiguous same-turn rows are never gathered"
    );
    assert_eq!(bundled.len(), 4);
    let kinds: Vec<ActivityKind> = bundled.iter().map(HistoryNode::activity_kind).collect();
    assert_eq!(
        kinds,
        vec![
            ActivityKind::Execute,
            ActivityKind::Conversation,
            ActivityKind::Execute,
            ActivityKind::Conversation,
        ]
    );
}

#[test]
fn failure_rows_stay_visible_and_split_runs_into_clean_sides() {
    // tool ok, tool ok, tool error, tool ok, tool ok (same turn, contiguous).
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "tool", Some("error"), 1),
            row_spec(5, 5, "tool", Some("completed"), 1),
            row_spec(6, 6, "tool", Some("completed"), 1),
        ],
    );
    let (nodes, _) = activity_view(ops);
    assert_eq!(nodes.len(), 6);
    let bundled = bundle(nodes);
    // The failure row is never folded; the two clean pairs either side bundle.
    assert_eq!(bundled.len(), 4);
    let failure = bundled
        .iter()
        .find(|node| node.outcome() == Outcome::Failure)
        .unwrap_or_else(|| panic!("failure row must remain visible"));
    assert_eq!(failure.node_key(), "4:0:40");
    let clean_bundles: Vec<&HistoryNode> = bundled
        .iter()
        .filter(|node| matches!(node, HistoryNode::ExecuteBundle { .. }))
        .collect();
    assert_eq!(clean_bundles.len(), 2);
    for node in clean_bundles {
        assert_eq!(node.sub_ops().len(), 2);
    }
}

#[test]
fn unknown_outcome_runs_are_eligible_and_labeled_neutrally() {
    // No structured status at all: Outcome::Unknown rows are still eligible
    // ("clean/no negative evidence"), and the bundle summary never claims
    // success.
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", None, 1),
            row_spec(3, 3, "tool", None, 1),
            row_spec(4, 4, "tool", None, 1),
        ],
    );
    let (nodes, _) = activity_view(ops);
    let bundled = bundle(nodes);
    let bundle_node = unwrap_bundle(&bundled);
    let HistoryNode::ExecuteBundle { summary, .. } = bundle_node else {
        panic!("expected ExecuteBundle");
    };
    assert_eq!(summary, "3 tool steps");
    assert_eq!(bundle_node.outcome(), Outcome::Unknown);

    // Mixed structured success + unknown: still not labeled success.
    let mixed = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", None, 1),
            row_spec(4, 4, "tool", Some("completed"), 1),
        ],
    );
    let (mixed_nodes, _) = activity_view(mixed);
    let mixed_bundled = bundle(mixed_nodes);
    let mixed_bundle = unwrap_bundle(&mixed_bundled);
    let HistoryNode::ExecuteBundle { summary, .. } = mixed_bundle else {
        panic!("expected ExecuteBundle");
    };
    assert_eq!(summary, "3 tool steps");
    assert_eq!(mixed_bundle.outcome(), Outcome::Unknown);
}

#[test]
fn change_adjacency_blocks_bundling() {
    // tool, tool, change, tool, tool (same turn): both runs sit immediately
    // adjacent to the change row, so neither bundles.
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "file", None, 1),
            row_spec(5, 5, "tool", Some("completed"), 1),
            row_spec(6, 6, "tool", Some("completed"), 1),
        ],
    );
    let (nodes, _) = activity_view(ops);
    assert_eq!(nodes.len(), 6);
    let bundled = bundle(nodes);
    assert_eq!(bundled.len(), 6);
    let change_row = bundled
        .iter()
        .find(|node| node.activity_kind() == ActivityKind::Change)
        .unwrap_or_else(|| panic!("change row must remain visible"));
    assert_eq!(change_row.node_key(), "4:0:40");
    assert!(
        bundled
            .iter()
            .all(|node| !matches!(node, HistoryNode::ExecuteBundle { .. })),
        "runs adjacent to a change row never bundle"
    );
}

#[test]
fn change_separated_by_messages_does_not_block_bundling() {
    // tool, tool, message, change, message, tool, tool (same turn): change rows
    // never fold, and runs not adjacent to them bundle normally.
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "message", None, 1),
            row_spec(5, 5, "file", None, 1),
            row_spec(6, 6, "message", None, 1),
            row_spec(7, 7, "tool", Some("completed"), 1),
            row_spec(8, 8, "tool", Some("completed"), 1),
        ],
    );
    let (nodes, _) = activity_view(ops);
    let bundled = bundle(nodes);
    assert_eq!(bundled.len(), 6);
    assert_eq!(
        bundled
            .iter()
            .filter(|node| matches!(node, HistoryNode::ExecuteBundle { .. }))
            .count(),
        2
    );
    assert!(
        bundled
            .iter()
            .any(|node| node.activity_kind() == ActivityKind::Change),
        "change evidence stays visible between bundles"
    );
}

#[test]
fn world_state_subop_member_breaks_runs() {
    // tool rows with a world_state META sub-op attached to the newest member
    // must not fold that member away; the clean pair around it still bundles.
    let a_root = import_op(2, 1, None, Some("completed"));
    let a_tool = import_op(3, 2, Some(a_root.id), Some("completed"));
    let a_stateful = import_op(4, 3, Some(a_tool.id), Some("completed"));
    let meta = world_state_import_op(4, 4, a_stateful.id);
    let ops = vec![
        a_root.clone(),
        tool_child(202, 1010, a_root.id, "Bash", 1),
        a_tool.clone(),
        tool_child(302, 1020, a_tool.id, "Bash", 1),
        a_stateful.clone(),
        tool_child(402, 1030, a_stateful.id, "Bash", 1),
        meta.clone(),
    ];
    let projection = HistoryProjection::from_ops(ops);
    let nodes = projection.activity_nodes();
    assert_eq!(nodes.len(), 3);
    let bundled = bundle(nodes);
    let stateful_row = bundled
        .iter()
        .find(|node| node.node_key() == a_stateful.id.to_string())
        .unwrap_or_else(|| panic!("world_state owner must remain a top-level row"));
    assert_eq!(stateful_row.sub_ops().len(), 1);
    let clean_bundle = unwrap_bundle(&bundled);
    assert_eq!(clean_bundle.sub_ops().len(), 2);
}

#[test]
fn promotion_marks_negative_change_and_unit_final_narrative() {
    // user request, tool ok, tool error, change, agent answer (one turn).
    let ops = linear_chain(
        1,
        "user request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("error"), 1),
            row_spec(4, 4, "file", None, 1),
            row_spec(5, 5, "message", None, 1),
        ],
    );
    let (nodes, annotations) = activity_view(ops);
    // display newest-first: agent answer(0), change(1), failure tool(2), ok
    // tool(3), user request(4).
    assert_eq!(nodes.len(), 5);
    assert!(annotations[0].promoted, "unit-final narrative promoted");
    assert!(annotations[1].promoted, "change row promoted");
    assert!(annotations[2].promoted, "failure row promoted");
    assert!(
        !annotations[3].promoted,
        "clean tool run member not promoted"
    );
    assert!(
        !annotations[4].promoted,
        "unit-opening request not promoted"
    );
    assert_eq!(annotations[0].work_unit.count, 5);
    assert!(annotations[0].work_unit.is_start);
    assert!(annotations[4].work_unit.is_end);
    assert_eq!(
        annotations[4].work_unit.title.as_deref(),
        Some("user request"),
        "unit title is the oldest primary narrative"
    );
}

#[test]
fn bundle_inherits_the_runs_external_parent_edges() {
    // user request -> tool -> tool -> tool -> agent answer: the bundle replaces
    // the run's display slots, so its parent_keys must carry the run's incoming
    // edge (the user request), not the intra-run member edges, and the kept
    // agent answer must keep pointing at the bundle key.
    let ops = linear_chain(
        1,
        "user request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "tool", Some("completed"), 1),
            row_spec(5, 5, "message", None, 1),
        ],
    );
    let (nodes, _) = activity_view(ops);
    let bundled = bundle(nodes);
    assert_eq!(bundled.len(), 3);
    let bundle_node = unwrap_bundle(&bundled);
    let empty_links = BTreeMap::new();
    let empty_notes = HashMap::new();
    let external_parents = bundle_node.parent_keys(&empty_links, &empty_notes);
    assert_eq!(
        external_parents,
        vec!["1:0:1"],
        "run inherits the incoming edge"
    );
    let kept_answer = &bundled[0];
    assert_eq!(kept_answer.node_key(), "5:0:50");
    let HistoryNode::CollapsedImport { op, .. } = kept_answer else {
        panic!("expected kept agent answer row");
    };
    let parents: Vec<String> = op.parents.iter().map(ToString::to_string).collect();
    assert_eq!(
        parents,
        vec![bundle_node.node_key()],
        "kept child resolves to the bundle key"
    );
}

#[test]
fn promotion_prevents_bundling() {
    // A three-tool run where the newest row is promoted must keep that row
    // top-level while the remaining contiguous pair still bundles.
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "tool", Some("completed"), 1),
        ],
    );
    let (nodes, mut annotations) = activity_view(ops);
    assert_eq!(nodes.len(), 4);
    // display newest-first: tool@seq4(0), tool@seq3, tool@seq2, user request.
    annotations[0].promoted = true;
    let bundled = bundle_activity_execute_runs(nodes, &annotations, &HashSet::new());
    assert_eq!(bundled.len(), 3);
    let promoted_row = &bundled[0];
    assert_eq!(promoted_row.node_key(), "4:0:40");
    let clean_bundle = unwrap_bundle(&bundled);
    assert_eq!(clean_bundle.sub_ops().len(), 2);
}

#[test]
fn work_unit_markers_are_stable_across_turns_and_paging() {
    // Two turns in one session chain; annotations are computed over the FULL
    // view so boundaries/counts never depend on window size or offset.
    let ops = linear_chain(
        1,
        "request",
        &[
            row_spec(2, 2, "tool", Some("completed"), 1),
            row_spec(3, 3, "tool", Some("completed"), 1),
            row_spec(4, 4, "message", None, 1),
            row_spec(5, 5, "message", None, 2),
            row_spec(6, 6, "tool", Some("completed"), 2),
            row_spec(7, 7, "message", None, 2),
        ],
    );
    let (_nodes, annotations) = activity_view(ops);
    assert_eq!(annotations.len(), 7);
    let turn_two_id = "session:10/turn:2".to_string();
    let turn_one_id = "session:10/turn:1".to_string();
    for annotation in annotations.iter().take(3) {
        assert_eq!(annotation.work_unit.id, turn_two_id);
        assert_eq!(annotation.work_unit.count, 3);
    }
    assert!(annotations[0].work_unit.is_start);
    assert!(!annotations[1].work_unit.is_start);
    assert!(annotations[2].work_unit.is_end);
    for annotation in annotations.iter().skip(3) {
        assert_eq!(annotation.work_unit.id, turn_one_id);
        assert_eq!(annotation.work_unit.count, 4);
    }
    assert!(annotations[3].work_unit.is_start);
    assert!(annotations[6].work_unit.is_end);
    assert_eq!(
        annotations[0]
            .session_summary
            .as_ref()
            .map(|summary| summary.count),
        Some(7),
        "the newest row summarizes the complete session across both turns"
    );
    assert!(
        annotations
            .iter()
            .skip(1)
            .all(|annotation| annotation.session_summary.is_none()),
        "one session group has exactly one summary marker"
    );
}

#[test]
fn session_summary_does_not_follow_an_interior_session_scoped_work_unit() {
    // Codex mixes turn-scoped chat/work rows with session-scoped lifecycle
    // rows. The latter form their own work unit, but must not become the
    // session summary when a newer turn row exists.
    let newest = manual_collapsed(
        import_op(31, 3, None, None),
        ActivityKind::Conversation,
        RecordRole::Narrative,
        Outcome::Unknown,
        Some(TurnId(2)),
        "final response",
        "message",
    );
    let lifecycle = manual_collapsed(
        import_op(32, 2, None, None),
        ActivityKind::System,
        RecordRole::Lifecycle,
        Outcome::Unknown,
        None,
        "task_complete",
        "import",
    );
    let oldest = manual_collapsed(
        import_op(33, 1, None, None),
        ActivityKind::Conversation,
        RecordRole::Narrative,
        Outcome::Unknown,
        Some(TurnId(1)),
        "initial request",
        "message",
    );

    let annotations = annotate_activity_rows(&[newest, lifecycle, oldest]);
    assert_eq!(annotations.len(), 3);
    assert_eq!(
        annotations[0]
            .session_summary
            .as_ref()
            .map(|summary| summary.count),
        Some(3)
    );
    assert_eq!(annotations[1].work_unit.id, "session:10");
    assert!(annotations[1].work_unit.is_start);
    assert!(
        annotations[1].session_summary.is_none(),
        "an interior session-scoped unit is not a whole-session boundary"
    );
    assert!(annotations[2].session_summary.is_none());
}

#[test]
fn work_unit_markers_are_view_wide_for_interleaved_units() {
    // Display order (newest-first) interleaves two units as A/B/A/B/A/B, so no
    // id is display-contiguous. Each logical unit must still yield exactly one
    // boundary pair, a full-view count, a whole-unit title (its oldest
    // narrative), and exactly one unit-final narrative promotion — never one
    // per segment — and rows must only be annotated, never reordered.
    let a3 = import_op(11, 11, None, None);
    let b3 = import_op(12, 12, None, None);
    let a2 = import_op(13, 13, Some(a3.id), Some("completed"));
    let b2 = import_op(14, 14, Some(b3.id), Some("completed"));
    let a1 = import_op(15, 15, Some(a2.id), Some("completed"));
    let b1 = import_op(16, 16, Some(b2.id), Some("completed"));
    let nodes = vec![
        manual_collapsed(
            a3,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(1)),
            "a3",
            "message",
        ),
        manual_collapsed(
            b3,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(2)),
            "b3",
            "message",
        ),
        manual_collapsed(
            a2,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            Some(TurnId(1)),
            "a2",
            "tool",
        ),
        manual_collapsed(
            b2,
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            Some(TurnId(2)),
            "b2",
            "tool",
        ),
        manual_collapsed(
            a1,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(1)),
            "a1",
            "message",
        ),
        manual_collapsed(
            b1,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(2)),
            "b1",
            "message",
        ),
    ];
    let annotations = annotate_activity_rows(&nodes);
    assert_eq!(annotations.len(), 6);
    let ids: Vec<&str> = annotations
        .iter()
        .map(|annotation| annotation.work_unit.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "session:10/turn:1",
            "session:10/turn:2",
            "session:10/turn:1",
            "session:10/turn:2",
            "session:10/turn:1",
            "session:10/turn:2",
        ],
        "rows are only annotated, never reordered or gathered"
    );
    let starts: Vec<bool> = annotations
        .iter()
        .map(|annotation| annotation.work_unit.is_start)
        .collect();
    assert_eq!(
        starts,
        vec![true, true, false, false, false, false],
        "exactly one is_start at each id's first display-order occurrence"
    );
    let ends: Vec<bool> = annotations
        .iter()
        .map(|annotation| annotation.work_unit.is_end)
        .collect();
    assert_eq!(
        ends,
        vec![false, false, false, false, true, true],
        "exactly one is_end at each id's last display-order occurrence"
    );
    for annotation in &annotations {
        assert_eq!(
            annotation.work_unit.count, 3,
            "count totals every top-level row of the id across the full view"
        );
    }
    for index in [0usize, 2, 4] {
        assert_eq!(
            annotations[index].work_unit.title.as_deref(),
            Some("a1"),
            "unit A title is its oldest narrative across the whole interleaved unit"
        );
    }
    for index in [1usize, 3, 5] {
        assert_eq!(
            annotations[index].work_unit.title.as_deref(),
            Some("b1"),
            "unit B title is its oldest narrative across the whole interleaved unit"
        );
    }
    let promoted: Vec<bool> = annotations
        .iter()
        .map(|annotation| annotation.promoted)
        .collect();
    assert_eq!(
        promoted,
        vec![true, true, false, false, false, false],
        "unit-final/newest narrative promotion happens exactly once per id"
    );
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    assert_eq!(
        keys,
        vec!["11:0:11", "12:0:12", "13:0:13", "14:0:14", "15:0:15", "16:0:16"]
    );
}

#[test]
fn bundle_parents_rewire_to_the_anchor_key() {
    // A kept row whose stored parent is a folded member must resolve to the
    // bundle key so layout stays connected after contraction.
    let tool_op_2 = import_op(2, 2, None, Some("completed"));
    let tool_op_3 = import_op(3, 3, Some(tool_op_2.id), Some("completed"));
    let tool_op_4 = import_op(4, 4, Some(tool_op_3.id), Some("completed"));
    let mut message_op_5 = import_op(5, 5, Some(tool_op_3.id), None);
    message_op_5.tags = Tags::MESSAGE;
    let nodes = vec![
        manual_collapsed(
            tool_op_4.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            Some(TurnId(1)),
            "tool 4",
            "tool",
        ),
        manual_collapsed(
            tool_op_3.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            Some(TurnId(1)),
            "tool 3",
            "tool",
        ),
        manual_collapsed(
            tool_op_2.clone(),
            ActivityKind::Execute,
            RecordRole::Action,
            Outcome::Success,
            Some(TurnId(1)),
            "tool 2",
            "tool",
        ),
        manual_collapsed(
            message_op_5,
            ActivityKind::Conversation,
            RecordRole::Narrative,
            Outcome::Unknown,
            Some(TurnId(1)),
            "text",
            "message",
        ),
    ];
    let annotations = annotate_activity_rows(&nodes);
    let structural = HashSet::new();
    let bundled = bundle_activity_execute_runs(nodes, &annotations, &structural);
    assert_eq!(bundled.len(), 2);
    let kept_message = &bundled[1];
    let HistoryNode::CollapsedImport { op, .. } = kept_message else {
        panic!("expected kept CollapsedImport message row");
    };
    assert_eq!(
        op.parents,
        ParentSet::One(tool_op_3.id),
        "source parent is retained"
    );
    let parent_keys = kept_message.parent_keys(&BTreeMap::new(), &HashMap::new());
    assert_eq!(parent_keys, vec![tool_op_4.id.to_string()]);
    let HistoryNode::ExecuteBundle { .. } = &bundled[0] else {
        panic!("expected ExecuteBundle at the run slot");
    };
}
