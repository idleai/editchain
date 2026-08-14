//! Tests for the lane layout module.

// Crate-level dependency marker (used by Cargo for feature resolution).
use regex as _;

use editchain_core::{NodeId, OpId};
use editchain_project::layout::{compute_graph_layout, compute_lanes, LayoutContext};
use editchain_project::HistoryProjection;

fn op(node: u64, seq: u64) -> OpId {
    OpId::new(NodeId(node), 0, seq)
}

/// Build a `compute_graph_layout` parents closure from a map of child → parents.
fn parents_from<'a>(map: &'a [(&'a str, &'a [&'a str])]) -> impl Fn(&str) -> Vec<String> + 'a {
    move |key: &str| {
        for (child, parents) in map {
            if *child == key {
                return parents.iter().map(ToString::to_string).collect();
            }
        }
        Vec::new()
    }
}

/// An `is_git` predicate that reports no git nodes (used by op-only tests).
fn no_git(_: &str) -> bool {
    false
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

#[test]
fn graph_layout_linear_single_lane() {
    // C -> B -> A (newest-first: C, B, A), all on one lane.
    let nodes = vec!["C".to_string(), "B".to_string(), "A".to_string()];
    let parents = parents_from(&[("C", &["B"]), ("B", &["A"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    assert_eq!(layout.rows.len(), 3);
    assert!(layout.rows.iter().all(|r| r.lane == 0));
    // Two edges: C→B and B→A.
    assert_eq!(layout.edges.len(), 2);
}

#[test]
fn graph_layout_merge_two_lanes() {
    // C (merge of A and B) -> A, B (newest-first: C, B, A).
    let nodes = vec!["C".to_string(), "B".to_string(), "A".to_string()];
    let parents = parents_from(&[("C", &["B", "A"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    assert_eq!(layout.rows.len(), 3);
    // Merge node and its two parents occupy distinct lanes.
    let lanes: std::collections::HashSet<usize> = layout.rows.iter().map(|r| r.lane).collect();
    assert!(lanes.len() >= 2);
    // Two edges from C (one to each parent).
    assert_eq!(layout.edges.len(), 2);
}

#[test]
fn graph_layout_edge_points_are_continuous() {
    // A long linear chain: E -> D -> C -> B -> A (newest-first).
    let nodes = vec![
        "E".to_string(),
        "D".to_string(),
        "C".to_string(),
        "B".to_string(),
        "A".to_string(),
    ];
    let parents = parents_from(&[("E", &["D"]), ("D", &["C"]), ("C", &["B"]), ("B", &["A"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);

    // Every edge's points must be contiguous: consecutive points differ by
    // exactly one row step, and the path starts at the child's row and ends at
    // the parent's row.
    for edge in &layout.edges {
        let pts = &edge.points;
        assert!(!pts.is_empty());
        let child_row = layout
            .rows
            .iter()
            .position(|r| r.node == edge.child)
            .expect("child in rows");
        let parent_row = layout
            .rows
            .iter()
            .position(|r| r.node == edge.parent)
            .expect("parent in rows");
        assert_eq!(pts.first().unwrap().row, child_row);
        assert_eq!(pts.last().unwrap().row, parent_row);
        for w in pts.windows(2) {
            let a = w.first().expect("window has two points");
            let b = w.get(1).expect("window has two points");
            assert_eq!(b.row, a.row + 1, "points must advance one row at a time");
        }
    }
}

/// Build a git commit entity with the given OID bytes and parent OIDs.
fn git_commit(oid_byte: u8, parent_bytes: &[u8]) -> editchain_core::GitCommitEntity {
    use editchain_core::{GitAvailability, GitObjectFormat, GitOid, GitSignature, Payload};
    let oid = |b: u8| {
        let mut bytes = [0u8; 32];
        bytes[0] = b;
        GitOid::new(GitObjectFormat::Sha1, bytes)
    };
    editchain_core::GitCommitEntity {
        repository: editchain_core::RepositoryId(1),
        object_format: GitObjectFormat::Sha1,
        oid: oid(oid_byte),
        imported_record: None,
        availability: GitAvailability::Resolved,
        tree: oid(0),
        parents: parent_bytes.iter().map(|&b| oid(b)).collect(),
        author: GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: 0,
        },
        committer: GitSignature {
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
    }
}

#[test]
fn graph_layout_topologically_sorts_git_commits() {
    // Three commits in a chain: C (oid 3) -> B (oid 2) -> A (oid 1).
    // Insert them into the projection in BTreeMap order (which is by OID, i.e.
    // A, B, C — NOT topological). `graph_layout` must re-sort so parents appear
    // below children.
    let mut projection = HistoryProjection::new();
    projection.merge_git_commits(vec![
        git_commit(3, &[2]), // C
        git_commit(1, &[]),  // A
        git_commit(2, &[1]), // B
    ]);

    let layout = projection.graph_layout();
    // Two edges: C->B and B->A.
    assert_eq!(layout.edges.len(), 2);

    // Every edge's parent must appear below its child in the layout rows.
    let idx: std::collections::HashMap<&str, usize> = layout
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.node.as_str(), i))
        .collect();
    for edge in &layout.edges {
        let child_i = idx
            .get(edge.child.as_str())
            .copied()
            .expect("child in layout rows");
        let parent_i = idx
            .get(edge.parent.as_str())
            .copied()
            .expect("parent in layout rows");
        assert!(
            parent_i > child_i,
            "parent {} must be below child {}",
            edge.parent,
            edge.child
        );
    }
}

/// Git commits must always occupy the leftmost lane (0), even when interleaved
/// with op chains.
#[test]
fn git_commits_occupy_leftmost_lane() {
    // Newest-first: G2 (git), O2 (op), G1 (git), O1 (op). Git nodes G1/G2 form
    // one component; ops O1/O2 form another. Git must land on lane 0, ops on
    // lane >= 1.
    let nodes = vec![
        "G2".to_string(),
        "O2".to_string(),
        "G1".to_string(),
        "O1".to_string(),
    ];
    let parents = parents_from(&[("G2", &["G1"]), ("O2", &["O1"])]);
    let is_git = |k: &str| -> bool { k.starts_with('G') };
    let layout = compute_graph_layout(&nodes, parents, &is_git);
    let lane_of = |k: &str| layout.rows.iter().find(|r| r.node == k).unwrap().lane;
    assert_eq!(lane_of("G2"), 0, "git commit should be on lane 0");
    assert_eq!(lane_of("G1"), 0, "git commit should be on lane 0");
    assert_ne!(
        lane_of("O2"),
        0,
        "op chain should not collide with git lane"
    );
    assert_ne!(
        lane_of("O1"),
        0,
        "op chain should not collide with git lane"
    );
}

#[test]
fn graph_layout_breaks_cycles_deterministically() {
    // A cycle: A -> B -> C -> A (each node's parent is the next in the ring).
    // `compute_graph_layout` must break the cycle deterministically rather than
    // dropping nodes or emitting them unsorted, so every node still appears.
    let nodes = vec!["A".to_string(), "B".to_string(), "C".to_string()];
    let parents = parents_from(&[("A", &["C"]), ("B", &["A"]), ("C", &["B"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);

    // All three nodes must be present (none dropped).
    assert_eq!(layout.rows.len(), 3);
    // Every node must appear exactly once.
    let mut seen = std::collections::HashSet::new();
    for r in &layout.rows {
        assert!(seen.insert(r.node.clone()), "duplicate node {}", r.node);
    }
}

#[test]
fn edges_for_window_emits_edge_entering_from_above() {
    // A long edge N0 -> N3 spanning several rows (newest-first: N0..N3).
    // When scrolling to a window that contains only N3 (the parent) but not N0
    // (the child above it), the edge must still be emitted so the line enters
    // from the top of the viewport instead of vanishing.
    let nodes = vec![
        "N0".to_string(),
        "N1".to_string(),
        "N2".to_string(),
        "N3".to_string(),
    ];
    let parents = parents_from(&[("N0", &["N3"])]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);

    // Window covering rows 0..1 (N0,N1): child N0 inside -> emitted normally.
    let edges = ctx.edges_for_window(0, 2);
    assert!(
        edges.iter().any(|e| e.child == "N0" && e.parent == "N3"),
        "edge N0->N3 should be emitted when its child is in the window"
    );

    // Window covering rows 2..4 (N2,N3): only the PARENT N3 is inside; N0 is
    // above it. The edge must still be emitted so it enters from offscreen.
    let edges2 = ctx.edges_for_window(2, 2);
    assert!(
        edges2.iter().any(|e| e.child == "N0" && e.parent == "N3"),
        "edge N0->N3 should be emitted when its parent is in the window even if its child is above"
    );
}

// ---------------------------------------------------------------------------
// Lane reuse across disconnected chains
// ---------------------------------------------------------------------------

/// Two disconnected linear chains that never overlap in time share one lane.
///
/// Chain A: A1 -> A2 (rows 2..3). Chain B: B1 -> B2 (rows 0..1, newest).
/// Newest-first display order: B2, B1, A2, A1. Because A's rows (2..3) and B's
/// rows (0..1) are disjoint, both chains should land on the same base lane.
#[test]
fn disconnected_non_overlapping_chains_share_lane() {
    let nodes = vec![
        "B2".to_string(),
        "B1".to_string(),
        "A2".to_string(),
        "A1".to_string(),
    ];
    let parents = parents_from(&[("B2", &["B1"]), ("A2", &["A1"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    let lane_of = |k: &str| layout.rows.iter().find(|r| r.node == k).unwrap().lane;
    assert_eq!(
        lane_of("B2"),
        lane_of("A2"),
        "disjoint chains should share a lane"
    );
    assert_eq!(lane_of("B1"), lane_of("A1"));
}

/// Two disconnected chains that overlap in time get different lanes.
///
/// Interleaved newest-first rows: A2(0), B2(1), A1(2), B1(3). Chain A spans
/// rows 0..2 and chain B spans rows 1..3 — they overlap in the middle, so they
/// must NOT share a lane.
#[test]
fn disconnected_overlapping_chains_get_distinct_lanes() {
    let nodes = vec![
        "A2".to_string(),
        "B2".to_string(),
        "A1".to_string(),
        "B1".to_string(),
    ];
    let parents = parents_from(&[("A2", &["A1"]), ("B2", &["B1"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    let lane_of = |k: &str| layout.rows.iter().find(|r| r.node == k).unwrap().lane;
    assert_ne!(
        lane_of("A2"),
        lane_of("B2"),
        "overlapping chains need distinct lanes"
    );
}

/// Three sequential chains that each fully end before the next begins all
/// reuse a single freed lane rather than each claiming a fresh column.
///
/// Newest-first rows: C(0..1), B(2..3), A(4..5). All three intervals are
/// disjoint, so greedy interval coloring packs them onto one base column —
/// this is exactly the "reuse freed lanes" behavior for sequential sessions.
#[test]
fn ended_chain_lane_is_reused_by_later_chain() {
    let nodes = vec![
        "C2".to_string(),
        "C1".to_string(),
        "B2".to_string(),
        "B1".to_string(),
        "A2".to_string(),
        "A1".to_string(),
    ];
    let parents = parents_from(&[("C2", &["C1"]), ("B2", &["B1"]), ("A2", &["A1"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    let lane_of = |k: &str| layout.rows.iter().find(|r| r.node == k).unwrap().lane;
    // All three disjoint chains pack onto a single base column.
    assert_eq!(
        lane_of("A2"),
        lane_of("C2"),
        "A and C should reuse the same lane"
    );
    assert_eq!(lane_of("B2"), lane_of("A2"), "B also reuses the freed lane");
    // And no chain takes a fresh second column.
    let distinct: std::collections::HashSet<usize> = layout.rows.iter().map(|r| r.lane).collect();
    assert_eq!(
        distinct.len(),
        1,
        "all three disjoint chains share one column"
    );
}

/// Reuse must not break merges: a merge node's two parents still get distinct
/// lanes even when the merge component shares a base column with another chain.
#[test]
fn reuse_preserves_merge_two_lanes() {
    // Merge component M (merge of X and Y) at rows 0..1; disjoint chain Z at
    // rows 3..4. M's parents X and Y must occupy distinct lanes.
    let nodes = vec![
        "M".to_string(),
        "Y".to_string(),
        "X".to_string(),
        "Z".to_string(),
    ];
    let parents = parents_from(&[("M", &["X", "Y"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    let lane_of = |k: &str| layout.rows.iter().find(|r| r.node == k).unwrap().lane;
    assert_ne!(
        lane_of("X"),
        lane_of("Y"),
        "merge parents need distinct lanes"
    );
}
