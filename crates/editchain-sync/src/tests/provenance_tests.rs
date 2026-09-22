//! A locally retained baseline record stays locally authored after a peer
//! independently supplies its exact bytes. The editor must keep deriving from
//! it, or losing the derived cache would quarantine correct history.

use editchain_core::human::HumanWorkRecord;
use editchain_core::op::ImportOp;
use editchain_protocol::editor::{EditorEvent, RecordEditorEvents};
use editchain_protocol::{Request, RequestBody, ResponseBody};
use editchain_store::{read_encoded_at, BlobReader};

use super::*;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";

fn document() -> serde_json::Value {
    serde_json::json!({ "id": "buffer", "uri": "file:///a.rs", "path": "a.rs", "version": 1 })
}

fn event(sequence: u64, time_ms: u64, payload: &serde_json::Value) -> io::Result<EditorEvent> {
    serde_json::from_value(serde_json::json!({
        "schema": 1, "session": SESSION, "sequence": sequence, "time_ms": time_ms,
        "identity": { "kind": "unsigned", "guid": SESSION,
                      "stream": "aaaaaaaaaaaaaaaaaaaaaaaa" },
        "event": payload,
    }))
    .map_err(io::Error::other)
}

fn snapshot_event(text: &str) -> io::Result<EditorEvent> {
    event(
        2,
        12,
        &serde_json::json!({"type":"document_snapshot", "document":document(), "text":text}),
    )
}

fn read_event(sequence: u64) -> io::Result<EditorEvent> {
    event(
        sequence,
        2000_u64.saturating_add(sequence),
        &serde_json::json!({"type":"code_read", "document":document(), "editor":"view",
            "ranges":[{"start":[0,0],"end":[0,10]}], "started_ms":0, "duration_ms":3000}),
    )
}

fn request(workspace: &Path, events: Vec<EditorEvent>) -> Request {
    Request {
        id: 1,
        body: RequestBody::RecordEditorEvents(RecordEditorEvents {
            workspace_path: workspace.to_string_lossy().into_owned(),
            chain_dir: ".editchain".into(),
            events,
        }),
    }
}

fn accepted(server: &mut editchain_node::Server, request: &Request) -> io::Result<u64> {
    let response = server
        .handle(request)
        .map_err(|error| io::Error::other(error.to_string()))?;
    match response.body {
        ResponseBody::Ok(value) => value
            .get("accepted")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| io::Error::other("missing accepted count")),
        ResponseBody::Error(error) => Err(io::Error::other(format!("{error:?}"))),
    }
}

fn work(root: &Path, sequence: u64) -> io::Result<HumanWorkRecord> {
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
    Err(io::Error::other("expected accepted human work"))
}

/// Exact durable bytes of the recorded document snapshot, found by payload.
fn local_snapshot(root: &Path) -> io::Result<(RecordKey, Vec<u8>)> {
    let chain = CanonicalChain::read(root)?;
    let reader = BlobReader::open(root)?;
    for (op, location) in chain.located_ops() {
        let OpKind::Import(import) = &op.kind else {
            continue;
        };
        let Payload::Blob(blob) = &import.raw_ref else {
            continue;
        };
        let Some(raw) = reader.resolve_content(blob.id) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            continue;
        };
        if value.get("source").and_then(serde_json::Value::as_str) != Some("vscode.editor")
            || value
                .pointer("/event/event/type")
                .and_then(serde_json::Value::as_str)
                != Some("document_snapshot")
        {
            continue;
        }
        let location = location.ok_or_else(|| io::Error::other("missing record location"))?;
        let encoded = read_encoded_at(root, location)?;
        return Ok((RecordKey::from_encoded(&encoded)?, encoded));
    }
    Err(io::Error::other("missing local snapshot source"))
}

/// A peer-authored record whose raw payload never reached this device.
fn foreign_source() -> io::Result<(RecordKey, Vec<u8>)> {
    let raw = serde_json::to_vec(&serde_json::json!({
        "source": "vscode.editor",
        "event": {"schema": 1, "session": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                  "sequence": 1, "time_ms": 30,
                  "event": {"type": "tracking_started", "dwell_ms": 2000,
                            "vscode_version": "1.85.0", "activity_schema": 3}},
    }))
    .map_err(io::Error::other)?;
    let hash = *blake3::hash(&raw).as_bytes();
    let op = Op {
        id: OpId::new(NodeId(7), 1, 99),
        parents: ParentSet::None,
        actor: ActorId(7),
        clock: Clock::UnixMs(30),
        scope: ScopeRef::None,
        tags: Tags::IMPORT | Tags::HUMAN,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Blob(BlobRef {
                id: ContentId::Hash256(hash),
                len: u32::try_from(raw.len()).map_err(io::Error::other)?,
            }),
            raw_hash: Some(hash),
        }),
    };
    let encoded = encode_op(&op).map_err(io::Error::other)?;
    Ok((RecordKey::from_encoded(&encoded)?, encoded))
}

#[test]
fn echoed_local_baseline_keeps_capture_across_a_cold_rebuild() -> io::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("ws");
    std::fs::create_dir_all(&workspace)?;
    let root = workspace.join(".editchain");
    let mut server = editchain_node::Server::new();
    let start = event(
        1,
        10,
        &serde_json::json!({"type":"tracking_started", "dwell_ms":2000,
            "vscode_version":"1.85.0", "activity_schema":3}),
    );
    check_eq!(
        accepted(
            &mut server,
            &request(
                &workspace,
                vec![start?, snapshot_event("local text")?, read_event(3)?]
            ),
        )?,
        3,
        "the local baseline records"
    );
    let frontier = work(&root, 3)?.after;
    check!(
        frontier.is_some(),
        "fixture has an accepted local snapshot frontier"
    );

    let (key, encoded) = local_snapshot(&root)?;
    let replica = Replica::open(&root, "space-1", false)?;
    check_eq!(
        replica.snapshot()?.len(),
        0,
        "consent withholds the baseline until a peer supplies it"
    );
    check_eq!(
        replica.ingest_records(&[(key, encoded)])?,
        vec![key],
        "the peer's exact copy of the local baseline is acknowledged"
    );
    check!(
        replica.snapshot()?.contains(key),
        "echoed evidence becomes shareable"
    );
    let (foreign_key, foreign_encoded) = foreign_source()?;
    check_eq!(
        replica.ingest_records(&[(foreign_key, foreign_encoded)])?,
        vec![foreign_key],
        "a genuinely foreign source enters shared evidence"
    );

    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("multiplayer/scope.json"))?)?;
    let local = ledger
        .get("local")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| io::Error::other("missing local provenance"))?;
    check!(
        local.contains(&serde_json::to_value(key).map_err(io::Error::other)?),
        "the echoed baseline keeps its local provenance"
    );
    check!(
        !local.contains(&serde_json::to_value(foreign_key).map_err(io::Error::other)?),
        "a foreign record never gains local provenance"
    );
    drop(replica);

    // Simulate loss of the derived cache, retaining every authoritative record.
    // The foreign source has no local bytes; capture must skip it, not fail.
    std::fs::remove_dir_all(root.join("editor-v1"))?;
    check_eq!(
        accepted(&mut server, &request(&workspace, vec![read_event(4)?]),)?,
        1,
        "cold recovery continues capture past the echoed baseline"
    );
    check_eq!(
        work(&root, 4)?.after,
        frontier,
        "the accepted local snapshot still feeds later rows after a rebuild"
    );
    check_eq!(
        CanonicalChain::read(&root)?.stats().quarantined,
        0,
        "re-deriving the local snapshot did not quarantine a correct row"
    );
    Ok(())
}
