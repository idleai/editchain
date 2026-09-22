use super::*;
use editchain_core::OpKind;
use editchain_protocol::SyncLiveRequest;

mod codex {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/codex.rs"
    ));
}

fn receipt(op: &Op) -> serde_json::Value {
    let bytes = editchain_store::format::encode_op(op).unwrap();
    serde_json::json!({"id":op.id, "digest":blake3::hash(&bytes).as_bytes()})
}

fn ledger(root: &std::path::Path, received: &serde_json::Value, local: &serde_json::Value) {
    let directory = root.join(".editchain/multiplayer");
    std::fs::create_dir_all(&directory).unwrap();
    editchain_store::durable::atomic_write(
        &directory.join("scope.json"),
        &serde_json::to_vec(&serde_json::json!({
            "version":1, "received":received, "local":local
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn version_nine_removes_received_previews_and_later_publishes_complete_items() {
    let root = tempfile::tempdir().unwrap();
    let chain = root.path().join(".editchain");
    let ops = codex::occurrence(2, 2, "waiting for the rest of this message").unwrap();
    let (raw, rest) = ops.split_first().unwrap();
    codex::append(&chain, std::slice::from_ref(raw)).unwrap();
    let canonical = human_edits::canonical(root.path());
    // Construct version 9's cached raw preview before enabling the new
    // receipt-aware presentation policy. The canonical records never change.
    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(window(&mut workspace).unwrap().total, 1);
    workspace.rows.flush().unwrap();
    let mut saved = workspace.saved();
    saved.version = 9;
    drop(
        workspace
            .checkpoint_store
            .commit::<_, Saved>(&saved)
            .unwrap(),
    );
    drop(workspace);
    ledger(
        root.path(),
        &serde_json::json!([receipt(raw)]),
        &serde_json::json!([]),
    );

    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert!(workspace.reused_checkpoint);
    assert_eq!(
        window(&mut workspace).unwrap().total,
        0,
        "pending preview removed"
    );
    assert_eq!(human_edits::canonical(root.path()), canonical);
    assert_eq!(workspace.projection.operation(raw.id), Some(raw));
    let search = RequestBody::FindInHistory(editchain_protocol::FindInHistoryRequest {
        snapshot_id: workspace.snapshot_id.clone(),
        query: "waiting".into(),
        top_k: 10,
    });
    let found = if let ResponseBody::Ok(found) = workspace.handle(&search).unwrap() {
        Some(found)
    } else {
        None
    };
    assert_eq!(found.unwrap().get("matches"), Some(&serde_json::json!([])));
    drop(workspace);

    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(
        window(&mut workspace).unwrap().total,
        0,
        "pending survives restart"
    );
    codex::append(&chain, rest).unwrap();
    let update = workspace
        .sync(&SyncLiveRequest {
            epoch: workspace.epoch.clone(),
            after_revision: 0,
            codex: None,
        })
        .unwrap();
    assert!(!update.deltas.is_empty());
    let rows = window(&mut workspace).unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows.first().unwrap().summary,
        "waiting for the rest of this message"
    );
    assert!(rows.first().unwrap().continuity_key.starts_with("item:"));
}

#[test]
fn local_raw_fallbacks_require_exact_foreign_provenance_to_be_hidden() {
    let ops = codex::occurrence(2, 2, "local legacy import").unwrap();
    let raw = ops.first().unwrap();
    let mut other = raw.clone();
    other.actor = editchain_core::ActorId(99);
    for (received, local) in [
        (serde_json::json!([]), serde_json::json!([])),
        (serde_json::json!([receipt(&other)]), serde_json::json!([])),
        (
            serde_json::json!([receipt(raw)]),
            serde_json::json!([receipt(raw)]),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        codex::append(&root.path().join(".editchain"), std::slice::from_ref(raw)).unwrap();
        ledger(root.path(), &received, &local);
        let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        assert_eq!(window(&mut workspace).unwrap().total, 1);
    }
}

#[test]
fn legacy_received_import_waits_for_its_normalized_content() {
    let root = tempfile::tempdir().unwrap();
    let chain = root.path().join(".editchain");
    let mut ops = codex::occurrence(2, 2, "legacy received content").unwrap();
    drop(ops.pop()); // Legacy records did not carry a derivation proof.
    let (raw, rest) = ops.split_first_mut().unwrap();
    if let OpKind::Import(import) = &mut raw.kind {
        import.raw_hash = None;
    }
    ledger(
        root.path(),
        &serde_json::json!([receipt(raw)]),
        &serde_json::json!([]),
    );
    codex::append(&chain, std::slice::from_ref(raw)).unwrap();
    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(window(&mut workspace).unwrap().total, 0);
    codex::append(&chain, rest).unwrap();
    let _update = workspace
        .sync(&SyncLiveRequest {
            epoch: workspace.epoch.clone(),
            after_revision: 0,
            codex: None,
        })
        .unwrap();
    let rows = window(&mut workspace).unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows.first().unwrap().summary, "legacy received content");
    assert_eq!(rows.first().unwrap().kind, "message");
}

#[test]
fn a_complete_claude_derivation_also_releases_received_imports() {
    use editchain_core::provider::{ClaudeDerivationEvidence, ProviderEvidence, ProviderFact};
    let root = tempfile::tempdir().unwrap();
    let mut ops = codex::occurrence(2, 2, "received Claude item").unwrap();
    let proof = ops.last_mut().unwrap();
    let note = (if let OpKind::Note(note) = &mut proof.kind {
        Some(note)
    } else {
        None
    })
    .unwrap();
    let bytes = (if let editchain_core::Payload::Inline(bytes) = &mut note.content {
        Some(bytes)
    } else {
        None
    })
    .unwrap();
    let mut evidence: ProviderEvidence = serde_json::from_slice(bytes).unwrap();
    let meta = (if let ProviderFact::CodexDerivation(meta) = evidence.fact {
        Some(meta)
    } else {
        None
    })
    .unwrap();
    evidence.fact = ProviderFact::ClaudeDerivation(ClaudeDerivationEvidence {
        contract: editchain_core::provider::ClaudeDerivationContract::BlocksV1,
        outputs: meta.outputs,
        includes_thinking: false,
    });
    *bytes = serde_json::to_vec(&evidence).unwrap();
    ledger(
        root.path(),
        &serde_json::json!([receipt(ops.first().unwrap())]),
        &serde_json::json!([]),
    );
    codex::append(&root.path().join(".editchain"), &ops).unwrap();
    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert!(workspace.projection.import_ready(ops.first().unwrap().id));
    assert!(window(&mut workspace)
        .unwrap()
        .rows
        .iter()
        .any(|row| row.summary == "received Claude item"));
}
