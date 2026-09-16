use super::{record, Encoding, Result};
use editchain_core::{human::HumanWorkRecord, Clock, Op, OpKind, Payload};
use editchain_protocol::editor::{EditorEvent, RecordEditorEvents};
use editchain_store::{
    durable::atomic_write,
    format::{encode_op, Page},
    CanonicalChain, SegmentStore,
};
use serde_json::{json, Value};
use std::path::Path;

fn check(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn event(sequence: u64, payload: &Value) -> Result<EditorEvent> {
    Ok(serde_json::from_value(json!({
        "schema": 1, "session": "11111111-1111-4111-8111-111111111111",
        "sequence": sequence, "time_ms": 2000_u64.saturating_add(sequence),
        "identity": { "kind": "unsigned", "guid": "local", "stream": "workspace" },
        "event": payload,
    }))?)
}

fn request(root: &Path, events: Vec<EditorEvent>) -> RecordEditorEvents {
    RecordEditorEvents {
        workspace_path: root.to_string_lossy().into_owned(),
        chain_dir: ".editchain".into(),
        events,
    }
}

fn conflict(root: &Path, original: &Op) -> Result<()> {
    let mut variant = original.clone();
    variant.clock = Clock::UnixMs(9999);
    let encoded = encode_op(&variant)?;
    let ledger = root.join("multiplayer/scope.json");
    std::fs::create_dir_all(root.join("multiplayer"))?;
    atomic_write(
        &ledger,
        &serde_json::to_vec(&json!({ "version": 1, "received": [
        { "id": variant.id, "digest": blake3::hash(&encoded).as_bytes() }
    ] }))?,
    )?;
    let mut page = Page::new(0);
    page.add_record(0, encoded);
    SegmentStore::open(root)?.append_page(&page)?;
    Ok(())
}

fn receive_original(root: &Path, source: &Op) -> Result<()> {
    let ledger = root.join("multiplayer/scope.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&ledger)?)?;
    let received = value
        .get_mut("received")
        .and_then(Value::as_array_mut)
        .ok_or("receipts")?;
    received
        .push(json!({ "id": source.id, "digest": blake3::hash(&encode_op(source)?).as_bytes() }));
    atomic_write(&ledger, &serde_json::to_vec(&value)?)?;
    Ok(())
}

fn work(root: &Path, sequence: u64) -> Result<HumanWorkRecord> {
    for (op, _) in CanonicalChain::read(root)?.located_ops() {
        if let OpKind::Import(import) = &op.kind {
            if let Payload::Inline(bytes) = &import.raw_ref {
                if let Ok(work) = serde_json::from_slice::<HumanWorkRecord>(bytes) {
                    if work.source_event.seq == sequence {
                        return Ok(work);
                    }
                }
            }
        }
    }
    Err("expected accepted human work".into())
}

#[test]
fn local_source_conflict_recovers_capture_without_reusing_quarantined_snapshot() -> Result<()> {
    recover_snapshot(false)
}

#[test]
fn independently_received_baseline_still_invalidates_a_cached_local_snapshot() -> Result<()> {
    recover_snapshot(true)
}

fn recover_snapshot(received_baseline: bool) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join(".editchain");
    let document = json!({ "id": "buffer", "uri": "file:///a.rs", "path": "a.rs", "version": 1 });
    let start = event(
        1,
        &json!({"type":"tracking_started", "dwell_ms":2000, "vscode_version":"1.85.0"}),
    )?;
    let snapshot = event(
        2,
        &json!({"type":"document_snapshot", "document":document, "text":"local text"}),
    )?;
    let read = |sequence| {
        event(
            sequence,
            &json!({"type":"code_read", "document":document,
        "editor":"view", "ranges":[{"start":[0,0],"end":[0,10]}], "started_ms":0, "duration_ms":3000}),
        )
    };
    let initial = request(temporary.path(), vec![start, snapshot, read(3)?]);
    let _result = record(&initial, &mut Encoding::default())?;
    check(
        work(&root, 3)?.after.is_some(),
        "fixture has a derived snapshot",
    )?;
    let chain = CanonicalChain::read(&root)?;
    let id = super::event_id(initial.events.get(1).ok_or("snapshot event")?)?;
    let source = chain
        .located_ops()
        .find(|(op, _)| op.id == id)
        .ok_or("local source")?
        .0;
    conflict(&root, source)?;
    if received_baseline {
        // A peer can independently provide an exact private-baseline record.
        // Its received receipt does not mean the local recorder never used it.
        receive_original(&root, source)?;
    }
    let continued = request(temporary.path(), vec![read(4)?]);
    check(
        record(&continued, &mut Encoding::default())?.get("accepted") == Some(&json!(1)),
        "capture continues after a conflict",
    )?;
    check(
        work(&root, 4)?.after.is_none(),
        "quarantined snapshot must not feed future work",
    )?;
    let chain = CanonicalChain::read(&root)?;
    let next_id = super::event_id(continued.events.first().ok_or("next event")?)?;
    let previous_id = super::event_id(initial.events.last().ok_or("previous event")?)?;
    check(
        chain.located_ops().any(|(op, _)| {
            op.id == next_id && op.parents == editchain_core::ParentSet::One(previous_id)
        }),
        "recovery settles the accepted source frontier before admitting new events",
    )?;
    check(
        record(&initial, &mut Encoding::default())?.get("replayed") == Some(&json!(3)),
        "exact retries keep their admitted parents even with received receipts",
    )?;
    check(
        CanonicalChain::read(&root)?.stats().quarantined >= 2,
        "conflict evidence remains quarantined",
    )?;
    // Simulate loss of the derived cache, retaining every authoritative record.
    std::fs::remove_dir_all(root.join("editor-v1"))?;
    let continued = request(temporary.path(), vec![read(5)?]);
    check(
        record(&continued, &mut Encoding::default())?.get("accepted") == Some(&json!(1)),
        "cold recovery continues capture",
    )?;
    check(
        work(&root, 5)?.after.is_none(),
        "cold replay also excludes the disputed snapshot",
    )
}

#[test]
fn conflict_at_recorder_frontier_does_not_create_a_false_sequence_gap() -> Result<()> {
    recover_frontier(false)
}

#[test]
fn received_variants_preserve_sequence_and_exact_retries_without_changing_identity() -> Result<()> {
    recover_frontier(true)
}

fn recover_frontier(received_baseline: bool) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join(".editchain");
    let started = event(
        1,
        &json!({"type":"tracking_started", "dwell_ms":2000, "vscode_version":"1.85.0"}),
    )?;
    let initial = request(temporary.path(), vec![started]);
    let _result = record(&initial, &mut Encoding::default())?;
    let chain = CanonicalChain::read(&root)?;
    let source = chain
        .located_ops()
        .find(|(op, _)| matches!(op.kind, OpKind::Import(_)))
        .ok_or("local source")?
        .0;
    conflict(&root, source)?;
    if received_baseline {
        receive_original(&root, source)?;
    }
    let mut altered = event(
        1,
        &json!({"type":"tracking_started", "dwell_ms":2000, "vscode_version":"1.85.0"}),
    )?;
    altered.time_ms = 42;
    check(
        record(
            &request(temporary.path(), vec![altered]),
            &mut Encoding::default(),
        )
        .is_err(),
        "quarantine cannot acknowledge a changed retry",
    )?;
    let mut other_identity = event(2, &json!({"type":"tracking_stopped"}))?;
    other_identity.identity.as_mut().ok_or("identity")?.guid = "another-person".into();
    check(
        record(
            &request(temporary.path(), vec![other_identity]),
            &mut Encoding::default(),
        )
        .is_err(),
        "quarantined content cannot permit changing recorder identity",
    )?;
    let next = request(
        temporary.path(),
        vec![event(2, &json!({"type":"tracking_stopped"}))?],
    );
    check(
        record(&next, &mut Encoding::default())?.get("accepted") == Some(&json!(1)),
        "local predecessor evidence preserves sequence admission",
    )?;
    check(
        record(&initial, &mut Encoding::default())?.get("replayed") == Some(&json!(1)),
        "quarantined local source can still acknowledge an exact retry",
    )
}
