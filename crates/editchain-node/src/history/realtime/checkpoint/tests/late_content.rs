use super::pending_imports::{ledger, receipt};
use super::*;
use crate::history::realtime::content::PendingContent;
use editchain_core::{BlobRef, ContentId, OpKind, Payload};
use editchain_protocol::{FindInHistoryRequest, SyncLiveRequest};

mod codex {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/codex.rs"
    ));
}

fn externalize(payload: &mut Payload, contents: &mut Vec<Vec<u8>>) {
    if let Payload::Inline(bytes) = payload {
        let reference = BlobRef {
            id: ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
            len: u32::try_from(bytes.len()).unwrap(),
        };
        contents.push(bytes.clone());
        *payload = Payload::Blob(reference);
    }
}

fn fixture() -> (tempfile::TempDir, Vec<Vec<u8>>) {
    let text = format!("latecontentmarker {}", "streamed message ".repeat(400));
    let mut ops = codex::occurrence(1, 1, &text).unwrap();
    let mut contents = Vec::new();
    for op in &mut ops {
        if let OpKind::Import(import) = &mut op.kind {
            externalize(&mut import.raw_ref, &mut contents);
        }
        if let OpKind::Message(message) = &mut op.kind {
            externalize(&mut message.content, &mut contents);
        }
    }
    let root = tempfile::tempdir().unwrap();
    ledger(
        root.path(),
        &serde_json::json!(ops.iter().map(receipt).collect::<Vec<_>>()),
        &serde_json::json!([]),
    );
    codex::append(&root.path().join(".editchain"), &ops).unwrap();
    (root, contents)
}

fn sync(workspace: &mut LiveWorkspace) -> editchain_protocol::LiveUpdate {
    workspace
        .sync(&SyncLiveRequest {
            epoch: workspace.epoch.clone(),
            after_revision: workspace.revision,
            codex: None,
        })
        .unwrap()
}

fn assert_received_text(workspace: &mut LiveWorkspace) {
    let rows = window(workspace).unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert!(rows
        .first()
        .unwrap()
        .summary
        .starts_with("latecontentmarker "));
    let search = workspace
        .handle(&RequestBody::FindInHistory(FindInHistoryRequest {
            snapshot_id: workspace.snapshot_id.clone(),
            query: "latecontentmarker".into(),
            top_k: 10,
        }))
        .unwrap();
    let matches = if let ResponseBody::Ok(found) = search {
        found
            .get("matches")
            .and_then(serde_json::Value::as_array)
            .cloned()
    } else {
        None
    };
    assert_eq!(
        matches.unwrap().len(),
        1,
        "received text becomes searchable"
    );
}

#[test]
fn content_only_arrivals_publish_live_rows_and_search_without_replaying_records() {
    for reverse in [false, true] {
        let (root, mut contents) = fixture();
        let canonical = human_edits::canonical(root.path());
        let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        let before = window(&mut workspace).unwrap().rows;
        assert_eq!(before.first().unwrap().summary, "(no summary)");
        assert!(
            sync(&mut workspace).deltas.is_empty(),
            "waiting alone does not rerender"
        );
        if reverse {
            contents.reverse();
        }
        let mut blobs =
            editchain_store::BlobStore::new(root.path().join(".editchain/blobs")).unwrap();
        for bytes in contents {
            blobs.write(&bytes).unwrap();
            let update = sync(&mut workspace);
            assert_eq!(update.work.chain_records, 0);
            assert_eq!(update.work.occurrences, 0);
            assert_eq!(
                update.deltas.len(),
                1,
                "content arrival publishes a live delta"
            );
            assert!(
                update.work.blocks <= 1,
                "only the dependent row can be refreshed"
            );
        }
        assert_received_text(&mut workspace);
        let after = window(&mut workspace).unwrap().rows;
        assert_eq!(
            before.first().unwrap().continuity_key,
            after.first().unwrap().continuity_key
        );
        assert_eq!(before.first().unwrap().lane, after.first().unwrap().lane);
        assert!(
            sync(&mut workspace).deltas.is_empty(),
            "complete rows stop being retried"
        );
        drop(workspace);
        let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        assert_received_text(&mut reopened);
        assert_eq!(human_edits::canonical(root.path()), canonical);
    }
}

#[test]
fn content_wait_survives_restart_and_repairs_version_twelve_cached_summaries() {
    for old_cache in [false, true] {
        let (root, contents) = fixture();
        let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        assert_eq!(
            window(&mut workspace)
                .unwrap()
                .rows
                .first()
                .unwrap()
                .summary,
            "(no summary)"
        );
        if old_cache {
            workspace.content = PendingContent::default();
            let mut saved = workspace.saved();
            saved.version = 12;
            drop(
                workspace
                    .checkpoint_store
                    .commit::<_, Saved>(&saved)
                    .unwrap(),
            );
        }
        drop(workspace);
        let mut blobs =
            editchain_store::BlobStore::new(root.path().join(".editchain/blobs")).unwrap();
        for bytes in &contents {
            blobs.write(bytes).unwrap();
        }
        let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        assert!(reopened.reused_checkpoint);
        assert_received_text(&mut reopened);
        assert!(sync(&mut reopened).deltas.is_empty());
        drop(reopened);
        let mut again = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        assert_received_text(&mut again);
    }
}

#[test]
fn a_new_revision_or_removal_wins_over_late_content_from_an_old_revision() {
    use editchain_core::provider::{CodexLogicalChange, ProviderEvidence, ProviderFact};
    for remove in [false, true] {
        let (root, contents) = fixture();
        let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        let mut newer = codex::occurrence(2, 1, "newer revision").unwrap();
        if remove {
            newer.retain(|op| !matches!(op.kind, OpKind::Message(_)));
            if let OpKind::Note(note) = &mut newer.last_mut().unwrap().kind {
                if let Payload::Inline(bytes) = &mut note.content {
                    let mut evidence: ProviderEvidence = serde_json::from_slice(bytes).unwrap();
                    if let ProviderFact::CodexDerivation(meta) = &mut evidence.fact {
                        meta.outputs.clear();
                        meta.changes = vec![CodexLogicalChange::RemoveTurn {
                            turn: "ongoing-turn".into(),
                        }];
                    }
                    *bytes = serde_json::to_vec(&evidence).unwrap();
                }
            }
        }
        codex::append(&root.path().join(".editchain"), &newer).unwrap();
        let mut blobs =
            editchain_store::BlobStore::new(root.path().join(".editchain/blobs")).unwrap();
        for bytes in contents {
            blobs.write(&bytes).unwrap();
        }
        let update = sync(&mut workspace);
        assert!(!update.deltas.is_empty());
        let rows = window(&mut workspace).unwrap().rows;
        if remove {
            assert!(rows.is_empty());
        } else {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows.first().unwrap().summary, "newer revision");
        }
        assert!(
            sync(&mut workspace).deltas.is_empty(),
            "retired dependencies stop polling"
        );
        drop(workspace);
        let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        assert_eq!(
            serde_json::to_value(window(&mut reopened).unwrap().rows).unwrap(),
            serde_json::to_value(rows).unwrap()
        );
    }
}
