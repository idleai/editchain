//! Sequential scans retain exact sizes and reject incomplete or conflicted input.

use super::*;
use editchain_engine::activity::{ItemId, Kind, Note, NoteKind, Operation, Original};

fn at<'a>(value: &'a Value, path: &str) -> &'a Value {
    value.pointer(path).unwrap()
}

fn operation(sequence: u64, kind: Kind) -> Op {
    Operation::new(
        OpId::new(NodeId(7), 0, sequence),
        ItemId::legacy("scan-item", sequence),
        ItemId::legacy("scan-recorder", 1),
        kind,
    )
    .into_op()
    .unwrap()
}

fn note(bytes: Vec<u8>) -> Op {
    operation(
        1,
        Kind::Note(Note {
            category: NoteKind::Comment,
            targets: Vec::new(),
            items: Vec::new(),
            version: 1,
            content: Payload::Inline(bytes),
            code: Payload::Empty,
        }),
    )
}

#[test]
fn compact_scan_preserves_exact_lengths_hashes_and_recorded_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let engine = Engine::open(&chain).unwrap();
    let original = operation(
        2,
        Kind::Original(Original {
            provider: "scan-test".into(),
            format: None,
            native: Vec::new(),
            location: None,
            bytes: Payload::Inline(vec![42; 30_000]),
            hash: None,
        }),
    );
    let values = [note(vec![65; 90_000]), original];
    for value in &values {
        let _admission = engine.append(value).unwrap();
    }
    let events = result(&chain, &["scan"], b"", 0);
    assert_eq!(events.as_array().unwrap().len(), 3);
    for (event, value) in events.as_array().unwrap().iter().zip(&values) {
        let encoded = editchain_engine::encode_op(value).unwrap();
        assert_eq!(at(event, "/type"), "record");
        let entry = at(event, "/entry");
        assert_eq!(at(entry, "/binary_bytes"), &json!(encoded.len()));
        assert_eq!(at(entry, "/record_ref/operation"), &json!(value.id));
        assert_eq!(
            at(entry, "/record_ref/record_hash"),
            &json!(blake3::hash(&encoded).as_bytes())
        );
        assert_eq!(at(entry, "/payloads_truncated"), &json!(true));
    }
    assert_eq!(at(&events, "/0/entry/payload_summary/0/2"), &json!(90_000));
    assert_eq!(at(&events, "/1/entry/payload_summary/0/2"), &json!(30_000));
    assert_eq!(
        at(&events, "/0/entry/operation/kind/Note/content/Inline")
            .as_array()
            .unwrap()
            .len(),
        16_000
    );
    assert!(at(&events, "/1/entry/operation/kind/Original/bytes/Inline")
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(at(&events, "/2/type"), "ready");
    assert_eq!(at(&events, "/2/stats/accepted"), &json!(2));
    let output = run(
        &chain,
        &[
            "scan",
            "--output",
            "jsonl",
            "--preview-bytes",
            "3",
            "--original-preview-bytes",
            "2",
        ],
        b"",
        0,
    );
    let configured: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let configured = Value::Array(configured);
    assert_eq!(
        at(&configured, "/0/entry/operation/kind/Note/content/Inline"),
        &json!([65, 65, 65])
    );
    assert_eq!(
        at(&configured, "/1/entry/operation/kind/Original/bytes/Inline"),
        &json!([42, 42])
    );
    let snapshot = engine.snapshot().unwrap();
    for value in &values {
        assert_eq!(snapshot.get(value.id), Some(value));
    }
    assert!(
        !chain.join("index-v3").exists(),
        "scan does not build an index"
    );
}

#[test]
fn failed_scans_never_emit_a_complete_summary() {
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("conflicted");
    let engine = Engine::open(&chain).unwrap();
    let _first = engine.append(&note(vec![1])).unwrap();
    let _second = engine.append(&note(vec![2])).unwrap();
    let conflict = run(&chain, &["scan"], b"", 4);
    assert!(String::from_utf8(conflict.stderr)
        .unwrap()
        .contains("Conflicting operation ID"));
    let events: Vec<Value> = serde_json::from_slice(&conflict.stdout).unwrap();
    assert!(events.iter().all(|event| at(event, "/type") != "ready"));
    let incomplete = temp.path().join("incomplete");
    let _append = Engine::open(&incomplete)
        .unwrap()
        .append(&note(vec![1]))
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(incomplete.join("000000.eclog"))
        .unwrap()
        .write_all(b"E")
        .unwrap();
    let tail = run(&incomplete, &["scan"], b"", 4);
    assert!(String::from_utf8(tail.stderr)
        .unwrap()
        .contains("Incomplete log tail"));
    let events: Vec<Value> = serde_json::from_slice(&tail.stdout).unwrap();
    assert!(events.iter().all(|event| at(event, "/type") != "ready"));
    let missing = temp.path().join("missing");
    let _missing = run(&missing, &["scan"], b"", 3);
    assert!(!missing.exists());
    let legacy = temp.path().join("legacy");
    let _append = Engine::open(&legacy)
        .unwrap()
        .append(&message(1, Payload::Empty))
        .unwrap();
    let _legacy = run(&legacy, &["scan"], b"", 2);
    let _invalid = run(&chain, &["scan", "--preview-bytes", "1048577"], b"", 2);
}
