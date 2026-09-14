use super::{batch, changed, document, event, live_request, records, start, window};
use editchain_core::human::HumanWorkKind;
use editchain_node::Server;
use editchain_protocol::Request;
use editchain_store::SegmentStore;
use serde_json::{json, Value};

fn receipt(sequence: u64, change: u64, group: u64) -> Value {
    event(
        sequence,
        json!({"type":"human_edit_batch","group":group,
        "edits":[{"change":change,"signal":"keyboard_selection"}]}),
    )
}

fn initial(root: &std::path::Path) -> Value {
    super::super::seed_ai(root, "AI\n");
    batch(
        root,
        vec![
            start(),
            event(
                2,
                json!({"type":"document_snapshot",
        "document":document(1),"text":"AI\n"}),
            ),
            changed(3, 2, "AI\n", "AIh\n"),
            receipt(4, 3, 3),
        ],
    )
}

fn human_row(history: &mut Server, snapshot: &Value) -> Value {
    let rows: Vec<_> = window(history, snapshot)
        .into_iter()
        .filter(|row| row["file_change"]["source"] == "human")
        .collect();
    assert_eq!(rows.len(), 1, "one file row regardless of receipt count");
    assert_eq!(
        rows[0]["sub_ops"],
        json!([]),
        "the actual edit needs no fold"
    );
    rows[0].clone()
}

#[test]
fn live_receipts_update_one_exact_diff_across_delivery_and_recorder_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let request = initial(root);
    let _ack = live_request(&mut Server::new(), request);
    let open = json!({"workspace_path":root,"chain_dir":".editchain"});
    let mut history = Server::new();
    let opened = live_request(&mut history, json!({"OpenLivePaged":open}));
    let mut snapshot = opened["snapshot_id"].clone();
    let first = human_row(&mut history, &snapshot);
    let continued = batch(
        root,
        vec![changed(5, 3, "AIh\n", "AIhi\n"), receipt(6, 5, 3)],
    );
    let ack = live_request(&mut Server::new(), continued.clone());
    assert_eq!(ack["work"]["bootstrap"], false);
    let delta = live_request(
        &mut history,
        json!({"SyncLive":{
        "epoch":opened["live"]["epoch"],"after_revision":0,"codex":null}}),
    );
    snapshot = delta["deltas"].as_array().unwrap().last().unwrap()["snapshot_id"].clone();
    let current = human_row(&mut history, &snapshot);
    assert_eq!(current["continuity_key"], first["continuity_key"]);
    let diff = live_request(
        &mut history,
        json!({"GetFileDiff":{
        "snapshot_id":snapshot,"change":current["file_change"]}}),
    );
    assert_eq!(diff["before"], "AI\n");
    assert_eq!(diff["after"], "AIhi\n");
    assert_eq!(
        records(root).len(),
        2,
        "both immutable receipts remain available"
    );
    assert_eq!(live_request(&mut Server::new(), continued)["replayed"], 2);
    drop(history);
    let mut resumed = Server::new();
    let opened = live_request(&mut resumed, json!({"OpenLivePaged":open}));
    assert_eq!(
        human_row(&mut resumed, &opened["snapshot_id"])["continuity_key"],
        first["continuity_key"]
    );
}

#[test]
fn a_live_edit_cannot_claim_an_intervening_automatic_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _ack = live_request(&mut Server::new(), initial(root));
    let _ack = live_request(
        &mut Server::new(),
        batch(
            root,
            vec![
                changed(5, 3, "AIh\n", "AGENT\n"),
                changed(6, 4, "AGENT\n", "AGENTi\n"),
                receipt(7, 6, 3),
            ],
        ),
    );
    let work = records(root);
    assert_eq!(work.len(), 2);
    assert_eq!(work[1].1.kind, HumanWorkKind::Gap);
    let report = live_request(
        &mut Server::new(),
        json!({"GetHumanWork":{
        "workspace_path":root,"chain_dir":".editchain"}}),
    );
    assert_eq!(report["human_changes"], 1);
    assert_eq!(report["unattributed_changes"], 2);
}

#[test]
fn reading_and_background_file_changes_do_not_fragment_the_active_file_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _ack = live_request(&mut Server::new(), initial(root));
    let mut other = document(1);
    other["id"] = json!("background");
    other["path"] = json!("other.txt");
    let snapshot = event(
        5,
        json!({"type":"document_snapshot","document":other,"text":"other"}),
    );
    other["version"] = json!(2);
    let mut automatic = changed(6, 2, "other", "agent");
    automatic["event"]["document"] = other.clone();
    let _ack = live_request(
        &mut Server::new(),
        batch(
            root,
            vec![
                snapshot,
                automatic,
                event(7, json!({"type":"document_saved","document":other})),
                event(
                    8,
                    json!({"type":"code_read","group":3,"document":document(2),"editor":"view",
            "ranges":[{"start":[0,0],"end":[1,0]}],"started_ms":0,"duration_ms":2000}),
                ),
                changed(9, 3, "AIh\n", "AIhi\n"),
                receipt(10, 9, 3),
            ],
        ),
    );
    let work = records(root);
    assert_eq!(work.len(), 3, "two edit receipts and one retained read");
    assert_eq!(work[1].1.kind, HumanWorkKind::Read);
    assert_eq!(work[0].1.edit_group, work[1].1.edit_group);
    assert_eq!(work[0].1.edit_group, work[2].1.edit_group);
    let mut history = Server::new();
    let opened = live_request(
        &mut history,
        json!({"OpenLivePaged":{
        "workspace_path":root,"chain_dir":".editchain"}}),
    );
    let _row = human_row(&mut history, &opened["snapshot_id"]);
    let report = live_request(
        &mut Server::new(),
        json!({"GetHumanWork":{
        "workspace_path":root,"chain_dir":".editchain"}}),
    );
    assert_eq!(report["human_changes"], 2);
    assert_eq!(report["unattributed_changes"], 1);
}

#[test]
fn writer_contention_preserves_the_checkpoint_for_the_next_capture_process() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _ack = live_request(&mut Server::new(), initial(root));
    let pending = batch(
        root,
        vec![changed(5, 3, "AIh\n", "AIhi\n"), receipt(6, 5, 3)],
    );
    let lock = SegmentStore::open(root.join(".editchain")).unwrap();
    let request: Request = serde_json::from_value(json!({"id":1,"body":pending})).unwrap();
    assert!(Server::new().handle(&request).is_err());
    drop(lock);
    let ack = live_request(&mut Server::new(), pending);
    assert_eq!(
        ack["work"]["bootstrap"], false,
        "a busy writer never discards capture state"
    );
    assert!(ack["work"]["records_decoded"].as_u64().unwrap() < 20);
    assert_eq!(records(root).len(), 2);
}

#[test]
fn restored_tabs_are_inventory_and_only_new_opens_and_final_closes_are_activities() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut started = start();
    started["event"]["activity_schema"] = json!(2);
    let tab = |kind: &str, restored: bool| {
        json!({"type":kind,"editor":"file",
        "uri":"file:///ai.txt","path":"ai.txt","restored":restored})
    };
    let closed =
        json!({"type":"editor_closed","editor":"file","uri":"file:///ai.txt","path":"ai.txt"});
    let _ack = live_request(
        &mut Server::new(),
        batch(
            root,
            vec![
                started,
                event(2, tab("editor_opened", true)),
                event(3, closed.clone()),
                event(4, tab("editor_opened", false)),
                event(5, closed),
            ],
        ),
    );
    let kinds: Vec<_> = records(root).iter().map(|(_, work)| work.kind).collect();
    assert_eq!(
        kinds,
        [
            HumanWorkKind::EditorClosed,
            HumanWorkKind::EditorOpened,
            HumanWorkKind::EditorClosed
        ]
    );
}
