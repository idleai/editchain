use super::{batch, document, event, live_request, seed_ai, start};
use editchain_node::Server;
use editchain_protocol::{editor::MAX_EDITOR_BUFFER_BYTES, Request, ResponseBody};
use serde_json::{json, Value};

fn change(sequence: u64, version: u64, before: &str, after: &str) -> Value {
    event(
        sequence,
        json!({"type":"document_changed", "document":document(version),
        "before_version":version - 1, "before":before, "after":after, "reason":null,
        "changes":[{"offset":0,"length":1,"text":after.chars().next().expect("nonempty destination").to_string()}]}),
    )
}

fn rows(history: &mut Server, snapshot: &Value) -> Vec<Value> {
    live_request(
        history,
        json!({"GetWindow":{
        "snapshot_id":snapshot,"offset":0,"limit":100,"include_layout":false}}),
    )["rows"]
        .as_array()
        .expect("history rows")
        .clone()
}

#[test]
fn large_snapshots_preserve_exact_diffs_after_restart_external_edits_and_disk_drift() {
    let tmp = tempfile::tempdir().expect("large snapshot fixture");
    let root = tmp.path();
    let prefix = "0 // 😀\n";
    let before = format!(
        "{prefix}{}",
        "x".repeat(MAX_EDITOR_BUFFER_BYTES - prefix.len())
    );
    let human = before.replacen('0', "1", 1);
    let external = before.replacen('0', "7", 1);
    let last = before.replacen('0', "8", 1);
    let mut recording = start();
    recording["event"]["activity_schema"] = json!(2);
    let events = vec![
        recording,
        event(
            2,
            json!({"type":"document_snapshot", "document":document(1), "text":before}),
        ),
        event(
            3,
            json!({"type":"code_read", "document":document(1), "editor":"view",
            "ranges":[{"start":[1,2_000_000],"end":[1,2_000_010]}], "started_ms":1,"duration_ms":2000}),
        ),
        change(4, 2, &before, &human),
        event(
            5,
            json!({"type":"human_edit","change":4,"signal":"editor_input"}),
        ),
        change(6, 3, &human, &external),
        change(7, 4, &external, &last),
        event(
            8,
            json!({"type":"human_edit","change":7,"signal":"editor_input"}),
        ),
        event(9, json!({"type":"document_saved","document":document(4)})),
    ];
    let mut recorder = Server::new();
    for observation in &events {
        assert_eq!(
            live_request(&mut recorder, batch(root, vec![observation.clone()]))["accepted"],
            1
        );
    }
    assert_eq!(
        live_request(&mut Server::new(), batch(root, events))["replayed"],
        9
    );
    std::fs::write(root.join("ai.txt"), "later disk contents\n").expect("external disk drift");
    let mut history = Server::new();
    let opened = live_request(
        &mut history,
        json!({"OpenLivePaged":{"workspace_path":root,"chain_dir":".editchain"}}),
    );
    let mut snapshot = opened["snapshot_id"].clone();
    let mut visible = rows(&mut history, &snapshot);
    if let Some(header) = visible
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
        visible = rows(&mut history, &snapshot);
    }
    let mut diffs = Vec::new();
    for row in visible
        .iter()
        .filter(|row| row["file_change"]["source"] == "human")
    {
        let diff = live_request(
            &mut history,
            json!({"GetFileDiff":{
            "snapshot_id":snapshot,"change":row["file_change"]}}),
        );
        assert_eq!(diff["partial"], false);
        assert_eq!(diff["binary"], false);
        diffs.push((
            diff["before"].as_str().expect("before").to_owned(),
            diff["after"].as_str().expect("after").to_owned(),
        ));
    }
    assert_eq!(
        diffs.len(),
        2,
        "two human edits retain complete revisions across the external change"
    );
    assert!(diffs.contains(&(before, human)));
    assert!(diffs.contains(&(external, last)));
}

#[test]
fn oversized_snapshots_and_incorrect_large_replacements_are_rejected_before_writing() {
    let big = "x".repeat(MAX_EDITOR_BUFFER_BYTES + 1);
    let acceptable = "x".repeat(2 * 1024 * 1024);
    let invalid = [
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":big}),
        ),
        change(2, 2, &big, &acceptable),
        change(2, 2, &acceptable, &big),
        change(2, 2, &acceptable, &acceptable.replacen('x', "bad", 1)),
    ];
    for observation in invalid {
        let tmp = tempfile::tempdir().expect("invalid large snapshot fixture");
        let request = Request {
            id: 1,
            body: serde_json::from_value(batch(tmp.path(), vec![start(), observation]))
                .expect("request shape"),
        };
        let response = Server::new().handle(&request).expect("validation response");
        assert!(matches!(response.body, ResponseBody::Error(_)));
        assert!(
            !tmp.path().join(".editchain").exists(),
            "invalid evidence cannot reach durable history"
        );
    }
}

#[test]
fn maximum_size_saved_files_remain_in_human_read_and_edit_coverage() {
    let tmp = tempfile::tempdir().expect("large coverage fixture");
    let header = "0 // AI generated\n";
    seed_ai(tmp.path(), header);
    let before = format!(
        "{header}{}",
        "x".repeat(MAX_EDITOR_BUFFER_BYTES - header.len())
    );
    let after = before.replacen('0', "1", 1);
    let events = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot", "document":document(1), "text":before}),
        ),
        event(
            3,
            json!({"type":"code_read", "document":document(1), "editor":"view",
            "ranges":[{"start":[0,0],"end":[1,0]}], "started_ms":2000,"duration_ms":2000}),
        ),
        change(4, 2, &before, &after),
        event(
            5,
            json!({"type":"human_edit","change":4,"signal":"editor_input"}),
        ),
    ];
    let _recorded = live_request(&mut Server::new(), batch(tmp.path(), events));
    std::fs::write(tmp.path().join("ai.txt"), &after).expect("save full captured buffer");
    let report = live_request(
        &mut Server::new(),
        json!({"GetHumanWork":{
        "workspace_path":tmp.path(), "chain_dir":".editchain"}}),
    );
    assert_eq!(
        report["ai_lines"], 1,
        "large files are included in the denominator"
    );
    assert_eq!(report["read_lines"], 1);
    assert_eq!(report["edited_lines"], 1);
    assert_eq!(report["read_and_edited_lines"], 1);
    assert_eq!(report["unavailable_files"], 0);
}
