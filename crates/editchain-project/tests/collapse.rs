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
use serde_json as _;

use editchain_core::{
    ActorId, Clock, GitAvailability, GitCommitEntity, GitObjectFormat, GitOid, ImportOp, MessageOp,
    NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, SessionId, Tags, ToolOp, ToolStage,
};

/// Build a metadata-only raw import op (tagged META).
fn meta_import_op(node: u64, seq: u64) -> Op {
    let mut op = import_op(node, seq);
    op.tags = Tags::IMPORT | Tags::META;
    op
}
use editchain_project::HistoryProjection;

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
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(name.as_bytes().to_vec()),
            stage: ToolStage::Start,
            content: Payload::Empty,
        }),
    }
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
        editchain_project::HistoryNode::EditOperation(_)
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
        editchain_project::HistoryNode::EditOperation(_)
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    };
    assert_eq!(author, "human");
}

#[test]
fn meta_imports_bundle_into_nearest_real_turn() {
    // A real turn (import + message child), then two META imports, then another
    // real turn. The META imports must bundle into the nearest preceding real
    // turn and not appear as their own nodes.
    // META bundling is off by default; enable it for this test and restore after.
    editchain_project::META_BUNDLE_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    let turn1 = import_op(1, 1);
    let msg = message_op(1, 2, turn1.id, "hello world");
    let meta1 = meta_import_op(1, 3);
    let meta2 = meta_import_op(1, 4);
    let turn2 = import_op(1, 5);
    let tool = tool_op(1, 6, turn2.id, "Bash");

    let projection = HistoryProjection::from_ops(vec![
        turn1.clone(),
        msg,
        meta1.clone(),
        meta2.clone(),
        turn2.clone(),
        tool,
    ]);
    let nodes = projection.nodes();
    editchain_project::META_BUNDLE_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);

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
        editchain_project::HistoryNode::EditOperation(_)
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    }
}

#[test]
fn meta_bundle_splices_children_to_keep_chain_continuous() {
    // A META import sits on the backbone between two real turns. When it is
    // bundled (dropped from the top-level list), the second turn's parent must
    // be re-pointed at the first turn — otherwise the chain severs.
    // META bundling is off by default; enable it for this test and restore after.
    editchain_project::META_BUNDLE_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    let turn1 = import_op(1, 1);
    let msg = message_op(1, 2, turn1.id, "hello world");
    let meta = meta_import_op(1, 3);
    // turn2's parent is the META op (it follows it on the backbone).
    let mut turn2 = import_op(1, 4);
    turn2.parents = ParentSet::One(meta.id);

    let projection =
        HistoryProjection::from_ops(vec![turn1.clone(), msg, meta.clone(), turn2.clone()]);
    let nodes = projection.nodes();
    editchain_project::META_BUNDLE_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);

    // Two real turns only (the META import is bundled into turn1).
    assert_eq!(nodes.len(), 2);
    // The newer turn (nodes[0]) must point at the older turn (nodes[1]) — NOT
    // at the dropped META op — so the chain stays continuous.
    let newer = &nodes[0];
    let older_key = nodes[1].node_key();
    let newer_parents = newer.parent_keys(
        &std::collections::BTreeMap::new(),
        &std::collections::HashMap::new(),
    );
    assert!(
        newer_parents.contains(&older_key),
        "newer turn should parent to older turn, got {newer_parents:?}"
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
        repository: editchain_core::RepositoryId(1),
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
