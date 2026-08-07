//! Integration tests for the VS Code service workspace loading.

#![expect(
    clippy::arithmetic_side_effects,
    reason = "Test helper constructs timestamps with addition"
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
};
use editchain_vscode_service::Workspace;

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
    let window = ws.history_window(0, 10, false);
    assert_eq!(window.total, 2);
    assert_eq!(window.rows.len(), 2);
}
