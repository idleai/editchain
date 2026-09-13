use super::{batch, document, event, live_request, records, start};
use editchain_core::{human::HumanWorkKind, ContentId};
use editchain_node::Server;
use editchain_store::{format::encode_op, CanonicalChain};
use serde_json::{json, Value};

fn viewing(sequence: u64, kind: &str, duration: u64, ranges: &Value) -> Value {
    event(
        sequence,
        json!({"type":kind,"document":document(1),"editor":"view",
            "ranges":ranges,"started_ms":2000,"duration_ms":duration}),
    )
}

#[test]
fn qualified_read_is_revision_bound_and_replayable_while_tab_lifecycle_alone_is_not_work() {
    let tmp = tempfile::tempdir().expect("workspace");
    let text = "const one = 1;\nconst two = 2;\nconst three = 3;\n";
    super::super::seed_ai(tmp.path(), text);
    let query = json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}});
    let initial = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":text}),
        ),
        event(
            3,
            json!({"type":"editor_opened","editor":"tab","uri":"file:///ai.txt"}),
        ),
    ];
    let mut server = Server::new();
    assert_eq!(
        live_request(&mut server, batch(tmp.path(), initial))["accepted"],
        3
    );
    assert!(records(tmp.path()).is_empty());
    assert_eq!(live_request(&mut server, query.clone())["read_lines"], 0);
    let request = batch(
        tmp.path(),
        vec![
            viewing(
                4,
                "code_read",
                2000,
                &json!([{"start":[0,0],"end":[1,0]},{"start":[2,0],"end":[3,0]}]),
            ),
            event(
                5,
                json!({"type":"editor_closed","editor":"tab","uri":"file:///ai.txt"}),
            ),
            event(6, json!({"type":"tracking_stopped"})),
        ],
    );
    assert_eq!(live_request(&mut server, request.clone())["accepted"], 3);
    let human = records(tmp.path());
    assert_eq!(human.len(), 1);
    assert_eq!(human[0].1.kind, HumanWorkKind::Read);
    let revision = human[0].1.after.as_ref().expect("exact read revision");
    assert_eq!(
        revision.content,
        ContentId::Hash256(*blake3::hash(text.as_bytes()).as_bytes())
    );
    assert_eq!(revision.version, 1);
    let report = live_request(&mut server, query.clone());
    assert_eq!(report["ai_lines"], 3);
    assert_eq!(report["read_lines"], 2, "folded gaps earn no reading");
    assert_eq!(
        report["exposed_lines"], 2,
        "new capture has only qualified reads"
    );
    assert_eq!(report["capture_gaps"], 0);

    let encoded = || {
        CanonicalChain::read(&tmp.path().join(".editchain"))
            .expect("retained evidence")
            .located_ops()
            .map(|(op, _)| encode_op(op).expect("encode record"))
            .collect::<Vec<_>>()
    };
    let before = encoded();
    assert_eq!(
        before.len(),
        15,
        "two AI records + six raw/annotation pairs + one read"
    );
    let mut restarted = Server::new();
    assert_eq!(live_request(&mut restarted, request)["replayed"], 3);
    assert_eq!(encoded(), before, "restart retry is byte-identical");
    assert_eq!(live_request(&mut restarted, query), report);
}

#[test]
fn new_reads_respect_recorded_dwell_while_legacy_exposures_keep_their_original_meaning() {
    let tmp = tempfile::tempdir().expect("workspace");
    let text = "const one = 1;\nconst two = 2;\nconst three = 3;\n";
    super::super::seed_ai(tmp.path(), text);
    let mut policy = start();
    policy["event"]["dwell_ms"] = json!(3000);
    let input = vec![
        policy,
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":text}),
        ),
        viewing(3, "code_read", 2000, &json!([{"start":[0,0],"end":[1,0]}])),
        viewing(
            4,
            "code_exposure",
            2000,
            &json!([{"start":[1,0],"end":[2,0]}]),
        ),
        viewing(5, "code_read", 3000, &json!([{"start":[2,0],"end":[3,0]}])),
        viewing(
            6,
            "code_exposure",
            3000,
            &json!([{"start":[2,0],"end":[3,0]}]),
        ),
    ];
    let request = batch(tmp.path(), input);
    let mut server = Server::new();
    assert_eq!(live_request(&mut server, request.clone())["accepted"], 6);
    let human = records(tmp.path());
    assert_eq!(
        human.iter().map(|(_, work)| work.kind).collect::<Vec<_>>(),
        vec![
            HumanWorkKind::Exposure,
            HumanWorkKind::Read,
            HumanWorkKind::Read
        ]
    );
    assert_eq!(human[0].1.summary, "Brief exposure · ai.txt · 2000 ms");
    assert_eq!(human[2].1.summary, "Reading indicator · ai.txt · 3000 ms");
    let report = live_request(
        &mut server,
        json!({"GetHumanWork":{"workspace_path":tmp.path(),"chain_dir":".editchain"}}),
    );
    assert_eq!(
        report["read_lines"], 1,
        "repeat and legacy reads form a coverage union"
    );
    assert_eq!(
        report["exposed_lines"], 2,
        "an early code_read does not add skimming evidence"
    );
    assert_eq!(report["exposure_ms"], 8000);
    assert_eq!(live_request(&mut Server::new(), request)["replayed"], 6);
    let replayed = records(tmp.path());
    for ((original, _), (replayed, _)) in human.iter().zip(&replayed) {
        assert_eq!(
            encode_op(original).expect("original"),
            encode_op(replayed).expect("replayed")
        );
    }
}
