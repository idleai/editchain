use super::*;
use editchain_core::taxonomy::ChainState;
use editchain_project::layout::LayoutContext;

fn node(key: &str, time: u64, parents: &[&str]) -> LiveBlockMeta {
    LiveBlockMeta {
        task_group: None,
        task_summary: None,
        task_protected: false,
        key: key.into(),
        sort_time: time,
        row_count: 1,
        spans: Vec::new(),
        node_key: key.into(),
        parents: parents.iter().map(|key| (*key).into()).collect(),
        chain_state: ChainState::Active,
    }
}

fn row(graph: &LiveGraph, key: &str, slot: u64) -> HistoryRow {
    let mut row: HistoryRow = serde_json::from_value(serde_json::json!({
        "summary": key, "timestamp_ms": 1, "group": "chain", "node_key": key,
        "parents": [], "is_submodule": false,
    }))
    .unwrap();
    graph.decorate(key, slot, &mut row);
    row
}

#[test]
fn causal_order_preserves_tied_and_skewed_sibling_edges_through_incremental_arrival() {
    let nodes = [
        node("git:base", 100, &[]),
        node("spawn", 99, &["git:base"]),
        node("a:196608", 100, &["spawn"]),
        node("a:262144", 100, &["a:196608"]),
        node("b:196608", 98, &["spawn"]),
        node("b:262144", 98, &["b:196608"]),
        node("merge", 100, &["a:262144", "b:262144"]),
    ];
    for size in [1, nodes.len()] {
        let mut graph = LiveGraph::default();
        for batch in nodes.chunks(size) {
            let before: Vec<_> = graph
                .nodes
                .keys()
                .map(|key| (key.clone(), row(&graph, key, 0).lane))
                .collect();
            let scheduled = graph.causal_updates(batch).unwrap();
            graph.edit(&[], &scheduled);
            for (key, lane) in before {
                assert_eq!(row(&graph, &key, 0).lane, lane);
            }
            for node in graph.nodes.values() {
                for parent in &node.parents {
                    assert!(node.order() < graph.nodes.get(parent).unwrap().order());
                    assert!(graph
                        .paths
                        .contains_key(&(node.key.clone(), parent.clone())));
                }
            }
        }
        let update = graph
            .causal_updates(&[node("fresh", 200, &["merge"])])
            .unwrap();
        assert_eq!(update.len(), 1);
        assert!(graph
            .causal_updates(&[node("spawn", 99, &["merge"])])
            .is_err());
    }
}

#[test]
fn repairing_a_late_parent_moves_only_the_causally_affected_descendants() {
    let mut graph = LiveGraph::default();
    graph.edit(
        &[],
        &[
            node("child", 10, &["missing"]),
            node("tip", 11, &["child"]),
            node("unrelated", 12, &[]),
        ],
    );
    let before = row(&graph, "child", 0).lane;
    let scheduled = graph.causal_updates(&[node("missing", 20, &[])]).unwrap();
    assert_eq!(scheduled.len(), 3);
    graph.edit(&[], &scheduled);
    assert_eq!(row(&graph, "child", 0).lane, before);
    assert_eq!(graph.nodes.get("unrelated").unwrap().sort_time, 12);
    assert!(graph.nodes.get("tip").unwrap().order() < graph.nodes.get("child").unwrap().order());
}

#[test]
fn a_child_arriving_first_cannot_take_the_parent_execution_lane() {
    let mut graph = LiveGraph::default();
    let mut spawn = node("spawn", 1, &[]);
    spawn.node_key = "1:0:1".into();
    graph.edit(&[], &[spawn]);
    let main_lane = row(&graph, "spawn", 0).lane;
    let mut child = node("child", 2, &["spawn"]);
    child.node_key = "2:0:1".into();
    graph.edit(&[], &[child]);
    assert_ne!(row(&graph, "child", 0).lane, main_lane);
    let mut next = node("next", 3, &["spawn"]);
    next.node_key = "1:0:2".into();
    graph.edit(&[], &[next]);
    assert_eq!(row(&graph, "next", 0).lane, main_lane);
    let mut merge = node("merge", 4, &["child", "next"]);
    merge.node_key = "1:0:3".into();
    graph.edit(&[], &[merge]);
    assert_eq!(row(&graph, "merge", 0).lane, main_lane);
}

fn assert_activity_geometry(graph: &LiveGraph) {
    let keys: Vec<_> = graph.order.iter().map(|order| order.1.clone()).collect();
    let layout = LayoutContext::new_with_chain_state(
        &keys,
        &|key| graph.nodes.get(key).unwrap().parents.clone(),
        &|key| is_git(graph.nodes.get(key).unwrap()),
        &|key| graph.nodes.get(key).unwrap().chain_state,
    );
    for (index, key) in keys.iter().enumerate() {
        let actual = row(graph, key, 0);
        let mut transitions = layout.row_transitions.get(index).unwrap().clone();
        transitions.sort_unstable();
        let mut muted = layout.row_muted_transitions.get(index).unwrap().clone();
        muted.sort_unstable();
        assert_eq!(
            actual.lane,
            layout.lanes.get(index).unwrap().lane,
            "{key}: lane"
        );
        assert_eq!(
            &actual.above,
            layout.row_above.get(index).unwrap(),
            "{key}: above"
        );
        assert_eq!(
            &actual.below,
            layout.row_below.get(index).unwrap(),
            "{key}: below"
        );
        assert_eq!(actual.transitions, transitions, "{key}: bends");
        assert_eq!(
            &actual.muted_above,
            layout.row_muted_above.get(index).unwrap(),
            "{key}: muted above"
        );
        assert_eq!(
            &actual.muted_below,
            layout.row_muted_below.get(index).unwrap(),
            "{key}: muted below"
        );
        assert_eq!(actual.muted_transitions, muted, "{key}: muted bends");
    }
}

#[test]
fn bootstrap_matches_established_activity_layout_for_forks_merges_sessions_and_anchors() {
    let cases = [
        vec![
            node("c", 3, &["b"]),
            node("b", 2, &["a"]),
            node("a", 1, &[]),
        ],
        vec![
            node("d", 4, &["b", "c"]),
            node("c", 3, &["a"]),
            node("b", 2, &["a"]),
            node("a", 1, &[]),
        ],
        vec![
            node("a2", 8, &["a1"]),
            node("b2", 7, &["b1"]),
            node("a1", 6, &["git:base"]),
            node("b1", 5, &["git:base"]),
            node("git:base", 3, &["git:old"]),
            node("git:old", 1, &[]),
        ],
        vec![
            node("a2", 8, &["a1"]),
            node("a1", 7, &[]),
            node("b2", 4, &["b1"]),
            node("b1", 2, &[]),
            node("git:old", 1, &[]),
        ],
        vec![
            node("a", 6, &["git:old"]),
            node("unrelated", 4, &[]),
            node("git:old", 1, &[]),
        ],
        vec![node("a", 1, &["b"]), node("b", 2, &[])],
    ];
    for mut nodes in cases {
        for muted in [false, true] {
            if muted {
                nodes.first_mut().unwrap().chain_state = ChainState::Muted;
            }
            let mut graph = LiveGraph::default();
            graph.edit(&[], &nodes);
            assert_activity_geometry(&graph);
            for node in &mut nodes {
                node.chain_state = if node.chain_state.is_active() {
                    ChainState::Muted
                } else {
                    ChainState::Active
                };
                graph.edit(&[], std::slice::from_ref(node));
                assert_activity_geometry(&graph);
            }
        }
    }
}

#[test]
fn adjacent_edges_stay_straight_and_retractions_do_not_leave_ghost_paths() {
    let middle = node("middle", 2, &["old"]);
    let mut graph = LiveGraph::default();
    graph.edit(
        &[],
        &[
            node("new", 3, &["middle"]),
            middle.clone(),
            node("old", 1, &[]),
        ],
    );
    assert_activity_geometry(&graph);
    graph.edit(&[], &[node("tip", 4, &["new"])]);
    assert_activity_geometry(&graph);
    assert_eq!(graph.max_lane(), 0);
    assert!(row(&graph, "middle", 0).transitions.is_empty());
    graph.edit(&["middle".into()], &[]);
    assert!(row(&graph, "new", 0).parents.is_empty());
    assert!(row(&graph, "new", 0).below.is_empty());
    assert!(row(&graph, "old", 0).above.is_empty());
    graph.edit(&[], &[middle]);
    assert_activity_geometry(&graph);
    assert_eq!(row(&graph, "middle", 1).above, vec![0]);
    assert_eq!(row(&graph, "middle", 1).below, vec![0]);
}

#[test]
fn live_fork_keeps_distinct_branches_and_git_leftmost() {
    let mut graph = LiveGraph::default();
    graph.edit(
        &[],
        &[
            node("left", 4, &["root"]),
            node("root", 2, &["git:base"]),
            node("git:base", 1, &[]),
        ],
    );
    let left = row(&graph, "left", 0).lane;
    graph.edit(&[], &[node("right", 3, &["root"])]);
    assert_eq!(row(&graph, "left", 0).lane, left);
    assert_ne!(row(&graph, "right", 0).lane, left);
    assert_eq!(row(&graph, "git:base", 0).lane, 0);
    assert!(row(&graph, "root", 0)
        .transitions
        .iter()
        .any(|(_, to)| *to == left));
    graph.edit(&[], &[node("merge", 5, &["left", "right"])]);
    assert_eq!(row(&graph, "left", 0).lane, left);
    assert_ne!(row(&graph, "right", 0).lane, left);
}

#[test]
fn adding_shared_git_spines_does_not_shift_existing_lanes() {
    let mut graph = LiveGraph::default();
    graph.edit(
        &[],
        &[
            node("a", 6, &["git:base"]),
            node("b", 5, &["git:old"]),
            node("git:base", 2, &["git:old"]),
            node("git:old", 1, &[]),
        ],
    );
    let before: Vec<_> = ["a", "b", "git:base", "git:old"]
        .into_iter()
        .map(|key| (key, row(&graph, key, 0).lane))
        .collect();
    for added in [node("c", 7, &["git:base"]), node("d", 8, &["git:old"])] {
        graph.edit(&[], &[added]);
        for (key, lane) in &before {
            assert_eq!(
                row(&graph, key, 0).lane,
                *lane,
                "{key} moved when a shared Git spine appeared"
            );
        }
    }
    assert_ne!(
        graph.spines.get("git:base").unwrap().lane,
        graph.spines.get("git:old").unwrap().lane
    );
    graph.edit(&["c".into(), "d".into()], &[]);
    for (key, lane) in before {
        assert_eq!(
            row(&graph, key, 0).lane,
            lane,
            "{key} moved after spine retraction"
        );
    }
}

#[test]
fn inserting_before_a_merge_parent_moves_only_its_jog_boundary() {
    let mut graph = LiveGraph::default();
    graph.edit(&[], &[node("a", 6, &["git:old"]), node("git:old", 1, &[])]);
    graph.edit(&[], &[node("other", 3, &[])]);
    assert_activity_geometry(&graph);
    graph.edit(&["other".into()], &[]);
    assert_activity_geometry(&graph);
}

#[test]
fn linear_append_work_is_bounded_and_does_not_reassign_existing_lanes() {
    for count in [1_u64, 10_000] {
        let nodes: Vec<_> = (1..=count)
            .map(|index| {
                node(
                    &format!("n{index}"),
                    index,
                    &if index == 1 {
                        Vec::new()
                    } else {
                        vec![format!("n{}", index - 1)]
                    }
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                )
            })
            .collect();
        let mut graph = LiveGraph::default();
        graph.edit(&[], &nodes);
        graph.edit(&[], &[node("tip", count + 1, &[&format!("n{count}")])]);
        assert_eq!(graph.paths.len(), usize::try_from(count).unwrap());
        assert_eq!(graph.max_lane(), 0);
        assert_eq!(row(&graph, "tip", 0).below, vec![0]);
        assert!(row(&graph, "tip", 0).transitions.is_empty());
    }
}

#[test]
fn new_overlapping_git_anchors_have_separate_spines_and_retractions_remove_their_paths() {
    let mut graph = LiveGraph::default();
    graph.edit(
        &[],
        &[node("git:new", 2, &["git:old"]), node("git:old", 1, &[])],
    );
    graph.edit(
        &[],
        &[
            node("a", 8, &["git:old"]),
            node("b", 6, &["git:old"]),
            node("c", 7, &["git:new"]),
            node("d", 5, &["git:new"]),
        ],
    );
    let old_spine = graph.spines.get("git:old").unwrap().lane;
    let new_spine = graph.spines.get("git:new").unwrap().lane;
    assert_ne!(old_spine, new_spine);
    assert_eq!(row(&graph, "git:old", 0).lane, 0);
    let old_lane = graph.lanes.display(old_spine);
    graph.edit(&["a".into()], &[]);
    assert!(!graph.spines.contains_key("git:old"));
    assert!(!row(&graph, "git:old", 0).above.contains(&old_lane));
    assert_eq!(graph.paths.len(), 4);
}
