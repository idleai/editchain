//! Tests for collapsing raw import ops with their normalized children.

#![expect(
    clippy::panic,
    reason = "Tests assert on enum variants with explicit panic arms"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "Tests index into vectors whose length is asserted immediately before"
)]
// Crate-level dependency markers (used by Cargo for feature resolution).
use regex as _;
use serde as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, CommandOp, CommandStage, GitAvailability, GitCommitEntity, GitLink,
    GitLinkKind, GitObjectFormat, GitOid, ImportOp, MessageOp, NodeId, NoteOp, NoteRelationship,
    Op, OpId, OpKind, ParentSet, Payload, RepositoryId, ScopeRef, SessionId, Tags, ToolOp,
    ToolStage,
};

/// Build a metadata-only raw import op (tagged META).
fn meta_import_op(node: u64, seq: u64) -> Op {
    let mut op = import_op(node, seq);
    op.tags = Tags::IMPORT | Tags::META;
    op
}

/// Build the exact Codex token-usage envelope emitted by legacy imports,
/// deliberately without the `META` tag added by current importers.
fn legacy_token_usage_import_op(node: u64, seq: u64, parent: OpId) -> Op {
    let usage = serde_json::json!({
        "input_tokens": 10,
        "cached_input_tokens": 2,
        "cache_write_input_tokens": 0,
        "output_tokens": 3,
        "reasoning_output_tokens": 1,
        "total_tokens": 13,
    });
    let mut op = import_op(node, seq);
    op.parents = ParentSet::One(parent);
    if let OpKind::Import(import) = &mut op.kind {
        import.raw_ref = Payload::Inline(
            serde_json::json!({
                "timestamp": "2026-09-06T14:17:24.614Z",
                "type": "token_usage_record",
                "payload": {
                    "thread_id": "0195cda5-433d-7f9a-9d7b-a9f15b60c2e2",
                    "turn_id": "turn-1",
                    "session_id": "0195cda5-433d-7f9a-9d7b-a9f15b60c2e2",
                    "root_turn_id": "turn-1",
                    "response_id": "response-1",
                    "usage": usage.clone(),
                    "turn_token_usage": usage.clone(),
                    "thread_token_usage": usage,
                },
            })
            .to_string()
            .into_bytes(),
        );
    }
    op
}
use editchain_project::HistoryProjection;

fn git_commit(oid_byte: u8, committed_at: i64) -> GitCommitEntity {
    let oid = |byte: u8| {
        let mut bytes = [0u8; 32];
        bytes[0] = byte;
        GitOid::new(GitObjectFormat::Sha1, bytes)
    };
    GitCommitEntity {
        repository: RepositoryId(1),
        object_format: GitObjectFormat::Sha1,
        oid: oid(oid_byte),
        imported_record: None,
        availability: GitAvailability::Resolved,
        tree: oid(0),
        parents: Vec::new(),
        author: editchain_core::GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: committed_at,
        },
        committer: editchain_core::GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: committed_at,
        },
        authored_at: committed_at,
        committed_at,
        message: Payload::Empty,
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    }
}

/// Build a raw import op.
fn import_op(node: u64, seq: u64) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(format!("raw line {seq}").into_bytes()),
            raw_hash: None,
        }),
    }
}

/// Build one exact relation fact anchored on a raw occurrence.
fn relation_fact(seq: u64, anchor: OpId, target: OpId, relationship: NoteRelationship) -> Op {
    Op {
        id: OpId::new(NodeId(91), 0, seq),
        parents: ParentSet::One(anchor),
        actor: ActorId(0),
        clock: Clock::None,
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::IMPORT | Tags::META,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![target],
            relationship,
            content: Payload::Inline(b"exact test evidence".to_vec()),
        }),
    }
}

/// Build a normalized message op whose parent is `parent`.
fn message_op(node: u64, seq: u64, parent: OpId, text: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::HUMAN | Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.as_bytes().to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// Build a normalized tool op whose parent is `parent`.
fn tool_op(node: u64, seq: u64, parent: OpId, name: &str) -> Op {
    tool_op_with_id((node, seq, parent), (name, "call-1", ToolStage::Start))
}

/// Build a normalized tool op with an explicit provider call identity/stage.
fn tool_op_with_id(position: (u64, u64, OpId), identity: (&str, &str, ToolStage)) -> Op {
    let (node, seq, parent) = position;
    let (name, call_id, stage) = identity;
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(call_id.as_bytes().to_vec()),
            tool_name: Payload::Inline(name.as_bytes().to_vec()),
            stage,
            content: Payload::Empty,
        }),
    }
}

/// Build a normalized tool-result import whose raw JSON carries a structured
/// `status`, so its derived outcome is `Success`/`Failure`/`Cancelled` rather
/// than `Unknown`.
fn status_result_import(node: u64, seq: u64, parent: OpId, status: &str) -> Op {
    let mut op = import_op(node, seq);
    op.parents = ParentSet::One(parent);
    if let OpKind::Import(import) = &mut op.kind {
        import.raw_ref = Payload::Inline(
            serde_json::json!({
                "type": "response_item",
                "payload": { "item": { "status": status } }
            })
            .to_string()
            .into_bytes(),
        );
    }
    op
}

/// Build a tool-result import (with an optional structured `status`) plus its
/// Finish-stage tool child, parented to the given call import.
fn tool_result_pair(
    node: u64,
    seq: u64,
    tool_seq: u64,
    parent: OpId,
    status: Option<&str>,
) -> (Op, Op) {
    let import = if let Some(status) = status {
        status_result_import(node, seq, parent, status)
    } else {
        let mut op = import_op(node, seq);
        op.parents = ParentSet::One(parent);
        op
    };
    let mut tool = tool_op(node, tool_seq, import.id, "");
    if let OpKind::Tool(t) = &mut tool.kind {
        t.stage = ToolStage::Finish;
        t.content = Payload::Inline(b"output".to_vec());
    }
    (import, tool)
}

#[test]
fn collapse_reduces_node_count_and_chains() {
    // Two raw import ops forming a linear chain (op2's parent is op1).
    let op1 = import_op(1, 1);
    let mut op2 = import_op(1, 2);
    op2.parents = ParentSet::One(op1.id);
    // op1 has a message child; op2 has a tool child.
    let msg = message_op(1, 3, op1.id, "hello world");
    let tool = tool_op(1, 4, op2.id, "Bash");

    let projection =
        HistoryProjection::from_ops(vec![op1.clone(), msg.clone(), op2.clone(), tool.clone()]);
    let nodes = projection.nodes();

    // 4 ops collapse to 2 nodes (one per raw import).
    assert_eq!(nodes.len(), 2);
    // Both are collapsed imports.
    for n in &nodes {
        assert!(matches!(
            n,
            editchain_project::HistoryNode::CollapsedImport { .. }
        ));
    }
}

#[test]
fn collapse_derives_meaningful_summary() {
    let op1 = import_op(1, 1);
    let msg = message_op(1, 3, op1.id, "hello world");
    let projection = HistoryProjection::from_ops(vec![op1.clone(), msg]);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 1);
    // Summary should be the message text, not the raw JSONL.
    assert_eq!(nodes.first().unwrap().summary(), "hello world");
}

#[test]
fn collapse_tool_summary_prefixes_tool() {
    let op1 = import_op(1, 1);
    let tool = tool_op(1, 3, op1.id, "Bash");
    let projection = HistoryProjection::from_ops(vec![op1.clone(), tool]);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes.first().unwrap().summary(), "tool: Bash");
}

#[test]
fn collapse_author_derived_from_children_tags() {
    // A raw import op whose child is a HUMAN message should collapse to a node
    // with author "human" (not "system", which the raw import's IMPORT-only tags
    // would otherwise produce).
    let op1 = import_op(1, 1);
    let msg = message_op(1, 3, op1.id, "hello world");
    let projection = HistoryProjection::from_ops(vec![op1.clone(), msg]);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 1);
    let author = match nodes.first().unwrap() {
        editchain_project::HistoryNode::CollapsedImport { author, .. } => author,
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::ExecuteBundle { .. }
        | editchain_project::HistoryNode::PlanBundle { .. }
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    };
    assert_eq!(author, "human");
}

#[test]
fn collapse_author_prefers_human_over_agent() {
    // A raw import op with both a HUMAN message and an AGENT tool child should
    // report "human" (HUMAN takes precedence).
    let op1 = import_op(1, 1);
    let msg = message_op(1, 3, op1.id, "hello world");
    let tool = tool_op(1, 4, op1.id, "Bash");
    let projection = HistoryProjection::from_ops(vec![op1.clone(), msg, tool]);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 1);
    let author = match nodes.first().unwrap() {
        editchain_project::HistoryNode::CollapsedImport { author, .. } => author,
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::ExecuteBundle { .. }
        | editchain_project::HistoryNode::PlanBundle { .. }
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    };
    assert_eq!(author, "human");
}

#[test]
fn meta_imports_bundle_along_exact_parent_chain() {
    // A real turn (import + message child), then two causally parented META
    // imports, then another real turn. The META imports must bundle along their
    // exact parent path and not appear as their own nodes.
    let options = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn1 = import_op(1, 1);
    let msg = message_op(1, 2, turn1.id, "hello world");
    let mut meta1 = meta_import_op(1, 3);
    meta1.parents = ParentSet::One(turn1.id);
    let mut meta2 = meta_import_op(1, 4);
    meta2.parents = ParentSet::One(meta1.id);
    let turn2 = import_op(1, 5);
    let tool = tool_op(1, 6, turn2.id, "Bash");

    let projection = HistoryProjection::from_ops_with(
        vec![
            turn1.clone(),
            msg,
            meta1.clone(),
            meta2.clone(),
            turn2.clone(),
            tool,
        ],
        options,
    );
    let nodes = projection.nodes();

    // Two real turns only — the META imports are bundled, not separate nodes.
    assert_eq!(nodes.len(), 2);
    // Newest-first: nodes[0] is turn2 (no sub-ops), nodes[1] is turn1 carrying
    // both META sub-ops.
    let older = &nodes[1];
    match older {
        editchain_project::HistoryNode::CollapsedImport { sub_ops, .. } => {
            assert_eq!(sub_ops.len(), 2);
            assert_eq!(sub_ops[0].id, meta1.id);
            assert_eq!(sub_ops[1].id, meta2.id);
        }
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::ExecuteBundle { .. }
        | editchain_project::HistoryNode::PlanBundle { .. }
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    }
}

#[test]
fn unparented_meta_after_turn_stays_standalone() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn = import_op(1, 1);
    let meta = meta_import_op(1, 2);

    let projection = HistoryProjection::from_ops_with(vec![turn, meta.clone()], opts);

    assert!(projection
        .nodes()
        .iter()
        .any(|node| node.node_key() == meta.id.to_string()));
}

#[test]
fn metadata_bundle_follows_provider_parent_across_structural_row() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn = import_op(1, 1);
    let mut timing = meta_import_op(1, 3);
    timing.parents = ParentSet::One(turn.id);
    let mut boundary = import_op(1, 5);
    boundary.parents = ParentSet::One(timing.id);
    boundary.tags |= Tags::STRUCTURAL;
    let mut local_command = meta_import_op(1, 7);
    local_command.parents = ParentSet::One(boundary.id);

    let turn_entity = OpId::new(NodeId(90), 0, 1);
    let timing_entity = OpId::new(NodeId(90), 0, 2);
    let boundary_entity = OpId::new(NodeId(90), 0, 3);
    let local_entity = OpId::new(NodeId(90), 0, 4);
    let facts = vec![
        relation_fact(1, turn.id, turn_entity, NoteRelationship::OccurrenceOf),
        relation_fact(2, timing.id, timing_entity, NoteRelationship::OccurrenceOf),
        relation_fact(3, timing.id, turn_entity, NoteRelationship::ProviderParent),
        relation_fact(
            4,
            boundary.id,
            boundary_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_fact(
            5,
            boundary.id,
            timing_entity,
            NoteRelationship::ProviderParent,
        ),
        relation_fact(
            6,
            local_command.id,
            local_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_fact(
            7,
            local_command.id,
            boundary_entity,
            NoteRelationship::ProviderParent,
        ),
    ];
    let mut records = vec![
        turn.clone(),
        timing.clone(),
        boundary.clone(),
        local_command.clone(),
    ];
    records.extend(facts);

    let projection = HistoryProjection::from_ops_with(records, opts);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0].node_key(), boundary.id.to_string());
    assert_eq!(nodes[1].node_key(), turn.id.to_string());

    let turn_row = nodes
        .iter()
        .find(|node| node.node_key() == turn.id.to_string())
        .unwrap();
    let boundary_row = nodes
        .iter()
        .find(|node| node.node_key() == boundary.id.to_string())
        .unwrap();
    assert_eq!(
        turn_row
            .sub_ops()
            .iter()
            .map(|op| op.id)
            .collect::<Vec<_>>(),
        vec![timing.id]
    );
    assert_eq!(
        boundary_row
            .sub_ops()
            .iter()
            .map(|op| op.id)
            .collect::<Vec<_>>(),
        vec![local_command.id]
    );
    assert!(projection.lifted_parent_keys(turn_row).is_empty());
    assert_eq!(
        projection.lifted_parent_keys(boundary_row),
        vec![turn.id.to_string()]
    );
    assert_eq!(projection.independent_chains(), 1);
}

#[test]
fn incremental_tool_result_correlations_do_not_create_graph_edges() {
    struct ToolPath {
        root: Op,
        branch: Op,
    }

    let mut left_path = ToolPath {
        root: import_op(1, 10),
        branch: import_op(1, 20),
    };
    left_path.branch.parents = ParentSet::One(left_path.root.id);
    let mut result_short = import_op(1, 30);
    result_short.parents = ParentSet::One(left_path.branch.id);

    let mut right_path = ToolPath {
        root: import_op(2, 10),
        branch: import_op(2, 20),
    };
    right_path.branch.parents = ParentSet::One(right_path.root.id);
    let mut result_full = import_op(2, 30);
    result_full.parents = ParentSet::One(right_path.branch.id);

    for (op, raw, hash) in [
        (&mut left_path.root, b"call-a".as_slice(), [1; 32]),
        (&mut right_path.root, b"call-a".as_slice(), [1; 32]),
        (&mut left_path.branch, b"call-b".as_slice(), [2; 32]),
        (&mut right_path.branch, b"call-b".as_slice(), [2; 32]),
        (&mut result_short, b"result-a".as_slice(), [3; 32]),
        (&mut result_full, b"result-a+b".as_slice(), [4; 32]),
    ] {
        let OpKind::Import(import) = &mut op.kind else {
            panic!("expected import fixture");
        };
        import.raw_ref = Payload::Inline(raw.to_vec());
        import.raw_hash = Some(hash);
    }

    let call_entities = [OpId::new(NodeId(90), 1, 1), OpId::new(NodeId(90), 1, 2)];
    let result_entity = OpId::new(NodeId(90), 1, 3);
    let tool_entities = [OpId::new(NodeId(90), 2, 1), OpId::new(NodeId(90), 2, 2)];
    let mut records = vec![
        left_path.root.clone(),
        tool_op_with_id(
            (1, 11, left_path.root.id),
            ("Read", "call-a", ToolStage::Start),
        ),
        left_path.branch.clone(),
        tool_op_with_id(
            (1, 21, left_path.branch.id),
            ("Read", "call-b", ToolStage::Start),
        ),
        result_short.clone(),
        tool_op_with_id((1, 31, result_short.id), ("", "call-a", ToolStage::Finish)),
        right_path.root.clone(),
        tool_op_with_id(
            (2, 11, right_path.root.id),
            ("Read", "call-a", ToolStage::Start),
        ),
        right_path.branch.clone(),
        tool_op_with_id(
            (2, 21, right_path.branch.id),
            ("Read", "call-b", ToolStage::Start),
        ),
        result_full.clone(),
        tool_op_with_id((2, 31, result_full.id), ("", "call-a", ToolStage::Finish)),
        tool_op_with_id((2, 32, result_full.id), ("", "call-b", ToolStage::Finish)),
    ];
    records.extend([
        relation_fact(
            101,
            left_path.root.id,
            call_entities[0],
            NoteRelationship::OccurrenceOf,
        ),
        relation_fact(
            102,
            left_path.root.id,
            tool_entities[0],
            NoteRelationship::Contains,
        ),
        relation_fact(
            103,
            right_path.root.id,
            call_entities[0],
            NoteRelationship::OccurrenceOf,
        ),
        relation_fact(
            104,
            right_path.root.id,
            tool_entities[0],
            NoteRelationship::Contains,
        ),
        relation_fact(
            105,
            left_path.branch.id,
            call_entities[1],
            NoteRelationship::OccurrenceOf,
        ),
        relation_fact(
            106,
            left_path.branch.id,
            call_entities[0],
            NoteRelationship::ProviderParent,
        ),
        relation_fact(
            107,
            left_path.branch.id,
            tool_entities[1],
            NoteRelationship::Contains,
        ),
        relation_fact(
            108,
            right_path.branch.id,
            call_entities[1],
            NoteRelationship::OccurrenceOf,
        ),
        relation_fact(
            109,
            right_path.branch.id,
            call_entities[0],
            NoteRelationship::ProviderParent,
        ),
        relation_fact(
            110,
            right_path.branch.id,
            tool_entities[1],
            NoteRelationship::Contains,
        ),
        relation_fact(
            111,
            result_short.id,
            result_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_fact(
            112,
            result_short.id,
            call_entities[0],
            NoteRelationship::ProviderParent,
        ),
        relation_fact(
            113,
            result_short.id,
            tool_entities[0],
            NoteRelationship::ToolResultOf,
        ),
        relation_fact(
            114,
            result_full.id,
            result_entity,
            NoteRelationship::OccurrenceOf,
        ),
        relation_fact(
            115,
            result_full.id,
            call_entities[0],
            NoteRelationship::ProviderParent,
        ),
        relation_fact(
            116,
            result_full.id,
            tool_entities[0],
            NoteRelationship::ToolResultOf,
        ),
        relation_fact(
            117,
            result_full.id,
            tool_entities[1],
            NoteRelationship::ToolResultOf,
        ),
    ]);

    let projection = HistoryProjection::from_ops(records);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 3);
    assert_eq!(
        projection.visible_op_id(result_short.id),
        Some(left_path.root.id)
    );
    assert_eq!(
        projection.visible_op_id(result_full.id),
        Some(result_full.id)
    );

    let row_by_id = |id: OpId| {
        nodes
            .iter()
            .find(|node| node.node_key() == id.to_string())
            .unwrap()
    };
    assert!(projection
        .lifted_parent_keys(row_by_id(left_path.root.id))
        .is_empty());
    assert_eq!(
        projection.lifted_parent_keys(row_by_id(left_path.branch.id)),
        vec![left_path.root.id.to_string()]
    );
    assert_eq!(
        projection.lifted_parent_keys(row_by_id(result_full.id)),
        vec![left_path.root.id.to_string()],
        "the provider parent supplies ancestry; correlated tool calls do not"
    );

    let positions: std::collections::HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.node_key(), index))
        .collect();
    for node in &nodes {
        let child = positions[&node.node_key()];
        for parent in projection.lifted_parent_keys(node) {
            assert!(child < positions[&parent], "edge must point downward");
        }
    }
}

#[test]
fn derived_parent_override_wins_over_provider_notes_without_mutating_canonical_topology() {
    let root = import_op(41, 1);
    let mut child = import_op(41, 2);
    child.parents = ParentSet::One(root.id);
    let child_entity = OpId::new(NodeId(90), 3, 1);
    let projection = HistoryProjection::from_ops(vec![
        root.clone(),
        child.clone(),
        relation_fact(201, child.id, child_entity, NoteRelationship::OccurrenceOf),
        relation_fact(202, child.id, root.id, NoteRelationship::ProviderParent),
    ]);
    let canonical = projection
        .nodes()
        .into_iter()
        .find(|node| node.node_key() == child.id.to_string())
        .unwrap_or_else(|| panic!("canonical child missing"));
    assert_eq!(
        projection.lifted_parent_keys(&canonical),
        vec![root.id.to_string()]
    );

    let mut derived = canonical.clone();
    derived.override_parent_keys(&[]);
    assert!(
        derived
            .parent_keys(&projection.git.links, projection.relationship_notes())
            .is_empty(),
        "a derived view must not silently reintroduce immutable provider edges"
    );
    assert_eq!(
        projection.lifted_parent_keys(&canonical),
        vec![root.id.to_string()],
        "the canonical projection remains unchanged"
    );
}

#[test]
fn meta_bundle_keeps_parents_unchanged() {
    // A META import sits on the backbone between two real turns. It is bundled
    // (dropped from the top-level list) into turn1. The second turn's causal
    // parent is the META op. q6 Phase-1 contract: bundling MUST NOT rewrite
    // stored parents/clocks — turn2 keeps pointing at the META op; chain
    // continuity is preserved through the representative map, not a parent splice.
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn1 = import_op(1, 1);
    let msg = message_op(1, 2, turn1.id, "hello world");
    let mut meta = meta_import_op(1, 3);
    meta.parents = ParentSet::One(turn1.id);
    // turn2's parent is the META op (it follows it on the backbone).
    let mut turn2 = import_op(1, 4);
    turn2.parents = ParentSet::One(meta.id);

    let projection = HistoryProjection::from_ops_with(
        vec![turn1.clone(), msg, meta.clone(), turn2.clone()],
        opts,
    );
    let nodes = projection.nodes();

    // Two real turns only (the META import is bundled into turn1).
    assert_eq!(nodes.len(), 2);
    // The META op is bundled under turn1 (the older node, nodes[1]).
    let older = &nodes[1];
    match older {
        editchain_project::HistoryNode::CollapsedImport { sub_ops, .. } => {
            assert_eq!(sub_ops.len(), 1, "META op should bundle under turn1");
            assert_eq!(sub_ops[0].id, meta.id);
        }
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::ExecuteBundle { .. }
        | editchain_project::HistoryNode::PlanBundle { .. }
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    }

    // Storage parents are NOT rewritten: turn2 still points at the bundled META
    // op (never the absorbing turn1). Layout resolves it through the
    // representative map instead.
    let newer = &nodes[0];
    let meta_key = meta.id.to_string();
    let newer_parents = newer.parent_keys(
        &std::collections::BTreeMap::new(),
        &std::collections::HashMap::new(),
    );
    assert!(
        newer_parents.contains(&meta_key),
        "bundling must NOT rewrite causal parents; turn2 should still parent to \
         the bundled META op, got {newer_parents:?}"
    );
}

#[test]
fn produced_commit_link_branches_from_folded_source_without_rewriting_agent_chain() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn = import_op(1, 1);
    let msg = message_op(1, 2, turn.id, "hello world");
    let mut meta = meta_import_op(1, 3);
    meta.parents = ParentSet::One(turn.id);
    let mut continuation = import_op(1, 4);
    continuation.parents = ParentSet::One(meta.id);
    let commit = git_commit(7, 5);
    let link_record = Op {
        id: OpId::new(NodeId(99), 0, 1),
        parents: ParentSet::One(meta.id),
        actor: ActorId(1),
        clock: Clock::None,
        scope: meta.scope,
        tags: Tags::IMPORT | Tags::META,
        kind: OpKind::GitLink(GitLink {
            source: meta.id,
            target_repo: commit.repository,
            target_oid: commit.oid,
            kind: GitLinkKind::ProducedBy,
        }),
    };
    let mut projection = HistoryProjection::from_ops_with(
        vec![
            turn.clone(),
            msg,
            meta.clone(),
            continuation.clone(),
            link_record,
        ],
        opts,
    );
    projection.merge_git_commits(vec![commit.clone()]);

    let nodes = projection.nodes();
    let source = nodes
        .iter()
        .find(|node| node.node_key() == turn.id.to_string())
        .unwrap();
    let continued = nodes
        .iter()
        .find(|node| node.node_key() == continuation.id.to_string())
        .unwrap();
    let committed = nodes
        .iter()
        .find(|node| node.node_key() == commit.oid.to_hex())
        .unwrap();

    assert!(
        !projection
            .lifted_parent_keys(source)
            .contains(&commit.oid.to_hex()),
        "ProducedBy is not a BasedOn edge from the command back to its result"
    );
    assert_eq!(
        projection.lifted_parent_keys(continued),
        vec![turn.id.to_string()],
        "the agent continuation remains attached to the producing row"
    );
    assert_eq!(
        projection.lifted_parent_keys(committed),
        vec![turn.id.to_string()],
        "the commit becomes a second child of the producing row"
    );
    assert_eq!(
        projection.parent_relations_for(committed, &[turn.id.to_string()]),
        vec![editchain_project::ParentRelation {
            parent: turn.id.to_string(),
            kind: editchain_project::RelationKind::ProducedCommit,
        }]
    );
    let structural = projection.structural_row_keys(&nodes);
    assert!(structural.contains(&turn.id.to_string()));
    assert!(structural.contains(&commit.oid.to_hex()));

    let raw_commit_parents =
        committed.parent_keys(&projection.git.links, projection.relationship_notes());
    assert_eq!(raw_commit_parents, vec![meta.id.to_string()]);
    let raw_source_parents =
        source.parent_keys(&projection.git.links, projection.relationship_notes());
    assert!(
        !raw_source_parents.contains(&commit.oid.to_hex()),
        "the source operation never treats its produced commit as an ancestor"
    );
}

#[test]
fn bundled_meta_based_on_link_is_inherited_by_visible_anchor() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn = import_op(1, 1);
    let msg = message_op(1, 2, turn.id, "hello world");
    let mut meta = meta_import_op(1, 3);
    meta.parents = ParentSet::One(turn.id);
    let projection = HistoryProjection::from_ops_with(vec![turn.clone(), msg, meta.clone()], opts);
    let node = projection
        .nodes()
        .into_iter()
        .find(|node| node.node_key() == turn.id.to_string())
        .unwrap();

    let mut bytes = [0u8; 32];
    bytes[0] = 8;
    let target_oid = GitOid::new(GitObjectFormat::Sha1, bytes);
    let mut links = std::collections::BTreeMap::new();
    drop(links.insert(
        meta.id,
        vec![GitLink {
            source: meta.id,
            target_repo: RepositoryId(1),
            target_oid,
            kind: GitLinkKind::BasedOn,
        }],
    ));

    assert_eq!(
        node.parent_keys(&links, &std::collections::HashMap::new()),
        vec![target_oid.to_hex()],
        "an exact session-start BasedOn relation must remain on its visible turn"
    );
}

#[test]
fn projection_does_not_infer_links_from_git_command_text_or_timestamps() {
    let command = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_000_100),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::COMMAND,
        kind: OpKind::Command(CommandOp {
            command_id: Payload::Empty,
            content: Payload::Inline(b"git commit -m inferred-before-refactor".to_vec()),
            stage: CommandStage::Finish,
        }),
    };
    let mut commit = git_commit(9, 1_000);
    commit.repository = RepositoryId(1);

    let mut projection = HistoryProjection::from_ops(vec![command]);
    projection.merge_git_commits(vec![commit]);

    assert!(
        projection.git.links.is_empty(),
        "only durable GitLink ops may connect sessions to Git"
    );
}

#[test]
fn tool_result_summary_previews_content() {
    // A tool_result (Finish, empty tool_name) should preview its content, not
    // show an empty summary.
    let op1 = import_op(1, 1);
    let mut result = tool_op(1, 3, op1.id, "");
    // tool_op sets stage Start; make it a Finish result with content.
    if let OpKind::Tool(t) = &mut result.kind {
        t.stage = ToolStage::Finish;
        t.content = Payload::Inline(b"1\tline one\n2\tline two\n".to_vec());
    }
    let projection = HistoryProjection::from_ops(vec![op1.clone(), result]);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 1);
    // Summary should be the first line with the line-number prefix stripped.
    assert_eq!(nodes.first().unwrap().summary(), "line one");
}

#[test]
fn tool_result_summary_truncates_long_content() {
    let op1 = import_op(1, 1);
    let mut result = tool_op(1, 3, op1.id, "");
    if let OpKind::Tool(t) = &mut result.kind {
        t.stage = ToolStage::Finish;
        t.content = Payload::Inline(vec![b'x'; 1100]);
    }
    let projection = HistoryProjection::from_ops(vec![op1.clone(), result]);
    let nodes = projection.nodes();
    let summary = nodes.first().unwrap().summary();
    assert!(summary.chars().count() <= 1025); // 1024 + ellipsis
    assert!(summary.ends_with('…'));
}

#[test]
fn tool_result_json_summary_pulls_label() {
    // A tool_result whose content is a JSON blob should show a short label, not
    // the raw JSON.
    let op1 = import_op(1, 1);
    let mut result = tool_op(1, 3, op1.id, "");
    if let OpKind::Tool(t) = &mut result.kind {
        t.stage = ToolStage::Finish;
        t.content = Payload::Inline(
            b"{\n  \"numStartups\": 557,\n  \"installMethod\": \"native\"\n}".to_vec(),
        );
    }
    let projection = HistoryProjection::from_ops(vec![op1.clone(), result]);
    let nodes = projection.nodes();
    let summary = nodes.first().unwrap().summary();
    assert!(summary.contains("numStartups=557"), "got {summary:?}");
    assert!(!summary.starts_with('{'));
}

#[test]
fn attachment_import_label_uses_filename() {
    // An attachment record with a filename should summarize as `<type>: <file>`.
    let op1 = import_op(1, 1);
    // Make it an attachment import with raw JSONL.
    let mut op = op1;
    op.kind = OpKind::Import(ImportOp {
        raw_ref: Payload::Inline(
            br#"{"type":"attachment","attachment":{"type":"edited_text_file","filename":"/repo/src/main.rs"}}"#
                .to_vec(),
        ),
        raw_hash: None,
    });
    let projection = HistoryProjection::from_ops(vec![op]);
    let nodes = projection.nodes();
    assert_eq!(
        nodes.first().unwrap().summary(),
        "edited_text_file: main.rs"
    );
}

#[test]
fn tool_result_groups_into_tool_call() {
    // A tool call import (Tool Start "Bash") followed by a tool result import
    // (Tool Finish, empty name) whose parent is the call's raw import. The
    // result should fold into the call's sub-ops, not render as its own node.
    let call_import = import_op(1, 1);
    let call = tool_op(1, 2, call_import.id, "Bash");
    // The result import's parent is the call's raw import (it follows it).
    let mut result_import = import_op(1, 3);
    result_import.parents = ParentSet::One(call_import.id);
    let mut result = tool_op(1, 4, result_import.id, "");
    if let OpKind::Tool(t) = &mut result.kind {
        t.stage = ToolStage::Finish;
        t.content = Payload::Inline(b"1\tline one\n2\tline two".to_vec());
    }

    let projection = HistoryProjection::from_ops(vec![
        call_import.clone(),
        call,
        result_import.clone(),
        result,
    ]);
    let nodes = projection.nodes();

    // One node (the tool call); the result is folded into its sub-ops.
    assert_eq!(nodes.len(), 1);
    let node = &nodes[0];
    // The combined summary includes the call name plus the result preview.
    assert_eq!(node.summary(), "tool: Bash line one");
    assert_eq!(node.sub_ops().len(), 1);
}

#[test]
fn grouped_codex_exec_failure_overrides_completed_call_envelope() {
    // The call's `status: completed` only concludes its lifecycle. The
    // attached custom-exec result is the authoritative execution outcome and
    // must make the single visible call+result row a failure.
    let mut call_import = import_op(1, 1);
    if let OpKind::Import(import) = &mut call_import.kind {
        import.raw_ref = Payload::Inline(
            br#"{"type":"response_item","payload":{"type":"custom_tool_call","status":"completed","name":"exec"}}"#
                .to_vec(),
        );
    }
    let call = tool_op(1, 2, call_import.id, "exec");
    let mut result_import = import_op(1, 3);
    result_import.parents = ParentSet::One(call_import.id);
    if let OpKind::Import(import) = &mut result_import.kind {
        import.raw_ref = Payload::Inline(
            br#"{"type":"response_item","payload":{"type":"custom_tool_call_output","output":[{"type":"input_text","text":"Script failed\nWall time 0.0 seconds\nOutput:\n"},{"type":"input_text","text":"Script error:\ncommand rejected"}]}}"#
                .to_vec(),
        );
    }
    let mut result = tool_op(1, 4, result_import.id, "");
    if let OpKind::Tool(tool) = &mut result.kind {
        tool.stage = ToolStage::Finish;
        tool.content = Payload::Inline(b"Script failed".to_vec());
    }

    let nodes = HistoryProjection::from_ops(vec![call_import, call, result_import, result]).nodes();

    assert_eq!(nodes.len(), 1);
    assert_eq!(
        nodes[0].outcome(),
        editchain_project::taxonomy::Outcome::Failure
    );
    assert_eq!(nodes[0].sub_ops().len(), 1);
}

#[test]
fn grouped_tool_results_keep_failure_over_later_success() {
    // Two tool-result rows fold into one call: a Failure followed by a later
    // Success. Last-wins folding would let the later Success mask the earlier
    // Failure and render a misleading success badge; the merge must keep the
    // Failure as the combined visible outcome.
    let call_import = import_op(1, 1);
    let call = tool_op(1, 2, call_import.id, "Bash");
    let mut ops = vec![call_import.clone(), call];
    for (node, seq, tool_seq, status) in [
        (1u64, 3u64, 4u64, Some("failed")),
        (1, 5, 6, Some("completed")),
    ] {
        let (import, tool) = tool_result_pair(node, seq, tool_seq, call_import.id, status);
        ops.push(import);
        ops.push(tool);
    }

    let projection = HistoryProjection::from_ops(ops);
    let nodes = projection.nodes();

    // One visible row (the call); both results are folded into its sub-ops.
    assert_eq!(nodes.len(), 1);
    let node = &nodes[0];
    assert_eq!(node.node_key(), call_import.id.to_string());
    assert_eq!(
        node.outcome(),
        editchain_project::taxonomy::Outcome::Failure,
        "a later Success must not erase the earlier Failure"
    );
}

#[test]
fn grouped_tool_results_keep_success_when_all_succeed() {
    // Two successful results plus one unknown-status result fold into one call;
    // the combined outcome stays Success, and the unknown result never
    // downgrades known evidence.
    let call_import = import_op(1, 1);
    let call = tool_op(1, 2, call_import.id, "Bash");
    let mut ops = vec![call_import.clone(), call];
    // The last entry carries no structured status -> derived outcome Unknown.
    for (node, seq, tool_seq, status) in [
        (1u64, 3u64, 4u64, Some("completed")),
        (1, 5, 6, Some("succeeded")),
        (1, 7, 8, None),
    ] {
        let (import, tool) = tool_result_pair(node, seq, tool_seq, call_import.id, status);
        ops.push(import);
        ops.push(tool);
    }

    let projection = HistoryProjection::from_ops(ops);
    let nodes = projection.nodes();

    assert_eq!(nodes.len(), 1);
    let node = &nodes[0];
    assert_eq!(node.node_key(), call_import.id.to_string());
    assert_eq!(
        node.outcome(),
        editchain_project::taxonomy::Outcome::Success,
        "unknown must not erase known success evidence"
    );
}

#[test]
fn combined_summary_truncates_at_1024() {
    // A row with many sub-ops whose combined content exceeds ~1024 chars should
    // truncate with an ellipsis.
    let call_import = import_op(1, 1);
    let call = tool_op(1, 2, call_import.id, "Bash");
    // Attach many tool-result sub-ops (each ~90 chars of preview) so the combined
    // content exceeds 1024.
    let mut ops = vec![call_import.clone(), call];
    for i in 0..15u64 {
        let mut result_import = import_op(1, 3 + i * 2);
        result_import.parents = ParentSet::One(call_import.id);
        let mut result = tool_op(1, 4 + i * 2, result_import.id, "");
        if let OpKind::Tool(t) = &mut result.kind {
            t.stage = ToolStage::Finish;
            t.content = Payload::Inline(vec![b'y'; 100]);
        }
        ops.push(result_import);
        ops.push(result);
    }

    let projection = HistoryProjection::from_ops(ops);
    let nodes = projection.nodes();
    let summary = nodes.first().unwrap().summary();
    assert!(summary.chars().count() <= 1025); // 1024 + ellipsis
    assert!(summary.ends_with('…'));
}

#[test]
fn tool_result_without_call_stays_standalone() {
    // A tool result whose parent is NOT a tool call stays as its own node.
    let msg_import = import_op(1, 1);
    let msg = message_op(1, 2, msg_import.id, "hello");
    let mut result_import = import_op(1, 3);
    result_import.parents = ParentSet::One(msg_import.id);
    let mut result = tool_op(1, 4, result_import.id, "");
    if let OpKind::Tool(t) = &mut result.kind {
        t.stage = ToolStage::Finish;
        t.content = Payload::Inline(b"output".to_vec());
    }

    let projection =
        HistoryProjection::from_ops(vec![msg_import.clone(), msg, result_import.clone(), result]);
    let nodes = projection.nodes();
    // Both the message node and the standalone tool-result node survive.
    assert_eq!(nodes.len(), 2);
}

#[test]
fn exact_tool_result_does_not_contract_across_semantic_command() {
    // Codex can record one execution twice: an outer custom-tool lifecycle and
    // an intervening CommandExecution event with its own provider identity.
    // The later output still names the outer call exactly, but folding it all
    // the way back to that call would skip the visible command and turn the
    // command plus continuation into sibling branches.
    let call_import = import_op(1, 1);
    let call = tool_op_with_id(
        (1, 2, call_import.id),
        ("exec", "call-outer", ToolStage::Start),
    );

    let mut command_import = import_op(1, 3);
    command_import.parents = ParentSet::One(call_import.id);
    let command = Op {
        id: OpId::new(NodeId(1), 0, 4),
        parents: ParentSet::One(command_import.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(4),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::AGENT | Tags::COMMAND,
        kind: OpKind::Command(CommandOp {
            command_id: Payload::Inline(b"exec-inner".to_vec()),
            content: Payload::Inline(b"git status".to_vec()),
            stage: CommandStage::Finish,
        }),
    };

    let mut result_import = import_op(1, 5);
    result_import.parents = ParentSet::One(command_import.id);
    let mut result = tool_op_with_id(
        (1, 6, result_import.id),
        ("", "call-outer", ToolStage::Finish),
    );
    if let OpKind::Tool(tool) = &mut result.kind {
        tool.content = Payload::Inline(b"clean".to_vec());
    }

    let mut continuation = import_op(1, 7);
    continuation.parents = ParentSet::One(result_import.id);
    let continuation_message = message_op(1, 8, continuation.id, "done");

    let projection = HistoryProjection::from_ops(vec![
        call_import.clone(),
        call,
        command_import.clone(),
        command,
        result_import.clone(),
        result,
        continuation.clone(),
        continuation_message,
    ]);

    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 4);
    assert_eq!(
        projection.visible_op_id(result_import.id),
        Some(result_import.id),
        "an exact call id must not authorize contraction across a semantic row"
    );
    let row = |id: OpId| {
        nodes
            .iter()
            .find(|node| node.node_key() == id.to_string())
            .unwrap()
    };
    assert_eq!(
        projection.lifted_parent_keys(row(result_import.id)),
        vec![command_import.id.to_string()]
    );
    assert_eq!(
        projection.lifted_parent_keys(row(continuation.id)),
        vec![result_import.id.to_string()]
    );
    assert!(
        projection
            .graph_layout()
            .rows
            .iter()
            .all(|layout| layout.lane == 0),
        "the preserved source path is linear"
    );
}

#[test]
fn direct_tool_result_is_not_blocked_by_same_call_id_on_another_chain() {
    // Provider call IDs are exact correlation keys inside their causal context,
    // not repository-global IDs. Archived/copied sessions may retain the same
    // call ID on another visible source chain.
    let left_call = import_op(1, 1);
    let left_tool = tool_op_with_id(
        (1, 2, left_call.id),
        ("Read", "call-shared", ToolStage::Start),
    );
    let mut left_result = import_op(1, 3);
    left_result.parents = ParentSet::One(left_call.id);
    let left_finish = tool_op_with_id(
        (1, 4, left_result.id),
        ("", "call-shared", ToolStage::Finish),
    );

    let right_call = import_op(2, 1);
    let right_tool = tool_op_with_id(
        (2, 2, right_call.id),
        ("Read", "call-shared", ToolStage::Start),
    );

    let projection = HistoryProjection::from_ops(vec![
        left_call.clone(),
        left_tool,
        left_result.clone(),
        left_finish,
        right_call.clone(),
        right_tool,
    ]);

    assert_eq!(projection.nodes().len(), 2);
    assert_eq!(
        projection.visible_op_id(left_result.id),
        Some(left_call.id),
        "the sole causal parent disambiguates a reused call id"
    );
    assert_eq!(projection.visible_op_id(right_call.id), Some(right_call.id));
}

#[test]
fn meta_before_first_turn_stays_standalone() {
    // A META import before any real turn has no parent to bundle into — it must
    // survive as its own node so session header records aren't lost.
    let meta = meta_import_op(1, 1);
    let turn = import_op(1, 2);
    let projection = HistoryProjection::from_ops(vec![meta.clone(), turn]);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 2);
}

#[test]
fn collapse_keeps_git_commits() {
    // A raw import op plus a git commit. The commit must survive collapse.
    let op1 = import_op(1, 1);
    let mut projection = HistoryProjection::from_ops(vec![op1.clone()]);
    // Add a git commit.
    let mut bytes = [0u8; 32];
    bytes[0] = 9;
    projection.merge_git_commits(vec![GitCommitEntity {
        repository: RepositoryId(1),
        object_format: GitObjectFormat::Sha1,
        oid: GitOid::new(GitObjectFormat::Sha1, bytes),
        imported_record: None,
        availability: GitAvailability::Resolved,
        tree: GitOid::new(GitObjectFormat::Sha1, [0u8; 32]),
        parents: Vec::new(),
        author: editchain_core::GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: 0,
        },
        committer: editchain_core::GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: 0,
        },
        authored_at: 0,
        committed_at: 0,
        message: Payload::Empty,
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    }]);
    let nodes = projection.nodes();
    // One collapsed import + one git commit.
    assert_eq!(nodes.len(), 2);
    assert!(nodes
        .iter()
        .any(|n| matches!(n, editchain_project::HistoryNode::GitCommit(_))));
}

/// q6 Phase-1 gate: META records must never bundle across source chains.
///
/// Two chains (different `node` -> distinct `(OpId.node, OpId.boot)` keys).
/// Chain A has a real anchor then a META record (bundles into A). Chain B has a
/// META record with **no** real anchor in B — it must stay standalone, NOT
/// attach to chain A's anchor.
#[test]
fn no_cross_chain_meta_bundling() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    // Chain A: node 1, turn + META.
    let a_turn = import_op(1, 1);
    let a_msg = message_op(1, 2, a_turn.id, "hello");
    let mut a_meta = meta_import_op(1, 3);
    a_meta.parents = ParentSet::One(a_turn.id);
    // Chain B: node 2, only a META record (no real anchor in B).
    let b_meta = meta_import_op(2, 1);

    let projection = HistoryProjection::from_ops_with(
        vec![a_turn.clone(), a_msg, a_meta.clone(), b_meta.clone()],
        opts,
    );
    let nodes = projection.nodes();

    // Chain A's META bundles under A's turn -> A contributes 1 node.
    let a_key = a_turn.id.to_string();
    let a_node = nodes.iter().find(|n| n.node_key() == a_key).unwrap();
    match a_node {
        editchain_project::HistoryNode::CollapsedImport { sub_ops, .. } => {
            assert_eq!(sub_ops.len(), 1, "chain A's META must bundle into A's turn");
            assert_eq!(sub_ops[0].id, a_meta.id);
        }
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::ExecuteBundle { .. }
        | editchain_project::HistoryNode::PlanBundle { .. }
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    }

    // Chain B's META has no anchor in B -> must stay standalone (never attach to A).
    let b_key = b_meta.id.to_string();
    assert!(
        nodes.iter().any(|n| n.node_key() == b_key),
        "chain B's META must remain a standalone row, not bundle across to chain A"
    );
    // Exactly the A turn + the B META row (A's META is bundled away).
    assert_eq!(
        nodes.len(),
        2,
        "expected A turn + B standalone META, got {nodes:?}"
    );
}

/// q6 Phase-1 gate: metadata after a standalone (content) op is NOT dropped.
///
/// A META record that follows a dated content-bearing import bundles under it
/// when bundling is on; a META record after a standalone non-import op is still
/// anchored by that op. Neither is silently dropped.
#[test]
fn metadata_after_standalone_not_dropped() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    // A standalone op (a message not tied to an import), then a META op.
    let standalone = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::AGENT | Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"standalone content".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    let meta = meta_import_op(1, 2);

    let projection = HistoryProjection::from_ops_with(vec![standalone.clone(), meta.clone()], opts);
    let nodes = projection.nodes();

    // The META record must survive (its occurrence is never dropped), rendered as
    // its own row — not attached forward, not silently removed.
    let s_node = nodes
        .iter()
        .find(|n| n.node_key() == standalone.id.to_string())
        .expect("standalone op must remain a row");
    assert!(
        matches!(s_node, editchain_project::HistoryNode::EditOperation { .. }),
        "standalone content op must be an EditOperation row"
    );
    let meta_key = meta.id.to_string();
    let meta_node = nodes.iter().find(|n| n.node_key() == meta_key);
    assert!(
        meta_node.is_some(),
        "metadata after a standalone op must NOT be dropped (q6 ruling)"
    );
}

/// q6 Phase-1 gate: options are per-projection, not global.
///
/// The same ops projected with bundling off and on produce different row sets,
/// chosen purely by the option — no process-global state, so tests can run in
/// any order without a mutex.
#[test]
fn bundle_options_are_per_projection() {
    let turn1 = import_op(1, 1);
    let msg = message_op(1, 2, turn1.id, "hi");
    let mut meta = meta_import_op(1, 3);
    meta.parents = ParentSet::One(turn1.id);
    let ops = vec![turn1.clone(), msg, meta.clone()];

    let opts_off = editchain_project::ProjectionOptions {
        bundle_metadata: false,
    };
    let opts_on = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let off = HistoryProjection::from_ops_with(ops.clone(), opts_off).nodes();
    let on = HistoryProjection::from_ops_with(ops, opts_on).nodes();

    // Off: turn + META both top-level.
    assert!(off.iter().any(|n| n.node_key() == meta.id.to_string()));
    // On: META bundled under turn -> only the turn node remains.
    assert!(!on.iter().any(|n| n.node_key() == meta.id.to_string()));
    assert_eq!(on.len(), 1);
}

/// q6 Phase-1 regression: a child whose PARENT is a bundled META op must stay
/// connected to its source chain, NOT fragment into its own independent chain.
///
/// Simulates a producer-side sequence where a META record sits on the backbone
/// (its own META import), a real turn follows it, and a later real turn's causal
/// parent is the bundled META op (a `parent_keys` producer would see this across
/// the backbone). After bundling, the META op is a sub-op of the first anchor; the
/// later turn's stored parent still names the META op. Layout must lift that edge
/// onto the anchor so the two real turns share one chain.
#[test]
fn child_parented_to_bundled_meta_stays_connected() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    // turn0 is the anchor; meta is bundled under it; turn1's parent is `meta`.
    let turn0 = import_op(1, 1);
    let msg0 = message_op(1, 2, turn0.id, "first");
    let mut meta = meta_import_op(1, 3);
    meta.parents = ParentSet::One(turn0.id);
    let mut turn1 = import_op(1, 5);
    turn1.parents = ParentSet::One(meta.id);

    let projection = HistoryProjection::from_ops_with(
        vec![turn0.clone(), msg0, meta.clone(), turn1.clone()],
        opts,
    );
    let nodes = projection.nodes();

    // The META op is bundled (not a top-level row) — exactly two real rows.
    assert!(!nodes.iter().any(|n| n.node_key() == meta.id.to_string()));
    assert_eq!(nodes.len(), 2);

    // Its stored parent is still `meta` (never rewritten): the DR/immutability
    // contract that bundling does not splice parents.
    let turn1_node = nodes
        .iter()
        .find(|n| n.node_key() == turn1.id.to_string())
        .expect("turn1 present");
    let parents = turn1_node.parent_keys(
        &std::collections::BTreeMap::new(),
        &std::collections::HashMap::new(),
    );
    assert!(
        parents.contains(&meta.id.to_string()),
        "stored parent of turn1 must remain the bundled META op, got {parents:?}"
    );

    // The two real turns are ONE connected chain, demonstrated via the same
    // representative lift the projection's own layout/ordering uses: turn1 is
    // not an independent root. This is the exact "no fragmented small chains"
    // property the viewer needs.
    assert_eq!(
        projection.independent_chains(),
        1,
        "bundling must not fragment two real turns into independent chains"
    );
}

#[test]
fn legacy_untagged_token_usage_bundles_without_fragmenting_its_chain() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn0 = import_op(1, 1);
    let msg0 = message_op(1, 2, turn0.id, "first");
    let usage = legacy_token_usage_import_op(1, 3, turn0.id);
    assert_eq!(usage.tags, Tags::IMPORT, "fixture must model legacy tags");
    let mut turn1 = import_op(1, 4);
    turn1.parents = ParentSet::One(usage.id);
    let msg1 = message_op(1, 5, turn1.id, "second");

    let projection = HistoryProjection::from_ops_with(
        vec![turn0.clone(), msg0, usage.clone(), turn1.clone(), msg1],
        opts,
    );
    let nodes = projection.nodes();

    assert_eq!(nodes.len(), 2, "usage accounting must not become a row");
    assert!(!nodes
        .iter()
        .any(|node| node.node_key() == usage.id.to_string()));
    let anchor = nodes
        .iter()
        .find(|node| node.node_key() == turn0.id.to_string())
        .expect("preceding semantic row");
    assert_eq!(
        anchor.sub_ops().iter().map(|op| op.id).collect::<Vec<_>>(),
        vec![usage.id]
    );
    let later = nodes
        .iter()
        .find(|node| node.node_key() == turn1.id.to_string())
        .expect("following semantic row");
    assert_eq!(
        projection.lifted_parent_keys(later),
        vec![turn0.id.to_string()],
        "a child of folded usage metadata must reconnect to its exact anchor"
    );
    assert_eq!(projection.independent_chains(), 1);
}

#[test]
fn legacy_untagged_claude_transport_sidecars_bundle_exactly() {
    let options = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let anchor = import_op(1, 1);
    let anchor_message = message_op(1, 2, anchor.id, "before");
    let mut previous = anchor.id;
    let mut metadata = Vec::new();
    for (seq, record_type) in [
        (3, "atis-latch"),
        (4, "fork-context-ref"),
        (5, "file-history-snapshot"),
        (6, "file-history-delta"),
    ] {
        let mut op = import_op(1, seq);
        op.parents = ParentSet::One(previous);
        if let OpKind::Import(import) = &mut op.kind {
            import.raw_ref = Payload::Inline(
                serde_json::json!({ "type": record_type })
                    .to_string()
                    .into_bytes(),
            );
        }
        previous = op.id;
        metadata.push(op);
    }
    let mut continuation = import_op(1, 7);
    continuation.parents = ParentSet::One(previous);
    let continuation_message = message_op(1, 8, continuation.id, "after");
    let mut input_ops = vec![anchor.clone(), anchor_message];
    input_ops.extend(metadata.iter().cloned());
    input_ops.extend([continuation.clone(), continuation_message]);

    let projection = HistoryProjection::from_ops_with(input_ops, options);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 2);
    let anchor_row = nodes
        .iter()
        .find(|node| node.node_key() == anchor.id.to_string())
        .unwrap();
    assert_eq!(
        anchor_row
            .sub_ops()
            .iter()
            .map(|op| op.id)
            .collect::<Vec<_>>(),
        metadata.iter().map(|op| op.id).collect::<Vec<_>>()
    );
    let continuation_row = nodes
        .iter()
        .find(|node| node.node_key() == continuation.id.to_string())
        .unwrap();
    assert_eq!(
        projection.lifted_parent_keys(continuation_row),
        vec![anchor.id.to_string()]
    );
}

#[test]
fn malformed_token_usage_lookalike_stays_visible() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let anchor = import_op(1, 1);
    let mut lookalike = import_op(1, 2);
    lookalike.parents = ParentSet::One(anchor.id);
    if let OpKind::Import(import) = &mut lookalike.kind {
        import.raw_ref = Payload::Inline(
            br#"{"type":"token_usage_record","payload":{"turn_id":"turn-1"}}"#.to_vec(),
        );
    }

    let projection = HistoryProjection::from_ops_with(vec![anchor, lookalike.clone()], opts);

    assert!(projection
        .nodes()
        .iter()
        .any(|node| node.node_key() == lookalike.id.to_string()));
}

/// Reproduce the real q6 backbone pattern at small scale: a single stream with
/// alternating real turns and (source-time-unknown) META records. Each real turn
/// parents to the preceding META record. Bundling must keep the WHOLE chain as
/// one connected chain, not fragment it per turn.
#[test]
fn long_linear_chain_with_meta_breaks_stays_one_chain() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let mut entries: Vec<Op> = Vec::new();
    let mut prev: Option<OpId> = None;
    // Sequence: t1 (real), m2 (meta), t3 (real), m4 (meta), t5 (real), m6 (meta),
    // t7 (real). Each record parents to the previous line (the real importer's
    // linear backbone), so real turns parent to the preceding META record.
    for seq in 1..=7u64 {
        let is_meta = seq % 2 == 0;
        let id = OpId::new(NodeId(1), 0, seq);
        let mut record = Op {
            id,
            parents: prev.map_or(ParentSet::None, ParentSet::One),
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(10)),
            tags: Tags::IMPORT,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(format!("raw {seq}").into_bytes()),
                raw_hash: None,
            }),
        };
        if is_meta {
            record.tags |= Tags::META | Tags::SOURCE_TIME_UNKNOWN;
        }
        prev = Some(id);
        entries.push(record);
    }
    let projection = HistoryProjection::from_ops_with(entries, opts);
    // Exactly the 4 real turns remain (3 meta bundled away).
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 4);
    assert_eq!(
        projection.independent_chains(),
        1,
        "a single stream alternating real turns and bundled meta must be ONE chain, \
         got {} chains",
        projection.independent_chains()
    );
}

/// q6 Phase-1 regression: META that bundles into a tool-RESULT row must stay
/// connected after `group_tool_results` folds that result row into its call.
///
/// Reproduces the real q6 fragmentation: a META op anchors under a tool-result
/// row; `group_tool_results` then folds the result row away into the call, so a
/// later turn parented to that META would lose its edge. The representative map
/// must be re-pointed to the call so the chain stays one chain.
#[test]
fn meta_under_tool_result_stays_connected_after_grouping() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let call_import = import_op(1, 1);
    let call = tool_op(1, 2, call_import.id, "Bash"); // Start call
                                                      // Tool-result row (Finish, empty name), parented to the call's raw import.
    let mut result_import = import_op(1, 3);
    result_import.parents = ParentSet::One(call_import.id);
    let mut result_tool = tool_op(1, 4, result_import.id, "");
    if let OpKind::Tool(t) = &mut result_tool.kind {
        t.stage = ToolStage::Finish;
    }
    let result_tool_id = result_tool.id;
    // A META op that bundles under the tool-result row.
    let mut meta = meta_import_op(1, 5);
    meta.parents = ParentSet::One(result_import.id);
    // A later real turn parented to that META (the linear backbone).
    let mut next_turn = import_op(1, 6);
    next_turn.parents = ParentSet::One(meta.id);

    let projection = HistoryProjection::from_ops_with(
        vec![
            call_import.clone(),
            call,
            result_import.clone(),
            result_tool,
            meta.clone(),
            next_turn.clone(),
        ],
        opts,
    );
    let nodes = projection.nodes();
    // The tool-result row and the META are both folded away -> rows = call + turn.
    assert!(!nodes.iter().any(|n| n.node_key() == meta.id.to_string()));
    assert!(!nodes
        .iter()
        .any(|n| n.node_key() == result_import.id.to_string()));
    assert_eq!(nodes.len(), 2);
    let call_node = nodes
        .iter()
        .find(|node| node.node_key() == call_import.id.to_string())
        .unwrap();
    let sub_op_ids: Vec<_> = call_node.sub_ops().iter().map(|op| op.id).collect();
    assert_eq!(
        sub_op_ids,
        vec![result_tool_id, meta.id],
        "metadata bundled beneath a grouped tool result must move with it"
    );
    // The call + next_turn remain ONE connected chain, even though next_turn's
    // parent is the META that anchored under the folded tool result.
    assert_eq!(
        projection.independent_chains(),
        1,
        "META under a (then-grouped) tool result must not fragment the chain"
    );
}

#[test]
fn file_row_summary_uses_annotated_path_note() {
    // A Codex file item persists its provider-neutral path text as an explicit
    // `Explains` note targeting the file op; the collapsed row renders it as
    // `file: <path>` instead of the hashed `PathId` or a raw event label.
    let op1 = import_op(1, 1);
    let file = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(op1.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::FILE,
        kind: OpKind::File(editchain_core::op::FileOp {
            path: editchain_core::PathId(42),
            stage: editchain_core::op::FileStage::Applied,
            base: None,
            after: None,
            edit: editchain_core::op::FileEdit::None,
        }),
    };
    let mut path_note = Op {
        id: OpId::new(NodeId(1), 0, 3),
        parents: ParentSet::One(op1.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::NOTE,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![file.id],
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(b"/tmp/x.txt".to_vec()),
        }),
    };
    let projection =
        HistoryProjection::from_ops(vec![op1.clone(), file.clone(), path_note.clone()]);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes.first().unwrap().summary(), "file: /tmp/x.txt");

    // The annotation must resolve regardless of input order: the note can
    // precede the file op in the collapsed row's children, so a hash-ordered
    // importer emission cannot flip the summary between the annotated path and
    // the raw-record fallback.
    let projection =
        HistoryProjection::from_ops(vec![path_note.clone(), file.clone(), op1.clone()]);
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes.first().unwrap().summary(), "file: /tmp/x.txt");

    // Without the annotation (e.g. Claude attachment rows) the raw-record label
    // wins, so existing behavior is preserved.
    path_note.kind = OpKind::Note(NoteOp {
        target_ids: vec![],
        relationship: NoteRelationship::Explains,
        content: Payload::Inline(b"attachment=file".to_vec()),
    });
    let projection =
        HistoryProjection::from_ops(vec![op1.clone(), file.clone(), path_note.clone()]);
    let nodes = projection.nodes();
    assert_eq!(nodes.first().unwrap().summary(), "raw line 1");
}

#[test]
fn visible_op_id_resolves_folded_ops_to_their_rendered_row() {
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn = import_op(1, 1);
    let msg = message_op(1, 2, turn.id, "hello world");
    let mut meta = meta_import_op(1, 3);
    meta.parents = ParentSet::One(turn.id);
    // A tool call import with a Start tool child, plus a folded result pair.
    let call_import = import_op(2, 4);
    let call_tool = tool_op(2, 5, call_import.id, "Bash");
    let (result_import, result_tool) = tool_result_pair(2, 6, 7, call_import.id, Some("success"));
    let projection = HistoryProjection::from_ops_with(
        vec![
            turn.clone(),
            msg.clone(),
            meta.clone(),
            call_import.clone(),
            call_tool,
            result_import.clone(),
            result_tool,
        ],
        opts,
    );

    // Top-level rows resolve to themselves.
    assert_eq!(projection.visible_op_id(turn.id), Some(turn.id));
    assert_eq!(
        projection.visible_op_id(call_import.id),
        Some(call_import.id)
    );
    // A normalized child folded into its raw import resolves to the import row.
    assert_eq!(projection.visible_op_id(msg.id), Some(turn.id));
    // A bundled META sub-op resolves to its anchor row.
    assert_eq!(projection.visible_op_id(meta.id), Some(turn.id));
    // A tool-result import folded into its call resolves to the call row.
    assert_eq!(
        projection.visible_op_id(result_import.id),
        Some(call_import.id)
    );
    // Op ids absent from the projection resolve to nothing (never a phantom).
    let unknown = OpId::new(NodeId(99), 0, 99);
    assert_eq!(projection.visible_op_id(unknown), None);
}
