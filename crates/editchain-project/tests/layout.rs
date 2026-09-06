//! Tests for the lane layout module.

// Crate-level dependency markers (used by Cargo for feature resolution).
use regex as _;
use serde as _;
use serde_json as _;

use editchain_core::{NodeId, OpId};
use editchain_project::layout::{
    compute_graph_layout, compute_lanes, GridPoint, LaneEdge, LayoutContext,
};
use editchain_project::{HistoryNode, HistoryProjection};

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

/// Assert that a set of lane ids is contiguous: exactly `0..=max` are used.
/// Compaction must never leave a hole at a collapsed lane index.
fn assert_lanes_are_dense(lanes: &[usize]) {
    let max = lanes.iter().copied().max().unwrap_or(0);
    let used: std::collections::HashSet<usize> = lanes.iter().copied().collect();
    assert_eq!(
        used.len(),
        max.saturating_add(1),
        "lane ids must be dense from 0..={max}: used={used:?}"
    );
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
fn graph_layout_branch_out_distinct_lanes() {
    // A fork without a merge: A (root) has two children B and C. The two
    // branches must occupy distinct lanes so the fork renders as two lines
    // diverging from the shared root rather than collapsing onto one column.
    let nodes = vec!["C".to_string(), "B".to_string(), "A".to_string()];
    let parents = parents_from(&[("B", &["A"]), ("C", &["A"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    let lane_of = |k: &str| layout.rows.iter().find(|r| r.node == k).unwrap().lane;
    assert_ne!(
        lane_of("B"),
        lane_of("C"),
        "the two fork branches must occupy distinct lanes"
    );
}

#[test]
fn graph_layout_fork_then_merge_diamond_distinct_lanes() {
    // A fork-then-merge diamond: A (root) forks into B and C, which merge into
    // D. Both branches inherit A's lane, so without reassignment they'd share
    // one column and the merge would be invisible. The two branches must land
    // on distinct lanes so the merge jog renders.
    let nodes = vec![
        "D".to_string(),
        "C".to_string(),
        "B".to_string(),
        "A".to_string(),
    ];
    let parents = parents_from(&[("D", &["B", "C"]), ("B", &["A"]), ("C", &["A"])]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    let lane_of = |k: &str| layout.rows.iter().find(|r| r.node == k).unwrap().lane;
    assert_ne!(
        lane_of("B"),
        lane_of("C"),
        "the two diamond branches must occupy distinct lanes"
    );
    // The merge node sits on one of the branch lanes (the first parent's).
    assert_eq!(lane_of("D"), lane_of("B"));
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
    use editchain_core::{GitAvailability, GitObjectFormat, GitOid, GitSignature};
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

/// Wrap a git commit entity as an imported op so `from_ops` projects it
/// directly (the path `merge_git_commits` is designed to skip).
fn git_commit_op(node: u64, seq: u64, commit: editchain_core::GitCommitEntity) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::IMPORT,
        kind: OpKind::GitCommit(Box::new(commit)),
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

/// A projection built directly from ops must keep valid Git parent edges in
/// `lifted_parent_keys` without any prior `merge_git_commits` call.
#[test]
fn lifted_git_parent_edge_survives_direct_from_ops_projection() {
    // Child (oid 3) -> parent (oid 2). Both commits are supplied as ops, so
    // `from_ops` reduces them into `git.commits` at construction time.
    let parent_commit = git_commit(2, &[]);
    let child_commit = git_commit(3, &[2]);
    let parent_key = parent_commit.oid.to_hex();
    let child_key = child_commit.oid.to_hex();

    let projection = HistoryProjection::from_ops(vec![
        git_commit_op(1, 1, child_commit),
        git_commit_op(2, 1, parent_commit),
    ]);

    let child_node = projection
        .nodes()
        .into_iter()
        .find(|node| node.node_key() == child_key)
        .expect("child commit node is projected");
    assert_eq!(
        projection.lifted_parent_keys(&child_node),
        vec![parent_key],
        "a valid Git parent edge must survive a direct from_ops projection \
         without a merge_git_commits call"
    );
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

#[test]
fn edges_for_window_emits_edge_for_parent_at_window_top() {
    // The exact scroll boundary: a merge child M sits immediately above the
    // window (row `offset - 1`) and its parent Y is the first visible row
    // (row `offset`). The parent-emitting pass used to start at `offset + 1`,
    // so a parent on the window-top row never had its offscreen child's edge
    // emitted and the line entering from above vanished at that boundary.
    //
    // Newest-first rows: M(0), Y(1), X(2). M merges X (base lane) and Y (extra
    // lane), so the M->Y edge jogs lanes.
    let nodes = vec!["M".to_string(), "Y".to_string(), "X".to_string()];
    let parents = parents_from(&[("M", &["X", "Y"])]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);
    let m_lane = *ctx.lane_at.get("M").expect("merge child lane");
    let y_lane = *ctx.lane_at.get("Y").expect("merge parent lane");
    assert_ne!(
        m_lane, y_lane,
        "merge child and its extra parent must occupy distinct lanes"
    );

    // Window covering exactly row 1 (Y, the first visible row): M is one row
    // above it, so the edge must still be emitted, entering from offscreen.
    let edges = ctx.edges_for_window(1, 1);
    let boundary: Vec<_> = edges
        .iter()
        .filter(|e| e.child == "M" && e.parent == "Y")
        .collect();
    assert_eq!(
        boundary.len(),
        1,
        "edge M->Y must be emitted exactly once for the boundary window"
    );
    assert_eq!(
        boundary
            .first()
            .expect("boundary edge emitted exactly once")
            .points,
        vec![
            GridPoint {
                row: 1,
                lane: m_lane
            },
            GridPoint {
                row: 1,
                lane: y_lane
            },
        ],
        "clamped edge must enter at the window top and jog onto the parent's lane"
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
    let lanes: Vec<usize> = layout.rows.iter().map(|r| r.lane).collect();
    assert_ne!(
        lane_of("A2"),
        lane_of("B2"),
        "overlapping chains need distinct lanes"
    );
    assert_lanes_are_dense(&lanes);
}

/// A disconnected component must reserve the full width of an active fork,
/// not only its base lane. The short B chain renders while A's two-lane diamond
/// is still open; B therefore needs a third lane instead of colliding with A's
/// branch lane.
#[test]
fn overlapping_component_does_not_collide_with_active_fork_lane() {
    // Newest-first rows:
    //   M(0), right(1), B2(2), B1(3), left(4), root(5)
    // A is the diamond M -> [right, left] -> root and spans rows 0..5.
    // B is disconnected but lies inside that span at rows 2..3.
    let nodes = vec![
        "M".to_string(),
        "right".to_string(),
        "B2".to_string(),
        "B1".to_string(),
        "left".to_string(),
        "root".to_string(),
    ];
    let parents = parents_from(&[
        ("M", &["right", "left"]),
        ("right", &["root"]),
        ("left", &["root"]),
        ("B2", &["B1"]),
    ]);
    let layout = compute_graph_layout(&nodes, parents, &no_git);
    let lane_of = |key: &str| layout.rows.iter().find(|row| row.node == key).unwrap().lane;
    let fork_lanes: std::collections::HashSet<usize> =
        [lane_of("right"), lane_of("left")].into_iter().collect();

    assert_eq!(fork_lanes.len(), 2, "diamond branches need two lanes");
    assert!(
        !fork_lanes.contains(&lane_of("B2")),
        "the nested disconnected chain must not reuse an active fork lane"
    );
    assert_eq!(lane_of("B2"), lane_of("B1"));
    assert_eq!(
        layout.rows.iter().map(|row| row.lane).max(),
        Some(2),
        "two active fork lanes plus one disconnected chain lane"
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

// ---------------------------------------------------------------------------
// Lane reuse inside ONE connected component
// ---------------------------------------------------------------------------

/// N sequential operation branches explicitly linked to successive commits in
/// one Git chain still reuse lanes after each branch ends. Without in-component
/// compaction, every branch root claims a permanent fresh lane and `max_lane`
/// grows linearly with the branch count.
#[test]
fn sequential_explicit_git_links_reuse_operation_lanes() {
    let n = 8usize;
    let mut nodes: Vec<String> = Vec::new();
    let mut parents: Vec<(String, Vec<String>)> = Vec::new();
    // Newest-first display order: each branch's newest op sits directly above
    // its linked commit, with the commit chain below the oldest session.
    for s in (0..n).rev() {
        let b = format!("s{s}b"); // newest op of session s, linked to git
        let g = format!("g{s}");
        let a = format!("s{s}a"); // oldest op of session s
        nodes.push(b.clone());
        nodes.push(g.clone());
        nodes.push(a.clone());
        parents.push((b.clone(), vec![a.clone(), g.clone()]));
        parents.push((
            g.clone(),
            if s == 0 {
                Vec::new()
            } else {
                vec![format!("g{}", s - 1)]
            },
        ));
        parents.push((a.clone(), Vec::new()));
    }
    let is_git = |k: &str| -> bool { k.starts_with('g') };
    let parents_of = |k: &str| -> Vec<String> {
        parents
            .iter()
            .find(|(c, _)| c == k)
            .map(|(_, ps)| ps.clone())
            .unwrap_or_default()
    };
    let ctx = LayoutContext::new(&nodes, &parents_of, &is_git);
    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    let max_lane = ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0);
    let lanes: Vec<usize> = ctx.lanes.iter().map(|r| r.lane).collect();
    // Everything is one component (each branch reaches the shared Git chain),
    // yet the sequential branches must pack onto one reusable op lane.
    assert_eq!(
        max_lane, 1,
        "sequential branches linked to a shared Git chain must reuse one op lane"
    );
    assert_lanes_are_dense(&lanes);
    for key in &nodes {
        let expected = usize::from(!is_git(key));
        assert_eq!(
            lane_of(key),
            expected,
            "{key} must land on the expected lane (git=0, ops=1)"
        );
    }
}

/// Repeated sequential fork diamonds inside one session reuse the freed branch
/// lane: diamond k's branches render on the same two columns as diamond 0's
/// instead of claiming a fresh lane per diamond.
#[test]
fn sequential_fork_diamonds_reuse_the_freed_branch_lane() {
    let diamonds = 10usize;
    let mut nodes: Vec<String> = Vec::new();
    let mut parents: Vec<(String, Vec<String>)> = Vec::new();
    for d in (0..diamonds).rev() {
        let c1 = format!("d{d}c1");
        let c2 = format!("d{d}c2");
        let m = format!("d{d}m");
        // Diamond d forks off m_{d-1} (or R) and merges back at m_d. The first
        // parent of the merge is the trunk branch so the merge returns to the
        // trunk lane; the fork branch is the secondary parent.
        let fork = if d == 0 {
            "R".to_string()
        } else {
            format!("d{}m", d - 1)
        };
        parents.push((c1.clone(), vec![fork.clone()]));
        parents.push((c2.clone(), vec![fork]));
        parents.push((m.clone(), vec![c2.clone(), c1.clone()]));
        // Newest-first display order: the merge row, then its two branches,
        // then the previous merge below.
        nodes.push(m.clone());
        nodes.push(c2.clone());
        nodes.push(c1.clone());
    }
    nodes.push("R".to_string());
    parents.push(("R".to_string(), Vec::new()));
    let parents_of = |k: &str| -> Vec<String> {
        parents
            .iter()
            .find(|(c, _)| c == k)
            .map(|(_, ps)| ps.clone())
            .unwrap_or_default()
    };
    let ctx = LayoutContext::new(&nodes, &parents_of, &no_git);
    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    let max_lane = ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0);
    let lanes_used: std::collections::HashSet<usize> = ctx.lanes.iter().map(|r| r.lane).collect();
    let lanes: Vec<usize> = ctx.lanes.iter().map(|r| r.lane).collect();
    assert_eq!(
        max_lane, 1,
        "sequential fork diamonds must reuse one branch lane, not one per diamond"
    );
    assert_eq!(
        lanes_used.len(),
        2,
        "exactly two columns: the trunk lane and one reused fork lane"
    );
    assert_lanes_are_dense(&lanes);
    for d in 0..diamonds {
        let c1 = format!("d{d}c1");
        let c2 = format!("d{d}c2");
        let m = format!("d{d}m");
        // The two concurrent branches of EVERY diamond still render on distinct
        // lanes (overlapping branch activity never collapses onto one lane).
        assert_ne!(
            lane_of(&c1),
            lane_of(&c2),
            "diamond {d} branches must stay on distinct lanes"
        );
        // And the merge returns to the trunk lane.
        assert_eq!(
            lane_of(&m),
            lane_of(&c2),
            "diamond {d} merge must return to the trunk lane"
        );
    }
}

/// Compaction must renumber the surviving lanes densely even when an
/// INTERMEDIATE lane merges while a later lane cannot. Three sessions (A, B,
/// C) linked to one git chain: A's op lane (rows 0..2) and B's op lane (rows
/// 4..6) are disjoint in time, so B merges into A's lane; C's op lane spans
/// rows 1..7 and overlaps the merged span, so C cannot merge. The survivors
/// would be lanes {0, 2, 3} (a gap at 1) unless they are renumbered — and the
/// git-leftmost shift would then leave an even wider hole.
///
/// Newest-first rows: A2(0), C2(1), A1(2), G2(3), B2(4), B1(5), C1(6), G1(7),
/// G0(8). Sessions: A2 -> [A1, G2], B2 -> [B1, G1], C2 -> [C1, G0]; git chain
/// G2 -> G1 -> G0.
#[test]
fn intermediate_lane_merges_but_later_lane_cannot_densifies_survivors() {
    let nodes = vec![
        "A2".to_string(),
        "C2".to_string(),
        "A1".to_string(),
        "G2".to_string(),
        "B2".to_string(),
        "B1".to_string(),
        "C1".to_string(),
        "G1".to_string(),
        "G0".to_string(),
    ];
    let parents_of = parents_from(&[
        ("A2", &["A1", "G2"]),
        ("A1", &[]),
        ("B2", &["B1", "G1"]),
        ("B1", &[]),
        ("C2", &["C1", "G0"]),
        ("C1", &[]),
        ("G2", &["G1"]),
        ("G1", &["G0"]),
        ("G0", &[]),
    ]);
    let is_git = |k: &str| -> bool { k.starts_with('G') };
    let ctx = LayoutContext::new(&nodes, &parents_of, &is_git);
    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    let max_lane = ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0);
    let lanes: Vec<usize> = ctx.lanes.iter().map(|r| r.lane).collect();
    // A and B merged onto one reusable op lane; C (overlapping B) kept its own.
    assert_eq!(
        lane_of("B1"),
        lane_of("A1"),
        "B's disjoint op lane must merge into A's lane"
    );
    assert_ne!(
        lane_of("C1"),
        lane_of("A1"),
        "C's overlapping op lane must stay distinct"
    );
    assert_eq!(max_lane, 2, "git=0 plus two dense op lanes");
    assert_lanes_are_dense(&lanes);
    assert_eq!(lane_of("A1"), 1);
    assert_eq!(lane_of("B1"), 1);
    assert_eq!(lane_of("C1"), 2);
    assert_eq!(lane_of("G0"), 0);
    assert_eq!(lane_of("G2"), 0);
}

/// Pass-through must reflect EXACT geometry runs, not per-component min/max
/// occupancy: after compaction merges two disjoint branch runs onto one lane,
/// a window inside the gap between them must NOT draw a false vertical line,
/// while a genuinely continuous segment crossing the window still draws one.
///
/// Uses the same three-session graph as
/// `intermediate_lane_merges_but_later_lane_cannot_densifies_survivors`:
/// final lanes are git=0, merged ops A+B=1, op C=2. Lane 1 has real segments
/// at rows 0..2 and 4..6 with a gap at row 3; lane 0 carries the continuous
/// git chain G2(3) -> G1(7) spanning rows 3..7.
#[test]
fn pass_through_skips_gap_between_merged_lane_runs_but_crosses_real_segment() {
    let nodes = vec![
        "A2".to_string(),
        "C2".to_string(),
        "A1".to_string(),
        "G2".to_string(),
        "B2".to_string(),
        "B1".to_string(),
        "C1".to_string(),
        "G1".to_string(),
        "G0".to_string(),
    ];
    let parents_of = parents_from(&[
        ("A2", &["A1", "G2"]),
        ("A1", &[]),
        ("B2", &["B1", "G1"]),
        ("B1", &[]),
        ("C2", &["C1", "G0"]),
        ("C1", &[]),
        ("G2", &["G1"]),
        ("G1", &["G0"]),
        ("G0", &[]),
    ]);
    let is_git = |k: &str| -> bool { k.starts_with('G') };
    let ctx = LayoutContext::new(&nodes, &parents_of, &is_git);
    // Window at row 3: the gap between lane 1's merged runs [0,2] and [4,6].
    let edges = ctx.edges_for_window(3, 1);
    assert!(
        !edges
            .iter()
            .any(|e| e.child.starts_with("__pass_through_1")),
        "must not draw a pass-through line inside the merged lane's gap"
    );
    // Window rows 4..5: the git chain's continuous same-lane run [2,8] crosses
    // with no git node inside the window — the sparse-chain line must still be
    // drawn (and the merged op lane has its nodes inside, so no second line).
    let edges = ctx.edges_for_window(4, 2);
    assert!(
        edges
            .iter()
            .any(|e| e.child.starts_with("__pass_through_0")),
        "continuous segment crossing the window must draw a pass-through line"
    );
    assert!(
        !edges
            .iter()
            .any(|e| e.child.starts_with("__pass_through_1")),
        "no pass-through on the op lane whose nodes are inside the window"
    );
}

/// A lane must NEVER be reused across overlapping branch activity: a fork
/// branch that is still live while a later sibling branch renders keeps its own
/// column even though the first branch started earlier. This guards the
/// compaction's row-span disjointness invariant against unsafe merges.
#[test]
fn concurrent_branch_overlap_never_reuses_a_lane() {
    // Newest-first: Y(0), M(1), X(2), C2(3), C1(4), R(5).
    //   R forks into trunk C1 and long branch C2 -> X; C1 forks again into M
    //   (merging X back) and Y, whose edge to C1 overlaps the C2/X branch rows.
    let nodes = vec![
        "Y".to_string(),
        "M".to_string(),
        "X".to_string(),
        "C2".to_string(),
        "C1".to_string(),
        "R".to_string(),
    ];
    let parents = parents_from(&[
        ("C1", &["R"]),
        ("C2", &["R"]),
        ("X", &["C2"]),
        ("M", &["C1", "X"]),
        ("Y", &["C1"]),
    ]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);
    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    // The long C2->X branch and the Y/M branch overlap in display rows, so they
    // must NOT be merged onto one lane even though both reuse compaction.
    assert_ne!(
        lane_of("X"),
        lane_of("Y"),
        "overlapping branches must not share a lane"
    );
    assert_ne!(
        lane_of("X"),
        lane_of("M"),
        "overlapping branches must not share a lane"
    );
    // M forks off C1 (Y already occupies C1's lane), so M and Y are two
    // concurrent children of C1 whose edges overlap rows — they stay distinct.
    assert_ne!(
        lane_of("M"),
        lane_of("Y"),
        "concurrent children of C1 with overlapping edges must stay distinct"
    );
    assert_ne!(lane_of("C1"), lane_of("X"));
}

/// A full, comparable snapshot of a layout's lane geometry (lanes, per-row
/// above/below/transitions, and every edge's point path).
fn geometry_snapshot(ctx: &LayoutContext, edges: &[LaneEdge]) -> String {
    let mut parts = Vec::new();
    for (row, r) in ctx.lanes.iter().enumerate() {
        parts.push(format!(
            "{}:{} a={:?} b={:?} t={:?}",
            r.node,
            r.lane,
            ctx.row_above.get(row).unwrap_or(&Vec::new()),
            ctx.row_below.get(row).unwrap_or(&Vec::new()),
            ctx.row_transitions.get(row).unwrap_or(&Vec::new()),
        ));
    }
    parts.push("edges:".to_string());
    for e in edges {
        parts.push(format!("{}->{} {:?}", e.child, e.parent, e.points));
    }
    parts.join("\n")
}

/// Regression: lane geometry must be a pure function of node order + parent
/// edges — byte-identical across every rebuild and every process.
///
/// The lane-reuse algorithm used to collect each component's members (and seed
/// the topological-order BFS queue) by iterating `HashMap`s. `RandomState`
/// seeds every fresh map (and every fresh process) differently, so the same
/// graph could render with different lanes in two service processes, or even in
/// two rebuilds of the same process. This graph is deliberately
/// hash-order-sensitive: component {M, B, A} is a merge of two rootless
/// branches (two zero-in-degree roots, so queue order decides which root owns
/// the base lane), and the E/F chains are disconnected but overlap in time so
/// interval coloring allocates distinct base columns too. Every rebuild must
/// produce identical geometry, and the tie-break is pinned exactly.
#[test]
fn lane_geometry_is_identical_across_repeated_builds() {
    // Newest-first rows: M(0) merges roots A and B; E2->E1 spans rows 3..5;
    // F2->F1 spans rows 4..6 and overlaps E so the two get distinct bases.
    let nodes = vec![
        "M".to_string(),
        "B".to_string(),
        "A".to_string(),
        "E2".to_string(),
        "F2".to_string(),
        "E1".to_string(),
        "F1".to_string(),
    ];
    let parents = parents_from(&[("M", &["A", "B"]), ("E2", &["E1"]), ("F2", &["F1"])]);

    let build = || {
        let ctx = LayoutContext::new(&nodes, &parents, &no_git);
        let layout = compute_graph_layout(&nodes, &parents, &no_git);
        geometry_snapshot(&ctx, &layout.edges)
    };
    let first = build();
    for round in 1..32 {
        assert_eq!(
            first,
            build(),
            "layout round {round} differs from round 0: HashMap iteration order leaked into lane geometry"
        );
    }

    // Deterministic tie-break canary: the per-component topological queue seeds
    // from display order, so the newer root B owns the base lane and the merge
    // M inherits the older root A's second lane.
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);
    let lane_of = |k: &str| {
        ctx.lanes
            .iter()
            .find(|r| r.node == k)
            .expect("node in rows")
            .lane
    };
    assert_eq!(lane_of("M"), 1, "merge inherits the older root's lane");
    assert_eq!(lane_of("A"), 1, "older root owns the second lane");
    assert_eq!(lane_of("B"), 0, "newer root owns the base lane");
    assert_eq!(lane_of("E2"), 0, "E chain reuses the base column");
    assert_eq!(lane_of("E1"), 0, "E chain reuses the base column");
    assert_eq!(lane_of("F2"), 1, "overlapping F chain gets its own base");
    assert_eq!(lane_of("F1"), 1, "overlapping F chain gets its own base");

    assert_eq!(
        ctx.row_transitions.first().map_or(&[][..], Vec::as_slice),
        &[(1, 0)][..],
        "merge jog from lane 1 to lane 0 happens at the merge row"
    );
    // The merge child M is a TIP (row 0, no children) and its B edge is an
    // ADJACENT cross-lane jog: the transition originates at M's own midpoint,
    // so no source-lane top half is emitted at the merge row (no dangling
    // boundary stub above the dot).
    let no_lanes: &[usize] = &[];
    assert_eq!(
        ctx.row_above.first().map_or(&[][..], Vec::as_slice),
        no_lanes
    );
    // The source-lane bottom half at the merge row survives via the SAME-LANE
    // M->A edge; the adjacent M->B edge contributes only its destination lane.
    assert_eq!(
        ctx.row_below.first().map_or(&[][..], Vec::as_slice),
        &[0, 1][..]
    );
    assert_eq!(
        ctx.row_above.get(5).map_or(&[][..], Vec::as_slice),
        &[0, 1][..]
    );
    assert_eq!(
        ctx.row_below.get(4).map_or(&[][..], Vec::as_slice),
        &[0, 1][..]
    );

    let layout = compute_graph_layout(&nodes, &parents, &no_git);
    let edge_points = |child: &str, parent: &str| -> Vec<(usize, usize)> {
        layout
            .edges
            .iter()
            .find(|e| e.child == child && e.parent == parent)
            .expect("edge in layout")
            .points
            .iter()
            .map(|p| (p.row, p.lane))
            .collect()
    };
    assert_eq!(edge_points("M", "A"), vec![(0, 1), (2, 1)]);
    // Adjacent cross-lane edge: the jog's source point duplicates the child
    // start point, so the path must not repeat it.
    assert_eq!(edge_points("M", "B"), vec![(0, 1), (0, 0), (1, 0)]);
    assert_eq!(edge_points("E2", "E1"), vec![(3, 0), (5, 0)]);
    assert_eq!(edge_points("F2", "F1"), vec![(4, 1), (6, 1)]);
}

// ---------------------------------------------------------------------------
// Pass-through edges: open chains spanning a window
// ---------------------------------------------------------------------------

/// A merge branch that only exists far below the viewport must NOT draw a
/// pass-through line on its lane through an otherwise-empty window.
///
/// Trunk N5..N1 down to merge M of two roots A (on the trunk lane) and B (on a
/// second lane). B sits only at the bottom row. Viewing a window that contains
/// no node on B's lane must not emit a line there — that was the bug where
/// scrolling filled in lanes for chains that shouldn't be present.
#[test]
fn pass_through_skips_lane_with_no_node_in_window() {
    let nodes = vec![
        "N5".to_string(),
        "N4".to_string(),
        "N3".to_string(),
        "N2".to_string(),
        "N1".to_string(),
        "M".to_string(),
        "A".to_string(),
        "B".to_string(),
    ];
    let parents = parents_from(&[
        ("N5", &["N4"]),
        ("N4", &["N3"]),
        ("N3", &["N2"]),
        ("N2", &["N1"]),
        ("N1", &["M"]),
        ("M", &["A", "B"]),
    ]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);
    // Window rows 2..6 = N3,N2,N1,M,A. Lane 0 (B's lane) has no node inside.
    let edges = ctx.edges_for_window(2, 5);
    // The trunk edges are present.
    assert!(edges.iter().any(|e| e.child == "N3" && e.parent == "N2"));
    assert!(edges.iter().any(|e| e.child == "N1" && e.parent == "M"));
    // No pass-through line on lane 0: B is at row 7, below the window.
    assert!(
        !edges
            .iter()
            .any(|e| e.child.starts_with("__pass_through_0")),
        "must not draw a pass-through line on lane 0 (B is below the window)"
    );
}

/// A genuinely sparse chain — one component occupying a lane both above and
/// below the window, with no node inside it — must still draw a pass-through
/// line so the chain stays continuous while scrolling.
#[test]
fn pass_through_draws_sparse_chain_across_window() {
    // One component: N0 (row 0) -> ... -> N9 (row 9), all on one lane. View a
    // middle window with no node inside it; the line must still be drawn.
    let nodes: Vec<String> = (0..10).map(|i| format!("N{i}")).collect();
    let parents = parents_from(&[
        ("N0", &["N1"]),
        ("N1", &["N2"]),
        ("N2", &["N3"]),
        ("N3", &["N4"]),
        ("N4", &["N5"]),
        ("N5", &["N6"]),
        ("N6", &["N7"]),
        ("N7", &["N8"]),
        ("N8", &["N9"]),
    ]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);
    // Window rows 3..7 has no node inside it (all nodes are at rows 0..9).
    let edges = ctx.edges_for_window(3, 4);
    assert!(
        edges.iter().any(|e| e.child.starts_with("__pass_through_")),
        "sparse chain spanning the window must draw a pass-through line"
    );
}

// ---------------------------------------------------------------------------
// Per-row graph geometry (lane + active lanes + transitions)
// ---------------------------------------------------------------------------

/// Per-row active lanes and transitions must be computed correctly for a merge.
///
/// Trunk N5..N1 down to merge M of two roots A (lane 0) and B (lane 1). The
/// vertical line on lane 0 passes through every trunk row; the merge jog from
/// lane 0 to lane 1 happens at the row just above B.
#[test]
fn per_row_active_lanes_and_transitions_for_merge() {
    let nodes = vec![
        "N5".to_string(),
        "N4".to_string(),
        "N3".to_string(),
        "N2".to_string(),
        "N1".to_string(),
        "M".to_string(),
        "A".to_string(),
        "B".to_string(),
    ];
    let parents = parents_from(&[
        ("N5", &["N4"]),
        ("N4", &["N3"]),
        ("N3", &["N2"]),
        ("N2", &["N1"]),
        ("N1", &["M"]),
        ("M", &["A", "B"]),
    ]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);

    // Lane assignment: trunk + A share one lane, B is on a different lane.
    // (Absolute lane numbers are nondeterministic due to HashMap iteration order
    // in the reuse algorithm, so assert the RELATIVE structure.)
    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    let trunk_lane = lane_of("N5");
    assert_eq!(lane_of("A"), trunk_lane, "A shares the trunk lane");
    assert_ne!(lane_of("B"), trunk_lane, "B is on a distinct lane");

    // The vertical line on the trunk lane: below-half on rows 0..5 (N5..M, leaving
    // each node downward) and above-half on rows 1..6 (N4..A, entering each node
    // from above). Row 6 is A, where the trunk line enters from above.
    for row in 0..6 {
        assert!(
            ctx.row_below
                .get(row)
                .is_some_and(|b| b.contains(&trunk_lane)),
            "row {row} should have the trunk lane in below"
        );
    }
    for row in 1..7 {
        assert!(
            ctx.row_above
                .get(row)
                .is_some_and(|a| a.contains(&trunk_lane)),
            "row {row} should have the trunk lane in above"
        );
    }
    // Row 0 is the TIP (newest, no children): no line above its dot.
    assert!(
        !ctx.row_above
            .first()
            .is_some_and(|a| a.contains(&trunk_lane)),
        "tip row 0 should have no line above"
    );
    // Row 7 (B) has its own lane entering from above.
    assert!(ctx
        .row_above
        .get(7)
        .is_some_and(|a| a.contains(&lane_of("B"))));
    // Row 7 is a ROOT (no parents): no line below its dot.
    assert!(
        !ctx.row_below
            .get(7)
            .is_some_and(|b| b.contains(&lane_of("B"))),
        "root row 7 should have no line below"
    );

    // The merge jog from the trunk lane to B's lane happens at row 6 (just above B).
    assert!(
        ctx.row_transitions
            .get(6)
            .is_some_and(|t| t.contains(&(trunk_lane, lane_of("B")))),
        "row 6 should have a trunk->B transition"
    );
}

/// Per-row geometry for a fork-then-merge diamond must emit the merge jog.
///
/// Diamond: D (merge of B,C) -> B,C -> A (root). Newest-first rows: D(0), C(1),
/// B(2), A(3). B and C are on distinct lanes; the merge jog from one branch lane
/// to the other happens at the row just above the second branch's root.
#[test]
fn per_row_transitions_for_fork_then_merge_diamond() {
    let nodes = vec![
        "D".to_string(),
        "C".to_string(),
        "B".to_string(),
        "A".to_string(),
    ];
    let parents = parents_from(&[("D", &["B", "C"]), ("B", &["A"]), ("C", &["A"])]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);

    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    let b_lane = lane_of("B");
    let c_lane = lane_of("C");
    assert_ne!(b_lane, c_lane, "diamond branches must be on distinct lanes");

    // The merge node D sits on the first parent's lane (B's).
    assert_eq!(lane_of("D"), b_lane);

    // The merge jog from B's lane to C's lane happens at the merge node D's own
    // row (row 0): D's dot sits on B's lane.
    assert!(
        ctx.row_transitions
            .first()
            .is_some_and(|t| t.contains(&(b_lane, c_lane))),
        "row 0 should have a B->C transition for the diamond merge"
    );
}

// ---------------------------------------------------------------------------
// Adjacent merge-only cross-lane edges: the jog originates at the child's own
// midpoint, so no source-lane halves may be emitted at the child row. True
// forks and operation→Git session anchors are covered separately below and
// bend in the parent row.
// ---------------------------------------------------------------------------

/// Adjacent cross-lane transition direction and exact halves.
///
/// Merge M (row 0) of X (row 2, first parent, same lane) and Y (row 1, second
/// parent, distinct lane): M->Y is an ADJACENT cross-lane edge. Its transition
/// must land at M's own row, directed (`m_lane` -> `y_lane`); the child row emits
/// the destination lane's bottom half and the parent row emits the destination
/// lane's top half — and NO source-lane top half at the child row.
#[test]
fn adjacent_cross_lane_transition_direction_and_exact_halves() {
    let nodes = vec!["M".to_string(), "Y".to_string(), "X".to_string()];
    let parents = parents_from(&[("M", &["X", "Y"])]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);

    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    let m_lane = lane_of("M");
    let y_lane = lane_of("Y");
    let x_lane = lane_of("X");
    assert_eq!(
        m_lane, x_lane,
        "merge child inherits its first parent's lane"
    );
    assert_ne!(
        m_lane, y_lane,
        "the second merge parent sits on a distinct lane"
    );

    // Row 0 is the merge child: the adjacent M->Y jog happens HERE (the child's
    // own row), directed child_lane -> parent_lane. The same-lane M->X edge
    // emits no transition.
    assert_eq!(
        ctx.row_transitions.first().map_or(&[][..], Vec::as_slice),
        &[(m_lane, y_lane)][..],
        "adjacent cross-lane edge must transition at the child row"
    );
    // M is a TIP: no line enters it from above, and the adjacent edge adds no
    // source-lane top half at its own row.
    let no_lanes: &[usize] = &[];
    assert_eq!(
        ctx.row_above.first().map_or(&[][..], Vec::as_slice),
        no_lanes,
        "no source-lane top half at the child row"
    );
    // Bottom halves at the child row: the source lane comes only from the
    // same-lane M->X edge; the adjacent edge contributes ONLY its destination
    // lane (the transition's destination half covers it at this row).
    let mut expected_below = vec![m_lane, y_lane];
    expected_below.sort_unstable();
    assert_eq!(
        ctx.row_below.first().map_or(&[][..], Vec::as_slice),
        expected_below.as_slice(),
        "child row keeps the same-lane source half and the adjacent destination half"
    );
    // The parent row receives the destination lane's top half (line enters Y
    // from above), and no transition occurs away from the child row.
    assert!(
        ctx.row_above.get(1).is_some_and(|a| a.contains(&y_lane)),
        "destination lane must enter the parent row from above"
    );
    assert!(
        ctx.row_transitions.get(1).is_some_and(Vec::is_empty),
        "no transition at the parent row"
    );
    assert!(
        ctx.row_transitions.get(2).is_some_and(Vec::is_empty),
        "no transition at the other parent's row"
    );
}

/// A lone adjacent fork must bend in the parent row without dangling halves.
///
/// Fork: A (row 2) has children C (row 0, first, same lane) and B (row 1,
/// second, distinct lane). B->A is a true fork edge, so B's lane leaves its dot
/// downward, enters A's row from above, and curves into A's dot there. The
/// same-lane C->A trunk continues independently on A's lane.
#[test]
fn lone_adjacent_fork_anchors_transition_at_parent() {
    let nodes = vec!["C".to_string(), "B".to_string(), "A".to_string()];
    let parents = parents_from(&[("B", &["A"]), ("C", &["A"])]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);

    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    let a_lane = lane_of("A");
    let b_lane = lane_of("B");
    let c_lane = lane_of("C");
    assert_ne!(b_lane, a_lane, "the fork branch sits on a distinct lane");
    assert_eq!(c_lane, a_lane, "the first fork child keeps the root's lane");
    let row_of = |k: &str| ctx.keys.iter().position(|n| n == k).unwrap();
    assert_eq!(row_of("B"), 1, "B is immediately above A");
    assert_eq!(row_of("A"), 2, "A is the parent row");

    // The fork transition belongs to A's own row, directed B -> A. This gives
    // the webview the top-right source + destination-dot anchors required for
    // a smooth bottom-right curve.
    assert_eq!(
        ctx.row_transitions.get(2).map_or(&[][..], Vec::as_slice),
        &[(b_lane, a_lane)][..],
        "lone adjacent fork must transition at the parent row"
    );
    assert!(
        ctx.row_transitions.get(1).is_some_and(Vec::is_empty),
        "child row must not carry the reversed-side transition"
    );
    // B is a branch tip: no line enters above its dot; its own lane leaves
    // downward and is continued into the top half of A's row.
    assert!(
        !ctx.row_above.get(1).is_some_and(|a| a.contains(&b_lane)),
        "no source-lane top half above the branch tip"
    );
    assert!(
        ctx.row_below.get(1).is_some_and(|b| b.contains(&b_lane)),
        "branch lane must leave the child dot downward"
    );
    assert!(
        ctx.row_above.get(2).is_some_and(|a| a.contains(&b_lane)),
        "branch lane must enter the anchor row from above"
    );
    // The destination lane keeps its halves at row 1 (C->A passes through A's
    // lane here too) and enters the parent row from above.
    assert!(
        ctx.row_above.get(1).is_some_and(|a| a.contains(&a_lane)),
        "destination lane top half survives at the child row"
    );
    assert!(
        ctx.row_below.get(1).is_some_and(|b| b.contains(&a_lane)),
        "destination lane bottom half survives at the child row"
    );
    assert!(
        ctx.row_above.get(2).is_some_and(|a| a.contains(&a_lane)),
        "same-lane trunk independently enters the parent row from above"
    );
    // A is a root: no line leaves it downward, and no other transitions exist.
    let no_lanes: &[usize] = &[];
    assert_eq!(
        ctx.row_below.get(2).map_or(&[][..], Vec::as_slice),
        no_lanes,
        "root row has no bottom halves"
    );
    assert!(
        ctx.row_transitions.first().is_some_and(Vec::is_empty),
        "same-lane C->A edge emits no transition"
    );
}

/// An exact session→Git anchor uses the same parent-row orientation even when
/// the commit has no second visible child yet.
#[test]
fn session_git_anchor_curves_into_git_parent_row() {
    let nodes = vec!["session".to_string(), "git".to_string()];
    let parents = parents_from(&[("session", &["git"])]);
    let is_git = |key: &str| key == "git";
    let ctx = LayoutContext::new(&nodes, &parents, &is_git);
    let lane_of = |key: &str| ctx.lanes.iter().find(|row| row.node == key).unwrap().lane;
    let session_lane = lane_of("session");
    let git_lane = lane_of("git");
    assert_ne!(session_lane, git_lane, "session and Git use distinct lanes");
    assert!(
        ctx.row_transitions.first().is_some_and(Vec::is_empty),
        "the child row must not render the reversed-side curve"
    );
    assert_eq!(
        ctx.row_transitions.get(1).map_or(&[][..], Vec::as_slice),
        &[(session_lane, git_lane)],
        "the curve belongs in the Git anchor row"
    );
    assert_eq!(
        ctx.row_below.first().map_or(&[][..], Vec::as_slice),
        &[session_lane],
        "the session lane leaves its node downward"
    );
    assert_eq!(
        ctx.row_above.get(1).map_or(&[][..], Vec::as_slice),
        &[session_lane],
        "the session lane enters the Git row from above"
    );

    let layout = compute_graph_layout(&nodes, &parents, &is_git);
    assert_eq!(layout.edges.len(), 1);
    let edge = layout.edges.first().expect("session-to-Git edge");
    assert_eq!(
        edge.points
            .iter()
            .map(|point| (point.row, point.lane))
            .collect::<Vec<_>>(),
        vec![(0, session_lane), (1, session_lane), (1, git_lane)],
        "edge points must turn in the Git parent row"
    );
}

/// A parent-row bend must not be pulled upward to the viewport boundary when
/// the Git anchor itself is still below the visible window.
#[test]
fn session_git_anchor_stays_vertical_until_parent_row_is_visible() {
    let nodes = vec![
        "session".to_string(),
        "filler".to_string(),
        "git".to_string(),
    ];
    let parents = parents_from(&[("session", &["git"])]);
    let is_git = |key: &str| key == "git";
    let ctx = LayoutContext::new(&nodes, &parents, &is_git);
    let session_lane = *ctx.lane_at.get("session").expect("session lane");
    let git_lane = *ctx.lane_at.get("git").expect("Git lane");
    assert_ne!(session_lane, git_lane);

    // Only the session row is visible. Row 1 is the lower viewport boundary;
    // the real curve belongs at the Git parent on row 2, so this slice must be
    // a straight continuation of the session lane.
    let edge = ctx
        .edges_for_window(0, 1)
        .into_iter()
        .find(|edge| edge.child == "session" && edge.parent == "git")
        .expect("session-to-Git edge");
    assert_eq!(
        edge.points,
        vec![
            GridPoint {
                row: 0,
                lane: session_lane,
            },
            GridPoint {
                row: 1,
                lane: session_lane,
            },
        ],
        "an offscreen Git anchor must not create an early boundary curve"
    );
}

/// Mixed incoming/long-edge geometry must preserve legitimately shared halves.
///
/// N2 -> M -> {X (long, same-lane), Y (adjacent, cross-lane), Z (long,
/// cross-lane)}. Rows: N2(0), M(1), Y(2), Z(3), X(4). M's lane has a genuine
/// incoming segment from N2 at M's row and a genuine outgoing run to X, so the
/// source-lane halves at row 1 must survive the adjacent-edge cleanup; the
/// non-adjacent M->Z edge must keep its source run into the jog row (row 2)
/// and its destination run out (rows 2-3).
#[test]
fn mixed_incoming_long_edge_preserves_shared_source_halves() {
    let nodes = vec![
        "N2".to_string(),
        "M".to_string(),
        "Y".to_string(),
        "Z".to_string(),
        "X".to_string(),
    ];
    let parents = parents_from(&[("N2", &["M"]), ("M", &["X", "Y", "Z"])]);
    let ctx = LayoutContext::new(&nodes, &parents, &no_git);

    let lane_of = |k: &str| ctx.lanes.iter().find(|r| r.node == k).unwrap().lane;
    let m_lane = lane_of("M");
    let y_lane = lane_of("Y");
    let z_lane = lane_of("Z");
    let x_lane = lane_of("X");
    assert_eq!(
        m_lane, x_lane,
        "merge child inherits its first parent's lane"
    );
    assert_ne!(m_lane, y_lane, "adjacent merge parent is cross-lane");
    assert_ne!(m_lane, z_lane, "long merge parent is cross-lane");
    assert_ne!(y_lane, z_lane, "secondary merge parents use distinct lanes");

    // M's row (1): the incoming N2->M edge and the outgoing M->X edge both use
    // M's lane, so the source-lane top half (entering M from above) and bottom
    // half (leaving M downward) must be PRESERVED despite the adjacent M->Y
    // cleanup. The adjacent edge adds only its destination lane below.
    assert_eq!(
        ctx.row_above.get(1).map_or(&[][..], Vec::as_slice),
        &[m_lane][..],
        "incoming edge keeps the source-lane top half at the child row"
    );
    let mut expected_below = vec![m_lane, y_lane];
    expected_below.sort_unstable();
    assert_eq!(
        ctx.row_below.get(1).map_or(&[][..], Vec::as_slice),
        expected_below.as_slice(),
        "outgoing same-lane edge keeps the source half; adjacent edge adds destination"
    );
    // The adjacent M->Y transition happens at the child row.
    assert!(
        ctx.row_transitions
            .get(1)
            .is_some_and(|t| t.contains(&(m_lane, y_lane))),
        "adjacent cross-lane edge transitions at the child row"
    );
    // The non-adjacent M->Z edge keeps its source vertical run INTO the jog row
    // (row 2: source top half on m_lane) and its destination run OUT (row 2:
    // destination bottom half; row 3: destination top half), with the jog at
    // row 2 — NOT at the child row.
    assert!(
        ctx.row_above.get(2).is_some_and(|a| a.contains(&m_lane)),
        "long cross-lane edge keeps its source run into the jog row"
    );
    assert!(
        ctx.row_transitions
            .get(2)
            .is_some_and(|t| t.contains(&(m_lane, z_lane))),
        "long cross-lane edge jogs at the row above its parent"
    );
    assert!(
        ctx.row_below.get(2).is_some_and(|b| b.contains(&z_lane)),
        "long cross-lane edge keeps its destination run out of the jog row"
    );
    assert!(
        ctx.row_above.get(3).is_some_and(|a| a.contains(&z_lane)),
        "long cross-lane edge keeps its destination run into the parent row"
    );
    // No transition at the parent rows of the long/same-lane edges.
    assert!(
        ctx.row_transitions.get(3).is_some_and(Vec::is_empty),
        "no transition at Z's row"
    );
    assert!(
        ctx.row_transitions.get(4).is_some_and(Vec::is_empty),
        "no transition at X's row"
    );
}

/// Edge points for adjacent cross-lane edges must start at the child's own
/// point exactly once — no duplicated child start point, no open segment
/// before the jog.
#[test]
fn adjacent_cross_lane_edge_points_have_no_duplicate_open_start() {
    // Merge M (row 0) of X (row 2) and Y (row 1): M->Y is the adjacent
    // cross-lane edge; M->X is a long same-lane edge; add a long cross-lane
    // edge (M->Z at row 3) for coverage.
    let nodes = vec![
        "M".to_string(),
        "Y".to_string(),
        "Z".to_string(),
        "X".to_string(),
    ];
    let parents = parents_from(&[("M", &["X", "Y", "Z"])]);
    let layout = compute_graph_layout(&nodes, &parents, &no_git);

    let lane_of = |k: &str| layout.rows.iter().find(|r| r.node == k).unwrap().lane;
    let row_of = |k: &str| layout.rows.iter().position(|r| r.node == k).unwrap();
    assert_eq!(row_of("M"), 0);
    assert_eq!(row_of("Y"), 1);

    // Every edge starts exactly once at the child's own point and ends at the
    // parent's point, with no repeated consecutive point and no open start.
    for edge in &layout.edges {
        let pts = &edge.points;
        assert!(
            !pts.is_empty(),
            "edge {}->{} must have points",
            edge.child,
            edge.parent
        );
        let child_row = row_of(&edge.child);
        let parent_row = row_of(&edge.parent);
        assert_eq!(
            pts.first(),
            Some(&GridPoint {
                row: child_row,
                lane: lane_of(&edge.child),
            }),
            "edge {}->{} must start at the child's own point",
            edge.child,
            edge.parent
        );
        assert_eq!(
            pts.last(),
            Some(&GridPoint {
                row: parent_row,
                lane: lane_of(&edge.parent),
            }),
            "edge {}->{} must end at the parent's point",
            edge.child,
            edge.parent
        );
        for w in pts.windows(2) {
            let a = w.first().expect("window has two points");
            let b = w.get(1).expect("window has two points");
            assert!(
                a != b,
                "edge {}->{} has a duplicate point at {:?}",
                edge.child,
                edge.parent,
                a
            );
        }
    }

    // Exact path for the adjacent cross-lane edge: child point -> jog at the
    // child row -> parent point, with the child point appearing exactly once.
    let m_lane = lane_of("M");
    let y_lane = lane_of("Y");
    let adjacent = layout
        .edges
        .iter()
        .find(|e| e.child == "M" && e.parent == "Y")
        .expect("adjacent cross-lane edge exists");
    assert_eq!(
        adjacent.points,
        vec![
            GridPoint {
                row: 0,
                lane: m_lane
            },
            GridPoint {
                row: 0,
                lane: y_lane
            },
            GridPoint {
                row: 1,
                lane: y_lane
            },
        ],
        "adjacent cross-lane edge must not duplicate the child start point"
    );
}

// ---------------------------------------------------------------------------
// Projection-level virtual-edge test (SPEC §5 gate): relationship notes must
// drive real fork/subagent branch lanes without mutating stored parents.
// ---------------------------------------------------------------------------

use editchain_core::{
    ActorId, Clock, MessageOp, NoteOp, NoteRelationship, Op, OpKind, ParentSet, Payload, ScopeRef,
    SessionId, Tags,
};

/// A standalone message op, scoped to a session, with a causal parent.
///
/// Using non-import ops keeps each node visible (imports fold continuations
/// into their raw import parent), so diverging fork/subagent chains stay as
/// separate rows where the lane divergence is observable.
fn msg(node: u64, seq: u64, release: u64, parent: Option<OpId>) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(release)),
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(format!("msg {node}.{seq}").into_bytes()),
            content_type: Payload::Inline(b"text/plain".to_vec()),
        }),
    }
}

/// A `ForkOf` relationship note: the fork session's first op (`parent_id`,
/// fork-first) points at the representative session's first op (`target_id`,
/// shared root).
fn fork_note(parent_id: OpId, target_id: OpId) -> Op {
    Op {
        id: OpId::new(NodeId(7), 0, 0xFF0),
        parents: ParentSet::One(parent_id),
        actor: ActorId(0),
        clock: Clock::None,
        scope: ScopeRef::Session(SessionId(0)),
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![target_id],
            relationship: NoteRelationship::ForkOf,
            content: Payload::Empty,
        }),
    }
}

#[test]
fn fork_note_renders_distinct_lane_via_virtual_edge() {
    // Two sessions that are forks of one original. Session A (root chain) and
    // session B (fork) share nothing causally — they only relate through a
    // ForkOf note. The projection must read that note so B branches off A's
    // root on a distinct lane, without any stored-parent mutation.
    let root = msg(1, 1, 10, None); // A's first op (shared root)
    let a2 = msg(1, 2, 10, Some(root.id)); // A's continuation
    let fork_first = msg(2, 1, 20, None); // B's first op
    let fork_second = msg(2, 2, 20, Some(fork_first.id)); // B's continuation
    let fork_note_op = fork_note(fork_first.id, root.id);

    let projection = HistoryProjection::from_ops(vec![
        root.clone(),
        a2.clone(),
        fork_first.clone(),
        fork_second,
        fork_note_op,
    ]);

    // The fork note must not render as a standalone row: every rendered node
    // must be one of the four real ops (no `0xFF0` note row).
    let nodes = projection.nodes();
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();
    assert_eq!(
        nodes.len(),
        4,
        "the structural note must fold out of rendered rows; got {keys:?}"
    );

    // The two fork continuations — session A's next op (`a2`) and session B's
    // first op (`fork_first`) — both descend from the shared root after the
    // fork. They must draw on distinct lanes; the trunk keeps the root's lane
    // and the fork branch lifts onto a spare lane.
    let graph = projection.graph_layout();
    let lane_of = |op_id: OpId| {
        graph
            .rows
            .iter()
            .find(|r| r.node == op_id.to_string())
            .unwrap()
            .lane
    };
    assert_ne!(
        lane_of(fork_first.id),
        lane_of(a2.id),
        "the fork branch and the trunk continuation must diverge onto distinct lanes"
    );
}

#[test]
fn fork_note_never_suppresses_rows_without_occurrence_evidence() {
    // A ForkOf edge says where a branch attaches; it does not prove that every
    // lower-sequence row in that physical source is a copied event. Without exact
    // OccurrenceOf facts projection must retain all physical rows.
    let roota = msg(1, 1, 10, None); // A's first (shared)
    let a2 = msg(1, 2, 10, Some(roota.id)); // shared
    let a3 = msg(1, 3, 10, Some(a2.id)); // A's divergence boundary
    let a4 = msg(1, 4, 10, Some(a3.id)); // A's continuation
                                         // Branch B re-imports the shared prologue: its own rootA2 and a2 copies.
    let b_roota = msg(2, 1, 20, None);
    let b_a2 = msg(2, 2, 20, Some(b_roota.id));
    let b3 = msg(2, 3, 20, Some(b_a2.id)); // B's divergence boundary
    let b4 = msg(2, 4, 20, Some(b3.id)); // B's continuation (kept)

    // ForkOf: parent = B's boundary (b3), target = A's boundary (a3).
    let fork_span = fork_note(b3.id, a3.id);

    let projection = HistoryProjection::from_ops(vec![
        roota.clone(),
        a2.clone(),
        a3.clone(),
        a4.clone(),
        b_roota.clone(),
        b_a2.clone(),
        b3.clone(),
        b4.clone(),
        fork_span,
    ]);

    let nodes = projection.nodes();
    let keys: Vec<String> = nodes.iter().map(HistoryNode::node_key).collect();

    assert_eq!(keys.len(), 8);
    for id in [
        roota.id, a2.id, a3.id, a4.id, b_roota.id, b_a2.id, b3.id, b4.id,
    ] {
        assert!(keys.contains(&id.to_string()), "row {id} must be kept");
    }
}

/// Unknown source time stays unknown; neither a session minimum nor an unrelated
/// global timestamp is fabricated for display ordering.
#[test]
fn undated_header_keeps_unknown_source_time() {
    // Old session A: an undated header + dated ops at Jul-10 (ts 1000..1003).
    let a_header = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::None,
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::META,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"header".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    let a1 = msg(1, 2, 10, None); // clock = seq = 2 -> tiny date
    let a2 = msg(1, 3, 10, Some(a1.id));
    // New session B with a LATER date (ts 5_000).
    let b1 = msg(2, 5000, 20, None);

    let projection =
        HistoryProjection::from_ops(vec![a_header.clone(), a1.clone(), a2.clone(), b1.clone()]);
    let nodes = projection.nodes();
    // Find the header node and the session-A dated nodes.
    let header_key = a_header.id.to_string();
    let header = nodes
        .iter()
        .find(|n| n.node_key() == header_key)
        .expect("header");
    assert_eq!(header.timestamp_ms(), 0);
    assert_eq!(
        header.effective_time(),
        editchain_project::EffectiveTime::Unknown
    );
}
