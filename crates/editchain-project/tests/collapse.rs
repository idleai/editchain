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
        editchain_project::HistoryNode::EditOperation { .. }
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
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    };
    assert_eq!(author, "human");
}

#[test]
fn meta_imports_bundle_into_nearest_real_turn() {
    // A real turn (import + message child), then two META imports, then another
    // real turn. The META imports must bundle into the nearest preceding real
    // turn and not appear as their own nodes.
    let opts = editchain_project::ProjectionOptions {
        bundle_metadata: true,
    };
    let turn1 = import_op(1, 1);
    let msg = message_op(1, 2, turn1.id, "hello world");
    let meta1 = meta_import_op(1, 3);
    let meta2 = meta_import_op(1, 4);
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
        opts,
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
        | editchain_project::HistoryNode::GitCommit(_) => panic!("expected CollapsedImport"),
    }
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
    let meta = meta_import_op(1, 3);
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
    let a_meta = meta_import_op(1, 3);
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
    let meta = meta_import_op(1, 3);
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
    let meta = meta_import_op(1, 3);
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
    // The call + next_turn remain ONE connected chain, even though next_turn's
    // parent is the META that anchored under the folded tool result.
    assert_eq!(
        projection.independent_chains(),
        1,
        "META under a (then-grouped) tool result must not fragment the chain"
    );
}
