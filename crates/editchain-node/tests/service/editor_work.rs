use super::{
    live_request, write_page, ActorId, Clock, FileEdit, FileOp, FileStage, ImportOp, NodeId, Op,
    OpId, OpKind, ParentSet, Payload, Request, ScopeRef, SessionId, Tags,
};
use serde_json::{json, Value};
use std::path::Path;

#[path = "editor_work/graph.rs"]
mod graph;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";

fn event(sequence: u64, data: Value) -> Value {
    let mut value =
        json!({"schema":1,"session":SESSION,"sequence":sequence,"time_ms":2000 + sequence});
    value["event"] = data;
    value
}

fn start() -> Value {
    event(
        1,
        json!({"type":"tracking_started","dwell_ms":2000,"vscode_version":"1.85.0"}),
    )
}

fn document(version: u64) -> Value {
    json!({"id":"buffer-1","uri":"file:///ai.txt","path":"ai.txt","version":version})
}

fn batch(root: &Path, events: Vec<Value>) -> Value {
    let mut value = json!({"RecordEditorEvents":{"workspace_path":root,"chain_dir":".editchain"}});
    value["RecordEditorEvents"]["events"] = Value::Array(events);
    value
}

fn seed_ai(root: &Path, content: &str) {
    let raw_id = OpId::new(NodeId(73), 0, 1);
    let raw = Op {
        id: raw_id,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::Session(SessionId(73)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                json!({"type":"event_msg","payload":{
            "type":"item_completed","item":{"type":"FileChange","id":"ai-file","status":"completed",
            "changes":{"ai.txt":{"type":"add","content":content}}}}})
                .to_string()
                .into_bytes(),
            ),
            raw_hash: None,
        }),
    };
    let file = Op {
        id: OpId::new(NodeId(73), 0, 2),
        parents: ParentSet::One(raw_id),
        actor: ActorId(1),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::Session(SessionId(73)),
        tags: Tags::AGENT | Tags::FILE,
        kind: OpKind::File(FileOp {
            path: editchain_import::derive_path_id("ai.txt"),
            stage: FileStage::Applied,
            base: None,
            after: None,
            edit: FileEdit::UnifiedDiff(Payload::Inline(b"@@ -0,0 +1 @@\n+AI".to_vec())),
        }),
    };
    let mut page = editchain_store::format::Page::new(0);
    for op in [&raw, &file] {
        page.add_record(
            0,
            editchain_store::format::encode_op(op).expect("encode fixture"),
        );
    }
    editchain_store::SegmentStore::open(root.join(".editchain"))
        .expect("open fixture writer")
        .append_page(&page)
        .expect("append AI fixture");
    std::fs::write(root.join("ai.txt"), content).expect("write fixture file");
}

#[test]
fn late_ai_imports_join_prior_exposure_and_hunks_do_not_claim_untouched_lines() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let text = "human heading\nai generated\nhuman footer\n";
    let mut server = editchain_node::Server::new();
    let events = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":text}),
        ),
        event(
            3,
            json!({"type":"code_exposure","document":document(1),"editor":"view","ranges":[{"start":[0,0],"end":[3,0]}],"started_ms":2000,"duration_ms":3000}),
        ),
    ];
    let _recorded = live_request(&mut server, batch(tmp.path(), events));
    let query = json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}});
    assert_eq!(live_request(&mut server, query.clone())["ai_lines"], 0);
    seed_ai(tmp.path(), text);
    // Replace only the synthetic import's evidence with a one-line update.
    let chain = editchain_store::CanonicalChain::read(&tmp.path().join(".editchain"))
        .expect("read fixture");
    let mut page = editchain_store::format::Page::new(0);
    for (op, _) in chain.located_ops() {
        let mut op = op.clone();
        if op.id.node == NodeId(73) {
            if let OpKind::Import(import) = &mut op.kind {
                let Payload::Inline(bytes) = &import.raw_ref else {
                    panic!("inline test evidence")
                };
                let mut raw: Value = serde_json::from_slice(bytes).expect("raw fixture");
                raw["payload"]["item"]["changes"]["ai.txt"] =
                    json!({"type":"update","unified_diff":"@@ -2 +2 @@\n-old line\n+ai generated"});
                import.raw_ref = Payload::Inline(raw.to_string().into_bytes());
            }
        }
        page.add_record(
            0,
            editchain_store::format::encode_op(&op).expect("fixture encoding"),
        );
    }
    // Rewrite the disposable fixture as one segment; no production chain is modified.
    for name in std::fs::read_dir(tmp.path().join(".editchain")).expect("fixture directory") {
        let path = name.expect("entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "eclog")
        {
            std::fs::remove_file(path).expect("remove fixture segment");
        }
    }
    write_page(&tmp.path().join(".editchain"), &page);
    let report = live_request(&mut editchain_node::Server::new(), query.clone());
    assert_eq!(
        report["ai_lines"], 1,
        "context and untouched lines remain non-AI"
    );
    assert_eq!(
        report["read_lines"], 1,
        "source time allows AI imports to arrive after exposure"
    );
    std::fs::write(tmp.path().join("ai.txt"), "ai generated\nai generated\n")
        .expect("ambiguous file");
    assert_eq!(live_request(&mut editchain_node::Server::new(),query)["ai_lines"],1,"only an already observed origin may survive; ambiguous hunk matching cannot create a second origin");
}

#[test]
fn editor_capture_replay_restart_conflicts_and_gaps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let request = batch(tmp.path(), vec![start()]);
    let mut server = editchain_node::Server::new();
    assert_eq!(live_request(&mut server, request.clone())["accepted"], 1);
    assert_eq!(live_request(&mut server, request.clone())["replayed"], 1);
    let mut restarted = editchain_node::Server::new();
    assert_eq!(live_request(&mut restarted, request.clone())["replayed"], 1);
    let mut conflict = request;
    conflict["RecordEditorEvents"]["events"][0]["time_ms"] = json!(9999);
    assert!(
        restarted
            .handle(&Request {
                id: 1,
                body: serde_json::from_value(conflict).expect("request")
            })
            .is_err(),
        "same identity cannot change bytes"
    );
    let gap = batch(
        tmp.path(),
        vec![event(3, json!({"type":"tracking_stopped"}))],
    );
    assert!(
        restarted
            .handle(&Request {
                id: 2,
                body: serde_json::from_value(gap).expect("request")
            })
            .is_err(),
        "missing predecessor cannot be acknowledged"
    );
    let chain = editchain_store::CanonicalChain::read(&tmp.path().join(".editchain"))
        .expect("read durable chain");
    assert_eq!(
        chain.stats().accepted,
        2,
        "one raw observation and its projection annotation"
    );
    assert_eq!(
        chain
            .located_ops()
            .filter(|(op, _)| editchain_core::human::is_observation_marker(op))
            .count(),
        1
    );
    assert_eq!(chain.stats().quarantined, 0);
    let resolver =
        editchain_store::BlobReader::open(&tmp.path().join(".editchain")).expect("blob reader");
    let (op, _) = chain
        .located_ops()
        .find(|(op, _)| matches!(op.kind, OpKind::Import(_)))
        .expect("capture op");
    let OpKind::Import(import) = &op.kind else {
        panic!("capture uses import envelope")
    };
    let Payload::Blob(blob) = &import.raw_ref else {
        panic!("capture payload must be durable")
    };
    let raw: Value =
        serde_json::from_slice(&resolver.resolve_content(blob.id).expect("resolve event"))
            .expect("event JSON");
    assert_eq!(raw["source"], "vscode.editor");
    assert_eq!(raw["event"]["sequence"], 1);
    let editchain_core::ContentId::Hash256(hash) = blob.id else {
        panic!("content-addressed capture")
    };
    let blobs =
        editchain_store::BlobStore::new(tmp.path().join(".editchain/blobs")).expect("blob store");
    let location = blobs.path_for(&hash);
    std::fs::remove_file(&location).expect("simulate missing blob");
    let retry = batch(tmp.path(), vec![start()]);
    assert_eq!(live_request(&mut restarted, retry.clone())["replayed"], 1);
    assert!(
        resolver.resolve_content(blob.id).is_some(),
        "retry restores the durable payload before acknowledging"
    );
    std::fs::write(location, b"corrupt").expect("simulate corrupt blob");
    assert!(
        restarted
            .handle(&Request {
                id: 3,
                body: serde_json::from_value(retry).expect("request")
            })
            .is_err(),
        "corrupt retained content must not receive an acknowledgement"
    );
}

#[test]
fn human_work_counts_distinct_ai_lines_with_disjoint_exposures_and_unsaved_edits() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let before = "const one = '😀';\nconst two = 2;\nconst three = 3;\n";
    let after = "const one = '😀';\nconst two = 20;\nconst three = 3;\n";
    seed_ai(tmp.path(), before);
    let exposure = |sequence, duration, ranges| {
        event(
            sequence,
            json!({"type":"code_exposure","document":document(1),
        "editor":"split-1","ranges":ranges,"started_ms":2000,"duration_ms":duration}),
        )
    };
    let ranges = json!([{"start":[0,0],"end":[1,0]},{"start":[2,0],"end":[3,0]}]);
    let offset = before.find("2;").expect("edit location") + 1;
    let utf16_offset = before
        .get(..offset)
        .expect("valid UTF8 fixture boundary")
        .encode_utf16()
        .count();
    let events = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":before}),
        ),
        exposure(3, 2500, ranges.clone()),
        exposure(4, 3000, ranges),
        exposure(5, 100, json!([{"start":[1,0],"end":[2,0]}])),
        event(
            6,
            json!({"type":"document_changed","document":document(2),"before_version":1,"before":before,"after":after,
            "reason":null,"changes":[{"offset":utf16_offset,"length":0,"text":"0"}]}),
        ),
        event(
            7,
            json!({"type":"human_edit","change":6,"signal":"keyboard_selection"}),
        ),
    ];
    let mut server = editchain_node::Server::new();
    let request = batch(tmp.path(), events);
    assert_eq!(live_request(&mut server, request.clone())["accepted"], 7);
    assert_eq!(live_request(&mut server, request)["replayed"], 7);
    let query = json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}});
    let report = live_request(&mut server, query.clone());
    assert_eq!(report["ai_lines"], 3, "known saved AI file is denominator");
    assert_eq!(
        report["read_lines"], 2,
        "repeated exposure is a union and folded gap is excluded"
    );
    assert_eq!(
        report["exposed_lines"], 3,
        "brief visibility stays an exposure indicator"
    );
    assert_eq!(
        report["historical_ai_lines_edited"], 1,
        "unsaved typing counts as work"
    );
    std::fs::write(tmp.path().join("ai.txt"), after).expect("save human buffer");
    let report = live_request(&mut editchain_node::Server::new(), query);
    assert_eq!(
        report["ai_lines"], 3,
        "human modified descendant preserves AI origin"
    );
    assert_eq!(report["edited_lines"], 1);
    assert_eq!(report["read_and_edited_lines"], 0);
    assert_eq!(report["capture_gaps"], 0);
}

#[test]
fn editor_invalid_utf16_replay_is_rejected_before_writing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let body = batch(
        tmp.path(),
        vec![
            start(),
            event(
                2,
                json!({"type":"document_changed","document":document(2),"before_version":1,
        "before":"😀x","after":"😀!x","changes":[{"offset":4,"length":0,"text":"!"}],"reason":null}),
            ),
        ],
    );
    let mut server = editchain_node::Server::new();
    let response = server
        .handle(&Request {
            id: 1,
            body: serde_json::from_value(body).expect("request"),
        })
        .expect("structured validation error");
    assert!(
        matches!(response.body, editchain_protocol::ResponseBody::Error(_)),
        "byte offsets must not be mistaken for UTF16 offsets"
    );
    assert!(
        !tmp.path().join(".editchain").exists(),
        "validation precedes all writes"
    );
}

#[test]
fn rename_after_destination_open_preserves_reading_and_the_first_human_edit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let before = "ai line\n";
    let after = "ai line human\n";
    seed_ai(tmp.path(), before);
    let renamed = |version| json!({"id":"destination","uri":"file:///renamed.txt","path":"renamed.txt","version":version});
    let events = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":before}),
        ),
        event(
            3,
            json!({"type":"document_snapshot","document":renamed(1),"text":before}),
        ),
        event(
            4,
            json!({"type":"document_renamed","from":"ai.txt","to":"renamed.txt"}),
        ),
        event(
            5,
            json!({"type":"code_exposure","document":renamed(1),"editor":"view","ranges":[{"start":[0,0],"end":[1,0]}],"started_ms":2004,"duration_ms":3000}),
        ),
        event(
            6,
            json!({"type":"document_changed","document":renamed(2),"before_version":1,"before":before,"after":after,
            "reason":null,"changes":[{"offset":7,"length":0,"text":" human"}]}),
        ),
        event(
            7,
            json!({"type":"human_edit","change":6,"signal":"keyboard_selection"}),
        ),
    ];
    let mut server = editchain_node::Server::new();
    let _recorded = live_request(&mut server, batch(tmp.path(), events));
    std::fs::rename(tmp.path().join("ai.txt"), tmp.path().join("renamed.txt"))
        .expect("rename fixture");
    std::fs::write(tmp.path().join("renamed.txt"), after).expect("save edit");
    let report = live_request(
        &mut server,
        json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}}),
    );
    assert_eq!(report["ai_lines"], 1);
    assert_eq!(report["read_lines"], 1);
    assert_eq!(report["edited_lines"], 1);
    assert_eq!(report["files"][0]["path"], "renamed.txt");
}

#[test]
fn automatic_edits_do_not_mark_human_work_and_deleted_ai_lines_remain_historical() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let before = "ai one\nai two\n";
    seed_ai(tmp.path(), before);
    let mut server = editchain_node::Server::new();
    let events = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":before}),
        ),
        event(
            3,
            json!({"type":"document_changed","document":document(2),"before_version":1,"before":before,"after":"ai two\n",
            "changes":[{"offset":0,"length":7,"text":""}],"reason":null}),
        ),
        event(
            4,
            json!({"type":"human_edit","change":3,"signal":"keyboard_selection"}),
        ),
        event(
            5,
            json!({"type":"document_changed","document":document(3),"before_version":2,"before":"ai two\n","after":"external\n",
            "changes":[{"offset":0,"length":7,"text":"external\n"}],"reason":null}),
        ),
    ];
    let _result = live_request(&mut server, batch(tmp.path(), events));
    std::fs::write(tmp.path().join("ai.txt"), "external\n").expect("external write");
    let report = live_request(
        &mut server,
        json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}}),
    );
    assert_eq!(report["human_changes"], 1);
    assert_eq!(report["historical_ai_lines_edited"], 1);
    assert_eq!(
        report["ai_lines"], 0,
        "unattributed replacements cannot inherit removed AI origins"
    );
}
