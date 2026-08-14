//! Integration tests for the VS Code service workspace loading.

#![expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "Test helper constructs timestamps with addition; tests index into known-length windows"
)]

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_codec as _;
use editchain_git as _;
use editchain_index as _;
use editchain_node as _;
use editchain_project as _;
use editchain_protocol as _;
use editchain_query as _;
use gix as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
    ToolOp, ToolStage,
};
use editchain_project::filter::ChainFilter;
use editchain_vscode_service::Workspace;

/// An empty filter that hides nothing (used to keep existing tests focused on
/// windowing rather than filtering).
fn no_filter() -> ChainFilter {
    ChainFilter::new(String::new(), String::new(), false, false)
}

fn msg_op(node: u64, seq: u64, text: &[u8]) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_000 + seq),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

#[test]
fn workspace_open_with_empty_chain() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // No chain dir and no git repo — should open with empty projection.
    let ws = Workspace::open(tmp.path().to_str().expect("utf8"), "").expect("open");
    assert!(ws.projection.is_empty());
}

#[test]
fn history_window_returns_rows() {
    // Build a projection directly with two ops.
    let ops = vec![msg_op(1, 1, b"first"), msg_op(1, 2, b"second")];
    let projection = editchain_project::HistoryProjection::from_ops(ops);
    let mut ws = Workspace::from_projection(projection);
    let window = ws.history_window(0, 10, false, &no_filter());
    assert_eq!(window.total, 2);
    assert_eq!(window.rows.len(), 2);
}

#[test]
fn op_rows_have_uniform_author_and_short_commit_id() {
    // A message op (MESSAGE tag only) should render a non-blank author label
    // ("system" fallback) and an abbreviated commit id (node:seq) rather than a
    // blank author and full node:boot:seq.
    let ops = vec![msg_op(7, 42, b"hello")];
    let projection = editchain_project::HistoryProjection::from_ops(ops);
    let mut ws = Workspace::from_projection(projection);
    let window = ws.history_window(0, 10, false, &no_filter());
    let row = &window.rows[0];
    assert_eq!(row.author, "system");
    assert_eq!(row.commit_id, "7:42");
}

#[test]
fn system_flag_marks_tool_and_import_ops() {
    // A tool op should be flagged is_system; a message op should not.
    let tool = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Inline(b"{}".to_vec()),
        }),
    };
    let msg = msg_op(1, 2, b"hello");
    let projection = editchain_project::HistoryProjection::from_ops(vec![tool, msg]);
    let mut ws = Workspace::from_projection(projection);
    let window = ws.history_window(0, 10, false, &no_filter());
    // Rows are newest-first; find by kind.
    let tool_row = window
        .rows
        .iter()
        .find(|r| r.kind == "tool")
        .expect("tool row");
    let msg_row = window
        .rows
        .iter()
        .find(|r| r.kind == "message")
        .expect("message row");
    assert!(tool_row.is_system);
    assert!(!msg_row.is_system);
}
