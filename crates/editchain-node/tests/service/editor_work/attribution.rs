use super::{batch, document, event, live_request, seed_ai, start, Request};
use editchain_core::human::HumanWorkKind;
use editchain_node::Server;
use editchain_project::human::work_record;
use editchain_store::CanonicalChain;
use serde_json::{json, Value};

fn change(sequence: u64, version: u64, before: &str, after: &str, origin: &Value) -> Value {
    event(
        sequence,
        json!({"type":"document_changed","document":document(version),"before_version":version - 1,
            "before":before,"after":after,"reason":null,"origin":origin,
            "changes":[{"offset":0,"length":before.encode_utf16().count(),"text":after}]}),
    )
}

#[test]
fn explicit_input_and_interleaved_unknown_changes_replay_without_reassigning_authorship() {
    let tmp = tempfile::tempdir().expect("attribution fixture");
    seed_ai(tmp.path(), "AI\n");
    let input = json!({"source":"cursor","kind":"type","detailed_source":"keyboard"});
    let deletion =
        json!({"source":"cursor","kind":"executeCommands","detailed_source":"deleteLeft"});
    let events = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":"AI\n"}),
        ),
        change(3, 2, "AI\n", "AIh\n", &input),
        event(
            4,
            json!({"type":"human_edit","change":3,"signal":"editor_input"}),
        ),
        change(
            5,
            3,
            "AIh\n",
            "AIh\nAGENT\n",
            &json!({"source":"unknown","name":"MainThreadTextEditor"}),
        ),
        event(6, json!({"type":"document_saved","document":document(3)})),
        change(7, 4, "AIh\nAGENT\n", "AI\nAGENT\n", &deletion),
        event(
            8,
            json!({"type":"human_edit","change":7,"signal":"editor_input"}),
        ),
        event(9, json!({"type":"document_saved","document":document(4)})),
    ];
    let request = batch(tmp.path(), events);
    let mut server = Server::new();
    assert_eq!(live_request(&mut server, request.clone())["accepted"], 9);
    std::fs::write(tmp.path().join("ai.txt"), "AI\nAGENT\n").expect("save fixture");
    let query = json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}});
    let before = live_request(&mut server, query.clone());
    assert_eq!(before["human_changes"], 2);
    assert_eq!(before["unattributed_changes"], 1);
    assert_eq!(before["historical_ai_lines_edited"], 1);
    assert_eq!(before["capture_gaps"], 0);
    let mut reopened = Server::new();
    assert_eq!(live_request(&mut reopened, request)["replayed"], 9);
    assert_eq!(live_request(&mut reopened, query), before);
    let chain = CanonicalChain::read(&tmp.path().join(".editchain")).expect("retained chain");
    let mut work: Vec<_> = chain
        .located_ops()
        .filter_map(|(op, _)| work_record(op))
        .collect();
    work.sort_by_key(|item| item.source_event.seq);
    assert_eq!(
        work.len(),
        2,
        "saves and unknown sources do not add edit rows"
    );
    assert!(work.iter().all(|item| item.kind == HumanWorkKind::Edit));
    assert_eq!(
        work.iter()
            .map(|item| item.source_event.seq)
            .collect::<Vec<_>>(),
        [4, 8]
    );
}

#[test]
fn malformed_origin_metadata_is_rejected_before_writing() {
    for origin in [
        json!({"source":""}),
        json!({"source":"x".repeat(513)}),
        json!({"source":"cursor","kind":"x".repeat(513)}),
    ] {
        let tmp = tempfile::tempdir().expect("attribution fixture");
        let request = batch(
            tmp.path(),
            vec![start(), change(2, 2, "AI", "AIh", &origin)],
        );
        let response = Server::new()
            .handle(&Request {
                id: 1,
                body: serde_json::from_value(request).expect("valid request shape"),
            })
            .expect("validation response");
        assert!(matches!(
            response.body,
            editchain_protocol::ResponseBody::Error(_)
        ));
        assert!(!tmp.path().join(".editchain").exists());
    }
}
