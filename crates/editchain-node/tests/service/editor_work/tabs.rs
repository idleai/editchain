use super::{agent, batch, changed, document, event, git, live_request, records, start, window};
use editchain_core::{human::HumanWorkKind, NodeId, OpId, ParentSet, Tags};
use editchain_node::Server;
use editchain_store::{format::encode_op, CanonicalChain};
use serde_json::{json, Value};

fn tab(sequence: u64, kind: &str, editor: &str) -> Value {
    event(
        sequence,
        json!({"type":kind,"editor":editor,"uri":"file:///ai.txt","path":"ai.txt"}),
    )
}

fn read(sequence: u64, kind: &str, duration: u64) -> Value {
    event(
        sequence,
        json!({"type":kind,"document":document(1),"editor":"view",
        "ranges":[{"start":[1,0],"end":[2,0]}],"started_ms":2000,"duration_ms":duration}),
    )
}

#[test]
fn tabs_reads_and_edits_share_a_connected_human_series_beside_agent_work() {
    let tmp = tempfile::tempdir().expect("workspace");
    let root = tmp.path();
    drop(git(root, &["init", "-q"]));
    std::fs::write(root.join("ai.txt"), "base\n").expect("base");
    drop(git(root, &["add", "ai.txt"]));
    drop(git(root, &["commit", "-qm", "base"]));
    let open = json!({"workspace_path":root,"chain_dir":".editchain"});
    let mut recorder = Server::new();
    let context = live_request(&mut recorder, json!({"GetEditorContext":open}));
    let agent_id = agent(
        root,
        OpId::new(NodeId(73), 0, 1),
        "base\n",
        "base\nAI\n",
        &context["repositories"][0],
    );
    let mut policy = start();
    policy["event"]["activity_schema"] = json!(2);
    let request = batch(
        root,
        vec![
            policy,
            event(
                2,
                json!({"type":"workspace_context","workspace_path":root,"observed_ms":2000,"repositories":context["repositories"]}),
            ),
            event(
                3,
                json!({"type":"document_snapshot","document":document(1),"text":"base\nAI\n"}),
            ),
            tab(4, "editor_opened", "left"),
            tab(5, "editor_opened", "right"),
            read(6, "code_read", 2000),
            changed(7, 2, "base\nAI\n", "base\nhuman\n"),
            event(
                8,
                json!({"type":"human_edit","change":7,"signal":"keyboard_selection"}),
            ),
            tab(9, "editor_closed", "right"),
            tab(10, "editor_closed", "left"),
        ],
    );
    assert_eq!(live_request(&mut recorder, request.clone())["accepted"], 10);
    let human = records(root);
    let kinds = [
        HumanWorkKind::EditorOpened,
        HumanWorkKind::EditorOpened,
        HumanWorkKind::Read,
        HumanWorkKind::Edit,
        HumanWorkKind::EditorClosed,
        HumanWorkKind::EditorClosed,
    ];
    assert_eq!(
        human.iter().map(|(_, work)| work.kind).collect::<Vec<_>>(),
        kinds
    );
    for (index, (op, work)) in human.iter().enumerate() {
        assert_eq!(
            op.parents,
            index
                .checked_sub(1)
                .map_or(ParentSet::None, |previous| ParentSet::One(
                    human[previous].0.id
                ))
        );
        assert_eq!(work.turn, 4);
        if matches!(
            work.kind,
            HumanWorkKind::EditorOpened | HumanWorkKind::EditorClosed
        ) {
            assert!(work.before.is_none() && work.after.is_none());
            assert!(
                !op.tags.matches_any(Tags::INFERRED),
                "tab lifecycle is directly observed"
            );
        }
    }
    std::fs::write(root.join("ai.txt"), "base\nhuman\n").expect("save edit");
    let report = live_request(&mut recorder, json!({"GetHumanWork":open}));
    assert_eq!(report["human_changes"], 1);
    assert_eq!(report["read_lines"], 1);
    assert_eq!(report["edited_lines"], 1);
    let _prepared =
        editchain_node::history::prepare_live_checkpoint(root, &root.join(".editchain"))
            .expect("prepare");
    for mode in ["Open", "OpenLive", "OpenLivePaged"] {
        let mut history = Server::new();
        let response = live_request(&mut history, json!({mode:open}));
        let mut snapshot = response["snapshot_id"].clone();
        let mut rows = window(&mut history, &snapshot);
        if mode == "OpenLivePaged" {
            let header = rows
                .iter()
                .find(|row| row["task_group"]["expanded"] == false)
                .expect("folded episode");
            let update = live_request(
                &mut history,
                json!({"ToggleLive":{
                    "snapshot_id":response["snapshot_id"],"key":header["continuity_key"],"task":true
                }}),
            );
            snapshot = update["deltas"]
                .as_array()
                .expect("deltas")
                .last()
                .expect("disclosure delta")["snapshot_id"]
                .clone();
            rows = window(&mut history, &snapshot);
        }
        for (index, (op, work)) in human.iter().enumerate() {
            let row = rows
                .iter()
                .find(|row| row["node_key"] == op.id.to_string())
                .unwrap_or_else(|| {
                    panic!(
                        "{mode}: missing {:?} at {}; rows: {rows:#?}",
                        work.kind, op.id
                    )
                });
            assert_eq!(row["author"], "human");
            assert_eq!(row["visibility"], "primary");
            if index > 0 {
                assert_eq!(row["parents"], json!([human[index - 1].0.id.to_string()]));
            } else {
                let key = format!(
                    "git:{}:{}",
                    context["repositories"][0]["repository"]
                        .as_str()
                        .expect("repo"),
                    context["repositories"][0]["head"].as_str().expect("head")
                );
                assert_eq!(row["parents"], json!([key]));
            }
            let expected = serde_json::to_value(work.kind).expect("kind");
            if work.kind == HumanWorkKind::Edit {
                assert_eq!(row["file_change"]["source"], "human", "{mode}");
                assert_eq!(row["file_change"]["path"], "ai.txt", "{mode}");
                assert_eq!(row["is_subop"], false, "{mode}");
                assert_eq!(row["sub_ops"], json!([]), "{mode}");
                assert_eq!(
                    rows.iter()
                        .filter(|row| row["file_change"]["source"] == "human")
                        .count(),
                    1,
                    "{mode}: no duplicate child"
                );
                let diff = live_request(
                    &mut history,
                    json!({"GetFileDiff":{
                        "snapshot_id":snapshot, "change":row["file_change"]
                    }}),
                );
                assert_eq!(diff["before"], "base\nAI\n");
                assert_eq!(diff["after"], "base\nhuman\n");
            }
            if matches!(
                work.kind,
                HumanWorkKind::EditorOpened | HumanWorkKind::EditorClosed
            ) {
                assert_eq!(row["kind"], expected);
                assert_eq!(row["record_role"], "lifecycle");
                assert_eq!(row["summary"], "ai.txt");
            }
        }
        let agent_row = rows
            .iter()
            .find(|row| row["node_key"] == agent_id.to_string())
            .expect("agent row");
        assert!(
            agent_row["file_change"].is_null(),
            "{mode}: agent disclosure is unchanged"
        );
        assert!(!agent_row["sub_ops"]
            .as_array()
            .expect("agent details")
            .is_empty());
        assert!(rows.iter().all(|row| row["kind"] != "exposure"));
    }
    assert_eq!(live_request(&mut Server::new(), request)["replayed"], 10);
    for ((before, _), (after, _)) in human.iter().zip(records(root)) {
        assert_eq!(
            encode_op(before).expect("before"),
            encode_op(&after).expect("after")
        );
    }
}

#[test]
fn legacy_exposure_is_hidden_without_changing_retained_work_or_its_parents() {
    let tmp = tempfile::tempdir().expect("workspace");
    let root = tmp.path();
    let request = batch(
        root,
        vec![
            start(),
            event(
                2,
                json!({"type":"document_snapshot","document":document(1),"text":"base\nAI\n"}),
            ),
            tab(3, "editor_opened", "old"),
            read(4, "code_exposure", 500),
            read(5, "code_exposure", 2000),
            tab(6, "editor_closed", "old"),
        ],
    );
    assert_eq!(
        live_request(&mut Server::new(), request.clone())["accepted"],
        6
    );
    let work = records(root);
    assert_eq!(
        work.len(),
        2,
        "legacy policy keeps its original derivations"
    );
    assert_eq!(work[0].1.kind, HumanWorkKind::Exposure);
    assert_eq!(work[1].0.parents, ParentSet::One(work[0].0.id));
    let encoded = || {
        CanonicalChain::read(&root.join(".editchain"))
            .expect("chain")
            .located_ops()
            .map(|(op, _)| encode_op(op).expect("encode"))
            .collect::<Vec<_>>()
    };
    let before = encoded();
    for mode in ["Open", "OpenLive"] {
        let mut server = Server::new();
        let response = live_request(
            &mut server,
            json!({mode:{"workspace_path":root,"chain_dir":".editchain"}}),
        );
        let rows = window(&mut server, &response["snapshot_id"]);
        assert!(rows.iter().any(|row| row["kind"] == "read"));
        assert!(rows.iter().all(|row| row["kind"] != "exposure"));
    }
    assert_eq!(live_request(&mut Server::new(), request)["replayed"], 6);
    assert_eq!(
        encoded(),
        before,
        "presentation changes do not rewrite canonical evidence"
    );
}
