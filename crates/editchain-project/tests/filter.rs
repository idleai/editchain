//! Tests for general chain filtering with truncation.

#![expect(
    clippy::indexing_slicing,
    reason = "Tests index into known-length parent vectors"
)]
// Crate-level dependency marker (used by Cargo for feature resolution).
use regex as _;

use editchain_core::{
    ActorId, Clock, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
};
use editchain_project::filter::ChainFilter;
use editchain_project::HistoryProjection;

/// Build a message op with a given clock and parent.
fn msg_op(node: u64, seq: u64, clock_ms: u64, parent: Option<OpId>, text: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.as_bytes().to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// Build a linear chain of message ops: `a -> b -> c` (a is oldest/root).
fn linear_chain() -> Vec<Op> {
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let b = msg_op(1, 2, 2_000, Some(a.id), "beta");
    let c = msg_op(1, 3, 3_000, Some(b.id), "gamma");
    vec![a, b, c]
}

#[test]
fn empty_filter_keeps_all_nodes() {
    let projection = HistoryProjection::from_ops(linear_chain());
    let filter = ChainFilter::new(String::new(), String::new(), false, false);
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 3);
}

#[test]
fn hide_undated_removes_clock_zero_nodes() {
    // b has clock 0 (undated); a and c are dated.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let b = msg_op(1, 2, 0, Some(a.id), "beta");
    let c = msg_op(1, 3, 3_000, Some(b.id), "gamma");
    let projection = HistoryProjection::from_ops(vec![a, b, c]);

    // With splice off, the undated node is dropped but edges are NOT reconnected.
    let filter = ChainFilter::new(String::new(), String::new(), true, false);
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
    // The kept nodes are a and c; c's parent still points at the hidden b.
    let c_node = nodes
        .iter()
        .find(|n| n.summary() == "gamma")
        .expect("gamma kept");
    assert_eq!(c_node.parent_keys(&projection.git.links).len(), 1);
}

#[test]
fn hide_undated_with_splice_reconnects_edges() {
    // b has clock 0 (undated); a and c are dated. With splice on, c's parent
    // should be rewritten to a (skipping the hidden b).
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let b = msg_op(1, 2, 0, Some(a.id), "beta");
    let c = msg_op(1, 3, 3_000, Some(b.id), "gamma");
    let a_id = a.id;
    let projection = HistoryProjection::from_ops(vec![a, b, c]);

    let filter = ChainFilter::new(String::new(), String::new(), true, true);
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
    // c's parent should now be a (the nearest kept ancestor).
    let c_node = nodes
        .iter()
        .find(|n| n.summary() == "gamma")
        .expect("gamma kept");
    let parents = c_node.parent_keys(&projection.git.links);
    assert_eq!(parents.len(), 1);
    assert_eq!(parents[0], a_id.to_string());
}

#[test]
fn hide_undated_removes_undated_leaf_nodes() {
    // A dated root with an undated leaf child (e.g. a `last-prompt` record).
    // The undated leaf must be hidden even though it has no children (it is an
    // endpoint) — it is junk metadata with no chain position to anchor.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let leaf = msg_op(1, 2, 0, Some(a.id), "last-prompt");
    let projection = HistoryProjection::from_ops(vec![a.clone(), leaf]);

    let filter = ChainFilter::new(String::new(), String::new(), true, true);
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].summary(), "alpha");
}

#[test]
fn summary_pattern_hides_matching_intermediate_nodes() {
    // Hide the middle node by summary; endpoints stay.
    let projection = HistoryProjection::from_ops(linear_chain());
    let filter = ChainFilter::new("beta".to_string(), String::new(), false, true);
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
    // The kept nodes are alpha and gamma; gamma's parent is alpha.
    let gamma = nodes
        .iter()
        .find(|n| n.summary() == "gamma")
        .expect("gamma kept");
    assert_eq!(
        gamma.parent_keys(&projection.git.links),
        vec![projection.ops[0].id.to_string()]
    );
}

#[test]
fn endpoints_are_preserved_even_when_matching() {
    // Both endpoints match the pattern but must still be kept (they have no
    // parent / no child in the full graph).
    let projection = HistoryProjection::from_ops(linear_chain());
    // Pattern matches alpha (root) and gamma (leaf) but not beta.
    let filter = ChainFilter::new("alpha|gamma".to_string(), String::new(), false, true);
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 3);
}

#[test]
fn kind_pattern_hides_matching_nodes() {
    // A chain where the middle node is a tool; hide by kind.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let tool = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(a.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(editchain_core::ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: editchain_core::ToolStage::Start,
            content: Payload::Empty,
        }),
    };
    let c = msg_op(1, 3, 3_000, Some(tool.id), "gamma");
    let projection = HistoryProjection::from_ops(vec![a.clone(), tool.clone(), c.clone()]);

    let filter = ChainFilter::new(String::new(), "tool".to_string(), false, true);
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
}
