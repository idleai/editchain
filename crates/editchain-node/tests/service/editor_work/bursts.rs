use super::{batch, changed, document, event, live_request, records, start, window};
use editchain_core::{human::HumanWorkKind, ParentSet};
use editchain_node::Server;
use editchain_protocol::{Request, ResponseBody};
use editchain_store::format::encode_op;
use serde_json::{json, Value};

fn burst(sequence: u64, changes: &[u64]) -> Value {
    event(
        sequence,
        json!({"type":"human_edit_batch", "edits": changes.iter()
        .map(|change| json!({"change":change,"signal":"keyboard_selection"})).collect::<Vec<_>>()}),
    )
}

#[test]
fn typing_bursts_have_one_exact_diff_each_and_preserve_intermediate_automatic_changes() {
    let tmp = tempfile::tempdir().expect("burst fixture");
    let root = tmp.path();
    super::super::seed_ai(root, "AI\n");
    let events = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":"AI\n"}),
        ),
        changed(3, 2, "AI\n", "AIh\n"),
        changed(4, 3, "AIh\n", "AIhi\n"),
        burst(5, &[3, 4]),
        changed(6, 4, "AIhi\n", "AIhi\nAGENT\n"),
        changed(7, 5, "AIhi\nAGENT\n", "AIhi!\nAGENT\n"),
        changed(8, 6, "AIhi!\nAGENT\n", "AIhi!!\nAGENT\n"),
        burst(9, &[7, 8]),
        event(10, json!({"type":"document_saved","document":document(6)})),
    ];
    let mut recorder = Server::new();
    // Persistence/network batches may split at any raw change or receipt.
    for observation in &events {
        assert_eq!(
            live_request(&mut recorder, batch(root, vec![observation.clone()]))["accepted"],
            1
        );
    }
    let work = records(root);
    assert_eq!(
        work.len(),
        2,
        "four keystrokes form two edits across one automatic mutation"
    );
    assert!(work
        .iter()
        .all(|(_, work)| work.kind == HumanWorkKind::Edit));
    assert_eq!(work[1].0.parents, ParentSet::One(work[0].0.id));
    std::fs::write(root.join("ai.txt"), "AIhi!!\nAGENT\n").expect("save");
    let open = json!({"workspace_path":root,"chain_dir":".editchain"});
    let report = live_request(&mut recorder, json!({"GetHumanWork":open}));
    assert_eq!(
        report["human_changes"], 4,
        "coverage uses every constituent change"
    );
    assert_eq!(report["unattributed_changes"], 1);
    assert_eq!(report["historical_ai_lines_edited"], 1);
    assert_eq!(report["capture_gaps"], 0);
    for mode in ["Open", "OpenLive", "OpenLivePaged"] {
        let mut history = Server::new();
        let response = live_request(&mut history, json!({mode:open}));
        let mut snapshot = response["snapshot_id"].clone();
        let mut rows = window(&mut history, &snapshot);
        if let Some(header) = rows
            .iter()
            .find(|row| row["task_group"]["expanded"] == false)
        {
            let update = live_request(
                &mut history,
                json!({"ToggleLive":{
                "snapshot_id":snapshot,"key":header["continuity_key"],"task":true}}),
            );
            snapshot = update["deltas"]
                .as_array()
                .expect("deltas")
                .last()
                .expect("delta")["snapshot_id"]
                .clone();
            rows = window(&mut history, &snapshot);
        }
        assert_eq!(
            rows.iter()
                .filter(|row| row["file_change"]["source"] == "human")
                .count(),
            2,
            "{mode}"
        );
        for (index, (before, after)) in [("AI\n", "AIhi\n"), ("AIhi\nAGENT\n", "AIhi!!\nAGENT\n")]
            .iter()
            .enumerate()
        {
            let row = rows
                .iter()
                .find(|row| row["node_key"] == work[index].0.id.to_string())
                .expect("burst row");
            assert_eq!(row["sub_ops"], json!([]), "no per-keystroke fold");
            let diff = live_request(
                &mut history,
                json!({"GetFileDiff":{"snapshot_id":snapshot,"change":row["file_change"]}}),
            );
            assert_eq!(diff["before"], *before);
            assert_eq!(diff["after"], *after);
        }
    }
    assert_eq!(
        live_request(&mut Server::new(), batch(root, events))["replayed"],
        10
    );
    for ((before, _), (after, _)) in work.iter().zip(records(root)) {
        assert_eq!(
            encode_op(before).expect("before"),
            encode_op(&after).expect("after")
        );
    }
}

#[test]
fn discontinuous_bursts_never_claim_automatic_or_missing_changes() {
    for boundary in [
        "automatic",
        "save",
        "context",
        "missing",
        "content",
        "document",
    ] {
        let tmp = tempfile::tempdir().expect("burst fixture");
        super::super::seed_ai(tmp.path(), "AI\n");
        let middle = match boundary {
            "automatic" => changed(4, 3, "AIh\n", "AGENT\n"),
            "save" => event(4, json!({"type":"document_saved","document":document(2)})),
            "context" => event(
                4,
                json!({"type":"workspace_context","observed_ms":2004,"repositories":[]}),
            ),
            _ => event(4, json!({"type":"editor_activated","document":document(2)})),
        };
        let mut last = changed(5, 3, "AIh\n", "AIhi\n");
        if boundary == "automatic" {
            last = changed(5, 4, "AGENT\n", "AGENTi\n");
        }
        if boundary == "content" {
            last = changed(5, 3, "DRIFT\n", "DRIFTi\n");
        }
        if boundary == "document" {
            last["event"]["document"]["id"] = json!("other");
        }
        let changes = if boundary == "missing" {
            vec![2, 5]
        } else {
            vec![3, 5]
        };
        let request = batch(
            tmp.path(),
            vec![
                start(),
                event(
                    2,
                    json!({"type":"document_snapshot","document":document(1),"text":"AI\n"}),
                ),
                changed(3, 2, "AI\n", "AIh\n"),
                middle,
                last,
                burst(6, &changes),
            ],
        );
        let mut server = Server::new();
        assert_eq!(live_request(&mut server, request)["accepted"], 6);
        let work = records(tmp.path());
        assert_eq!(work.len(), 1, "{boundary}");
        assert_eq!(work[0].1.kind, HumanWorkKind::Gap, "{boundary}");
        let report = live_request(
            &mut server,
            json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}}),
        );
        assert_eq!(
            report["human_changes"], 0,
            "{boundary}: rejected grouping cannot affect coverage"
        );
        assert_eq!(report["historical_ai_lines_edited"], 0, "{boundary}");
    }
}

#[test]
fn edits_reverted_within_a_burst_still_count_as_human_work() {
    let tmp = tempfile::tempdir().expect("burst fixture");
    super::super::seed_ai(tmp.path(), "AI\n");
    let mut server = Server::new();
    let request = batch(
        tmp.path(),
        vec![
            start(),
            event(
                2,
                json!({"type":"document_snapshot","document":document(1),"text":"AI\n"}),
            ),
            changed(3, 2, "AI\n", "AIh\n"),
            changed(4, 3, "AIh\n", "AI\n"),
            burst(5, &[3, 4]),
        ],
    );
    assert_eq!(live_request(&mut server, request)["accepted"], 5);
    let work = records(tmp.path());
    assert_eq!(work.len(), 1);
    let before = work[0].1.before.as_ref().expect("before");
    let after = work[0].1.after.as_ref().expect("after");
    assert_eq!(before.content, after.content);
    assert_ne!(before.version, after.version);
    let report = live_request(
        &mut server,
        json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}}),
    );
    assert_eq!(report["human_changes"], 2);
    assert_eq!(report["historical_ai_lines_edited"], 1);
}

#[test]
fn a_pending_keystroke_can_cross_publication_of_the_previous_burst() {
    let tmp = tempfile::tempdir().expect("burst fixture");
    let request = batch(
        tmp.path(),
        vec![
            start(),
            event(
                2,
                json!({"type":"document_snapshot","document":document(1),"text":"AI\n"}),
            ),
            changed(3, 2, "AI\n", "AIh\n"),
            changed(4, 3, "AIh\n", "AIhi\n"),
            changed(5, 4, "AIhi\n", "AIhi!\n"),
            burst(6, &[3, 4]),
            changed(7, 5, "AIhi!\n", "AIhi!!\n"),
            burst(8, &[5, 7]),
        ],
    );
    assert_eq!(live_request(&mut Server::new(), request)["accepted"], 8);
    let work = records(tmp.path());
    assert_eq!(work.len(), 2);
    assert!(work
        .iter()
        .all(|(_, work)| work.kind == HumanWorkKind::Edit));
    assert_eq!(work[0].1.after, work[1].1.before);
}

#[test]
fn malformed_burst_receipts_are_rejected_before_writing() {
    for edits in [
        json!([]),
        json!([{"change":0,"signal":"keyboard_selection"}]),
        json!([{"change":3,"signal":"keyboard_selection"}]),
        json!([{"change":1,"signal":"undo"}]),
        json!([{"change":1,"signal":"keyboard_selection"},{"change":1,"signal":"keyboard_selection"}]),
        json!([{"change":2,"signal":"keyboard_selection"},{"change":1,"signal":"keyboard_selection"}]),
    ] {
        let tmp = tempfile::tempdir().expect("burst fixture");
        let body = batch(
            tmp.path(),
            vec![
                start(),
                event(2, json!({"type":"human_edit_batch","edits":edits})),
            ],
        );
        let response = Server::new()
            .handle(&Request {
                id: 1,
                body: serde_json::from_value(body).expect("shape"),
            })
            .expect("response");
        assert!(matches!(response.body, ResponseBody::Error(_)));
        assert!(!tmp.path().join(".editchain").exists());
    }
}
