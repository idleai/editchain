use super::{batch, changed, document, event, live_request, records, start, window};
use editchain_core::{human::HumanWorkKind, Tags};
use editchain_node::Server;
use serde_json::{json, Value};

fn observed(sequence: u64, change: u64, group: u64) -> Value {
    event(
        sequence,
        json!({"type":"observed_edit_batch","group":group,"changes":[change]}),
    )
}

fn human(sequence: u64, change: u64, group: u64) -> Value {
    event(
        sequence,
        json!({"type":"human_edit_batch","group":group,
        "edits":[{"change":change,"signal":"editor_input"}]}),
    )
}

#[test]
fn unattributed_edits_stream_one_exact_diff_without_claiming_human_or_ai_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    super::super::seed_ai(root, "AI\n");
    let _ack = live_request(
        &mut Server::new(),
        batch(
            root,
            vec![
                start(),
                event(
                    2,
                    json!({"type":"document_snapshot","document":document(1),"text":"AI\n"}),
                ),
                changed(3, 2, "AI\n", "A\n"),
                observed(4, 3, 3),
            ],
        ),
    );
    let mut server = Server::new();
    let opened = live_request(
        &mut server,
        json!({"OpenLivePaged":{"workspace_path":root,"chain_dir":".editchain"}}),
    );
    let first = window(&mut server, &opened["snapshot_id"])
        .into_iter()
        .find(|row| row["file_change"]["source"] == "editor")
        .unwrap();
    let continued = batch(root, vec![changed(5, 3, "A\n", "\n"), observed(6, 5, 3)]);
    let _ack = live_request(&mut Server::new(), continued.clone());
    let updated = live_request(
        &mut server,
        json!({"SyncLive":{
        "epoch":opened["live"]["epoch"],"after_revision":opened["live"]["revision"],"codex":null}}),
    );
    let snapshot = &updated["deltas"].as_array().unwrap().last().unwrap()["snapshot_id"];
    let rows: Vec<_> = window(&mut server, snapshot)
        .into_iter()
        .filter(|row| row["file_change"]["source"] == "editor")
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["continuity_key"], first["continuity_key"]);
    assert_eq!(rows[0]["sub_ops"], json!([]));
    let diff = live_request(
        &mut server,
        json!({"GetFileDiff":{"snapshot_id":snapshot,"change":rows[0]["file_change"]}}),
    );
    assert_eq!(diff["before"], "AI\n");
    assert_eq!(diff["after"], "\n");
    for (op, record) in records(root) {
        assert_eq!(record.kind, HumanWorkKind::ObservedEdit);
        assert!(!op.tags.matches_any(Tags::HUMAN | Tags::INFERRED));
    }
    let report = live_request(
        &mut Server::new(),
        json!({"GetHumanWork":{"workspace_path":root,"chain_dir":".editchain"}}),
    );
    assert_eq!(report["human_changes"], 0);
    assert_eq!(report["unattributed_changes"], 2);
    assert_eq!(report["historical_ai_lines"], 1);
    assert_eq!(report["historical_ai_lines_edited"], 0);
    assert_eq!(live_request(&mut Server::new(), continued)["replayed"], 2);
}

#[test]
fn background_observed_edits_do_not_replace_the_active_human_group() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    super::super::seed_ai(root, "AI\n");
    let mut background = document(1);
    background["id"] = json!("background");
    background["path"] = json!("other.txt");
    let snapshot = event(
        5,
        json!({"type":"document_snapshot","document":background,"text":"x"}),
    );
    background["version"] = json!(2);
    let mut automatic = changed(6, 2, "x", "xy");
    automatic["event"]["document"] = background;
    let _ack = live_request(
        &mut Server::new(),
        batch(
            root,
            vec![
                start(),
                event(
                    2,
                    json!({"type":"document_snapshot","document":document(1),"text":"AI\n"}),
                ),
                changed(3, 2, "AI\n", "AIh\n"),
                human(4, 3, 3),
                snapshot,
                automatic,
                observed(7, 6, 6),
                changed(8, 3, "AIh\n", "AIhi\n"),
                human(9, 8, 3),
            ],
        ),
    );
    let work = records(root);
    assert_eq!(
        work.iter()
            .map(|(_, record)| record.kind)
            .collect::<Vec<_>>(),
        vec![
            HumanWorkKind::Edit,
            HumanWorkKind::ObservedEdit,
            HumanWorkKind::Edit
        ]
    );
    assert_eq!(work[0].1.edit_group, work[2].1.edit_group);
    let mut server = Server::new();
    let opened = live_request(
        &mut server,
        json!({"OpenLivePaged":{"workspace_path":root,"chain_dir":".editchain"}}),
    );
    let rows = window(&mut server, &opened["snapshot_id"]);
    assert_eq!(
        rows.iter()
            .filter(|row| row["file_change"]["source"] == "human")
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["file_change"]["source"] == "editor")
            .count(),
        1
    );
}
