//! Tests for collapsing raw import ops with their normalized children.

#![expect(
    clippy::panic,
    reason = "Tests assert on enum variants with explicit panic arms"
)]

use editchain_core::{
    ActorId, Clock, GitAvailability, GitCommitEntity, GitObjectFormat, GitOid, ImportOp, MessageOp,
    NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, SessionId, Tags, ToolOp, ToolStage,
};
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
