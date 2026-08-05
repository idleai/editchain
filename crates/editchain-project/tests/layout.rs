//! Tests for the lane layout module.

use editchain_core::{NodeId, OpId};
use editchain_project::layout::compute_lanes;

fn op(node: u64, seq: u64) -> OpId {
    OpId::new(NodeId(node), 0, seq)
}

#[test]
fn linear_history_single_lane() {
    // A -> B -> C (newest-first: C, B, A)
    let nodes = vec![op(1, 3), op(1, 2), op(1, 1)];
    let parents = |id: &OpId| {
        if id.seq == 3 {
            vec![op(1, 2)]
        } else if id.seq == 2 {
            vec![op(1, 1)]
        } else {
            Vec::new()
        }
    };
    let rows = compute_lanes(&nodes, parents);
    assert_eq!(rows.len(), 3);
    // All on lane 0.
    assert!(rows.iter().all(|r| r.lane == 0));
}

#[test]
fn branch_uses_two_lanes() {
    // C (merge of A and B) -> A, B (newest-first: C, B, A)
    let nodes = vec![op(1, 3), op(1, 2), op(1, 1)];
    let parents = |id: &OpId| {
        if id.seq == 3 {
            vec![op(1, 2), op(1, 1)]
        } else {
            Vec::new()
        }
    };
    let rows = compute_lanes(&nodes, parents);
    assert_eq!(rows.len(), 3);
    // The merge node and its two parents occupy distinct lanes.
    let lanes: std::collections::HashSet<usize> = rows.iter().map(|r| r.lane).collect();
    assert!(lanes.len() >= 2);
}
