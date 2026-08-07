//! Tests for the lane layout module.

use editchain_core::{NodeId, OpId};
use editchain_project::layout::{compute_graph_layout, compute_lanes};
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
    let layout = compute_graph_layout(&nodes, parents);
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
    let layout = compute_graph_layout(&nodes, parents);
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
    let layout = compute_graph_layout(&nodes, parents);

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
