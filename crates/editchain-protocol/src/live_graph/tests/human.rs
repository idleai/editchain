use super::*;

fn human(key: &str, time: u64, parents: &[&str], stream: &str) -> LiveBlockMeta {
    LiveBlockMeta {
        human_stream: Some(stream.into()),
        ..node(key, time, parents)
    }
}

#[test]
fn recorder_restarts_inherit_the_human_lane_through_serialized_updates() {
    let mut graph = LiveGraph::default();
    graph.edit(
        &[],
        &[
            node("git:base", 1, &[]),
            human("1:0:1", 2, &["git:base"], "person:workspace"),
            human("9:0:1", 3, &["git:base"], "peer:workspace"),
            human("1:0:2", 4, &["1:0:1"], "person:workspace"),
            human("9:0:2", 5, &["9:0:1"], "peer:workspace"),
        ],
    );
    let lane = row(&graph, "1:0:1", 0).lane;
    let peer = row(&graph, "9:0:1", 0).lane;
    assert_ne!(lane, peer);
    let mut previous = "1:0:2";
    for key in ["2:0:1", "3:0:1", "4:0:1"] {
        // Native-to-WASM JSON keeps the stream even with skewed restart clocks.
        let next = graph
            .causal_updates(&[human(key, 2, &[previous], "person:workspace")])
            .unwrap();
        let next: Vec<LiveBlockMeta> =
            serde_json::from_slice(&serde_json::to_vec(&next).unwrap()).unwrap();
        graph.edit(&[], &next);
        assert_eq!(row(&graph, key, 0).lane, lane);
        assert_eq!(row(&graph, "9:0:1", 0).lane, peer);
        assert!(row(&graph, key, 0).transitions.is_empty());
        previous = key;
    }
}

#[test]
fn true_forks_stay_separate_even_with_the_same_persistent_human_stream() {
    let mut graph = LiveGraph::default();
    graph.edit(&[], &[human("1:0:1", 1, &[], "person:workspace")]);
    graph.edit(&[], &[human("2:0:1", 3, &["1:0:1"], "person:workspace")]);
    let main = row(&graph, "2:0:1", 0).lane;
    graph.edit(&[], &[human("3:0:1", 2, &["1:0:1"], "person:workspace")]);
    let fork = row(&graph, "3:0:1", 0).lane;
    assert_ne!(main, fork);
    graph.edit(&[], &[human("4:0:1", 4, &["2:0:1"], "person:workspace")]);
    assert_eq!(row(&graph, "4:0:1", 0).lane, main);
    assert_eq!(row(&graph, "3:0:1", 0).lane, fork);
    assert!(!row(&graph, "1:0:1", 0).transitions.is_empty());
}

#[test]
fn human_continuity_requires_both_endpoints_to_name_the_same_stream() {
    for stream in [
        None,
        Some("another-person:workspace"),
        Some("person:other-workspace"),
    ] {
        let mut graph = LiveGraph::default();
        graph.edit(&[], &[human("1:0:1", 1, &[], "person:workspace")]);
        let mut next = node("1:0:2", 2, &["1:0:1"]);
        next.human_stream = stream.map(str::to_owned);
        graph.edit(&[], &[next]);
        assert_ne!(row(&graph, "1:0:1", 0).lane, row(&graph, "1:0:2", 0).lane);
    }
    // Untagged legacy/agent streams still use their recorder-qualified IDs.
    let mut graph = LiveGraph::default();
    graph.edit(&[], &[node("1:0:1", 1, &[])]);
    graph.edit(&[], &[node("2:0:1", 2, &["1:0:1"])]);
    assert_ne!(row(&graph, "1:0:1", 0).lane, row(&graph, "2:0:1", 0).lane);
}
