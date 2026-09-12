use super::*;
use editchain_core::{NodeId, OpId};
use std::cmp::Reverse;

fn task(turn: &str) -> TaskIdentity {
    TaskIdentity {
        key: format!("thread:{turn}"),
        thread: "thread".into(),
        turn: turn.into(),
        boundary: OpId::new(NodeId(1), 0, 0),
    }
}

fn put(runs: &mut runs::Runs, time: u64, turn: &str, parent: u64) {
    let key = format!("{turn}:{time}");
    runs.put(
        key.clone(),
        (Reverse(time), key, 1),
        task(turn),
        format!("{turn}:{parent}"),
    );
}

#[test]
fn append_and_revision_have_bounded_membership_work_at_any_task_size() {
    for size in [10, 1_000, 100_000] {
        let mut runs = runs::Runs::default();
        for time in 1..=size {
            put(&mut runs, time, "a", time - 1);
        }
        let original = runs.sections.keys().next().unwrap().clone();
        runs.dirty.clear();
        runs.membership.clear();
        put(&mut runs, size + 1, "a", size);
        assert_eq!(runs.sections.len(), 1);
        assert_eq!(
            runs.sections.get(&original).unwrap().members.len(),
            usize::try_from(size).unwrap() + 1
        );
        assert_eq!(runs.dirty.len(), 1);
        assert_eq!(runs.membership.len(), 1);
        assert_eq!(
            runs.section(&format!("a:{}", size + 1)),
            Some(original.as_str())
        );
        runs.dirty.clear();
        runs.membership.clear();
        put(&mut runs, size + 1, "a", size);
        assert!(runs.dirty.is_empty());
        assert_eq!(runs.membership.len(), 1);
    }
}

#[test]
fn interleaved_tasks_keep_exact_paths_without_moving_members() {
    let mut runs = runs::Runs::default();
    for (time, parent) in [(10, 0), (20, 10), (30, 20), (40, 30)] {
        put(&mut runs, time, "a", parent);
    }
    let original = runs.section("a:40").unwrap().to_owned();
    runs.membership.clear();
    put(&mut runs, 15, "b", 0);
    put(&mut runs, 25, "b", 15);
    assert_eq!(runs.sections.len(), 2);
    assert_eq!(runs.membership.len(), 2);
    assert_eq!(runs.sections.get(&original).unwrap().members.len(), 4);
    assert_eq!(runs.section("a:40"), Some(original.as_str()));
}

#[test]
fn disconnected_members_and_task_incarnations_never_join() {
    let mut runs = runs::Runs::default();
    put(&mut runs, 10, "a", 0);
    put(&mut runs, 20, "a", 0);
    assert_ne!(runs.section("a:10"), runs.section("a:20"));
    let mut restored = task("a");
    restored.key.push_str(":restored");
    restored.boundary.seq = 30;
    runs.put(
        "a:30".into(),
        (Reverse(30), "a:30".into(), 1),
        restored,
        "a:20".into(),
    );
    assert_ne!(runs.section("a:30"), runs.section("a:20"));
}

#[test]
fn removed_interior_splits_and_an_exact_late_edge_can_rejoin() {
    let mut runs = runs::Runs::default();
    for (time, parent) in [(10, 0), (20, 10), (30, 20), (40, 30)] {
        put(&mut runs, time, "a", parent);
    }
    runs.remove("a:30");
    assert_ne!(runs.section("a:40"), runs.section("a:20"));
    put(&mut runs, 40, "a", 20);
    assert_eq!(runs.section("a:40"), runs.section("a:20"));
}

fn meta(key: &str, time: u64, parents: &[&str]) -> LiveBlockMeta {
    serde_json::from_value(
        serde_json::json!({"key":key, "node_key":key, "sort_time":time,
        "parents":parents, "row_count":1, "spans":[]}),
    )
    .unwrap()
}

#[test]
fn summaries_are_physical_and_late_forks_split_protected_attachments() {
    let mut groups = Tasks::default();
    let mut graph = editchain_protocol::live_graph::LiveGraph::default();
    let mut inputs = HashMap::new();
    let nodes = vec![
        meta("root", 0, &[]),
        meta("a", 1, &["root"]),
        meta("b", 2, &["a"]),
        meta("c", 3, &["b"]),
        meta("d", 4, &["c"]),
        meta("e", 5, &["d"]),
    ];
    for node in &nodes {
        drop(inputs.insert(
            node.key.clone(),
            LiveRow {
                key: node.key.clone(),
                anchor: OpId::new(NodeId(1), 0, node.sort_time),
                incarnation: OpId::new(NodeId(1), 0, node.sort_time),
                operations: Vec::new(),
                task: Some(task("a")),
            },
        ));
    }
    graph.edit(&[], &nodes);
    let changes = groups.update(&[], &nodes, &inputs, &graph);
    let summary = changes.summaries.get("e").unwrap().as_ref().unwrap();
    assert_eq!(summary.member_count, 5);
    assert_eq!(summary.anchor, "e");
    assert_eq!(changes.summaries.len(), 1);
    assert!(!changes.membership.contains_key("root"));
    let fork = meta("subagent", 6, &["c"]);
    graph.edit(&[], std::slice::from_ref(&fork));
    let changes = groups.update(&[], &[fork], &inputs, &graph);
    assert!(!graph.foldable("c"));
    assert!(groups.runs.section("c").is_none());
    assert_ne!(groups.runs.section("a"), groups.runs.section("e"));
    for (anchor, summary) in changes
        .summaries
        .iter()
        .filter_map(|(key, summary)| summary.as_ref().map(|summary| (key, summary)))
    {
        assert_eq!(anchor, &summary.anchor);
        assert!(nodes.iter().any(|node| &node.key == anchor));
        assert!(summary.member_count > 1);
    }
}
