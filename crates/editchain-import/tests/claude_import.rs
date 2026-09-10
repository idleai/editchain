//! End-to-end contracts for Claude source generations, incremental continuity,
//! and exact topology migration.
#![expect(
    clippy::unwrap_used,
    reason = "integration fixtures require setup and import success"
)]

use std::io::Write as _;
use std::path::Path;
use time as _;

use blake3 as _;
use editchain_codec as _;
use editchain_core::{NoteRelationship, OpKind, ParentSet};
use editchain_import::claude_code::reader::read_session_file;
use editchain_import::claude_code::topology::CLAUDE_NORMALIZATION_VERSION;
use editchain_import::cursor::canonical_source_key;
use editchain_import::error::ImportError;
use editchain_import::ids::{derive_keyed_source_stream, SourcePosition};
use editchain_import::import::import_claude_code;
use editchain_import::model::{DiscoveryRequest, ImportOptions};
use editchain_import::sink::{
    BlobSink, ContentAddressedBlobSink, CursorStore, MemoryBlobSink, MemoryCursorStore,
    MemoryOpSink, INLINE_LIMIT,
};
use editchain_project::HistoryProjection;
use process_wrap as _;
use proptest as _;
use serde as _;
use sha2 as _;
use tokio as _;

fn request(root: &Path) -> DiscoveryRequest {
    DiscoveryRequest {
        workspace_path: "/workspace".into(),
        sessions_dir: root.into(),
        chain_dir: root.join("unused-chain"),
    }
}

fn import(
    root: &Path,
    ops: &mut MemoryOpSink,
    cursors: &mut MemoryCursorStore,
) -> editchain_import::model::ImportReport {
    import_claude_code(
        &request(root),
        &ImportOptions::default(),
        ops,
        &mut MemoryBlobSink::new(),
        cursors,
    )
    .unwrap()
}

fn event_line(uuid: &str, parent: Option<&str>, text: &str) -> String {
    copied_event_line(
        uuid,
        parent,
        text,
        CopyEnvelope {
            session_id: "session-1",
            cwd: "/workspace",
            slug: "seed",
            has_session_kind: true,
        },
    )
}

#[derive(Clone, Copy)]
struct CopyEnvelope<'a> {
    session_id: &'a str,
    cwd: &'a str,
    slug: &'a str,
    has_session_kind: bool,
}

fn copied_event_line(
    uuid: &str,
    parent: Option<&str>,
    text: &str,
    copy: CopyEnvelope<'_>,
) -> String {
    let mut value = serde_json::json!({
        "type": "user",
        "uuid": uuid,
        "parentUuid": parent,
        "sessionId": copy.session_id,
        "sessionKind": "bg",
        "slug": copy.slug,
        "cwd": copy.cwd,
        "timestamp": "2026-07-10T00:00:00.000Z",
        "message": { "role": "user", "content": text },
    });
    if !copy.has_session_kind {
        drop(value.as_object_mut().unwrap().remove("sessionKind"));
    }
    value.to_string() + "\n"
}

fn payload_fingerprint(op: &editchain_core::Op) -> Option<String> {
    let OpKind::Note(note) = &op.kind else {
        return None;
    };
    let editchain_core::Payload::Inline(evidence) = &note.content else {
        return None;
    };
    serde_json::from_slice::<serde_json::Value>(evidence)
        .ok()?
        .get("payloadFingerprint")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn source_key(root: &Path, path: &Path) -> String {
    canonical_source_key("claude-code", root, path).unwrap()
}

#[test]
fn incremental_append_continues_physical_source_chain_and_exact_topology() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    std::fs::write(&path, event_line("event-1", None, "one")).unwrap();
    let mut ops = MemoryOpSink::new();
    let mut cursors = MemoryCursorStore::new();

    let first_report = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(first_report.raw_ops, 1);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(event_line("event-2", Some("event-1"), "two").as_bytes())
        .unwrap();
    let second_report = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(second_report.raw_ops, 1);

    let key = source_key(dir.path(), &path);
    let stream = derive_keyed_source_stream(&key, 0);
    let first = stream.op_from_position(SourcePosition::raw(1)).unwrap();
    let second = stream.op_from_position(SourcePosition::raw(2)).unwrap();
    let second_raw = ops.ops.iter().find(|op| op.id == second).unwrap();
    assert_eq!(second_raw.parents, ParentSet::One(first));
    assert!(ops.ops.iter().any(|op| {
        matches!(
            &op.kind,
            OpKind::Note(note) if note.relationship == NoteRelationship::ProviderParent
        ) && op.parents == ParentSet::One(second)
    }));

    let third_report = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(third_report.raw_ops, 0);
    assert_eq!(third_report.normalized_ops, 0);
}

#[test]
fn blob_failure_keeps_the_accepted_prefix_and_retry_preserves_complete_raw_bytes() {
    struct FailingBlobSink;

    impl BlobSink for FailingBlobSink {
        fn store_blob(&mut self, _data: &[u8]) -> Result<(), ImportError> {
            Err(ImportError::BlobSink("blob write failed".into()))
        }
    }

    for normalize in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-1.jsonl");
        let first = event_line("event-1", None, "accepted");
        std::fs::write(&path, &first).unwrap();
        let mut ops = MemoryOpSink::new();
        let mut cursors = MemoryCursorStore::new();
        let _report = import(dir.path(), &mut ops, &mut cursors);
        let key = source_key(dir.path(), &path);
        let accepted_cursor = cursors.get_cursor(&key).unwrap();
        let accepted_count = ops.ops.len();
        let appended = event_line("event-2", Some("event-1"), &"x".repeat(INLINE_LIMIT));
        std::fs::write(&path, first + &appended).unwrap();
        let options = ImportOptions {
            normalize,
            ..ImportOptions::default()
        };

        let result = import_claude_code(
            &request(dir.path()),
            &options,
            &mut ops,
            &mut FailingBlobSink,
            &mut cursors,
        );
        assert!(matches!(result, Err(ImportError::BlobSink(_))));
        assert_eq!(cursors.get_cursor(&key).unwrap(), accepted_cursor);
        assert_eq!(ops.ops.len(), accepted_count);

        let mut blobs = ContentAddressedBlobSink::new();
        let report = import_claude_code(
            &request(dir.path()),
            &options,
            &mut ops,
            &mut blobs,
            &mut cursors,
        )
        .unwrap();
        assert_eq!(report.raw_ops, 1);
        let expected_hash = editchain_import::hash_raw(appended.as_bytes());
        assert_eq!(blobs.get(&expected_hash), Some(appended.as_bytes()));
        assert!(ops.ops.iter().any(|op| {
            matches!(&op.kind, OpKind::Import(import)
                if import.raw_hash == Some(expected_hash)
                    && matches!(&import.raw_ref, editchain_core::Payload::Blob(blob)
                        if blob.id == editchain_core::ContentId::Hash256(expected_hash)
                            && usize::try_from(blob.len).unwrap() == appended.len()))
        }));
        assert_eq!(cursors.get_cursor(&key).unwrap().unwrap().ops_emitted, 2);
    }
}

#[test]
fn identical_source_is_not_reimported_after_sessions_root_relocation() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("archive");
    let live = dir.path().join("live");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::create_dir_all(&live).unwrap();
    let archive_path = archive.join("session-1.jsonl");
    let live_path = live.join("session-1.jsonl");
    let source = event_line("event-1", None, "one");
    std::fs::write(&archive_path, &source).unwrap();
    std::fs::write(&live_path, &source).unwrap();
    let mut ops = MemoryOpSink::new();
    let mut cursors = MemoryCursorStore::new();

    let first = import(&archive, &mut ops, &mut cursors);
    assert_eq!(first.raw_ops, 1);
    let relocated = import(&live, &mut ops, &mut cursors);
    assert_eq!(relocated.files_processed, 0);
    assert_eq!(relocated.raw_ops, 0);
    assert_eq!(
        source_key(&archive, &archive_path),
        source_key(&live, &live_path)
    );
}

#[test]
fn unchanged_legacy_cursor_replays_only_exact_topology_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    std::fs::write(
        &path,
        event_line("event-1", None, "one") + &event_line("event-2", Some("event-1"), "two"),
    )
    .unwrap();
    let (_lines, _bytes, mut cursor) = read_session_file(&path, None).unwrap();
    cursor.normalization_version = 1;
    let legacy_key = path.to_string_lossy().to_string();
    let key = source_key(dir.path(), &path);
    let mut cursors = MemoryCursorStore::new();
    cursors.set_cursor(&legacy_key, &cursor).unwrap();
    let mut ops = MemoryOpSink::new();

    let report = import(dir.path(), &mut ops, &mut cursors);

    assert_eq!(report.raw_ops, 0);
    assert_eq!(report.normalized_ops, 3);
    assert!(ops.ops.iter().all(|op| matches!(op.kind, OpKind::Note(_))));
    assert_eq!(
        cursors
            .get_cursor(&key)
            .unwrap()
            .unwrap()
            .normalization_version,
        CLAUDE_NORMALIZATION_VERSION
    );
}

#[test]
fn version_two_cursor_replays_only_payload_fingerprint_supplements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    std::fs::write(
        &path,
        event_line("event-1", None, "one") + &event_line("event-2", Some("event-1"), "two"),
    )
    .unwrap();
    let (_lines, _bytes, mut cursor) = read_session_file(&path, None).unwrap();
    cursor.normalization_version = 2;
    let key = source_key(dir.path(), &path);
    let mut cursors = MemoryCursorStore::new();
    cursors.set_cursor(&key, &cursor).unwrap();
    let mut ops = MemoryOpSink::new();

    let report = import(dir.path(), &mut ops, &mut cursors);

    assert_eq!(report.raw_ops, 0);
    assert_eq!(report.normalized_ops, 2);
    assert!(ops.ops.iter().all(|op| {
        matches!(
            &op.kind,
            OpKind::Note(note) if note.relationship == NoteRelationship::OccurrenceOf
        ) && payload_fingerprint(op).is_some()
    }));
    assert_eq!(
        cursors
            .get_cursor(&key)
            .unwrap()
            .unwrap()
            .normalization_version,
        CLAUDE_NORMALIZATION_VERSION
    );

    let repeated = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(repeated.files_processed, 0);
    assert_eq!(repeated.normalized_ops, 0);
}

#[test]
fn copied_sessions_share_a_root_then_branch_at_distinct_children() {
    let dir = tempfile::tempdir().unwrap();
    let left_path = dir.path().join("left.jsonl");
    let right_path = dir.path().join("right.jsonl");
    std::fs::write(
        &left_path,
        copied_event_line(
            "shared-root",
            None,
            "shared root",
            CopyEnvelope {
                session_id: "left-session",
                cwd: "/workspace/left",
                slug: "seed-left",
                has_session_kind: true,
            },
        ) + &copied_event_line(
            "left-child",
            Some("shared-root"),
            "left continuation",
            CopyEnvelope {
                session_id: "left-session",
                cwd: "/workspace/left",
                slug: "seed-left",
                has_session_kind: true,
            },
        ),
    )
    .unwrap();
    std::fs::write(
        &right_path,
        copied_event_line(
            "shared-root",
            None,
            "shared root",
            CopyEnvelope {
                session_id: "right-session",
                cwd: "/workspace/right",
                slug: "seed-right",
                has_session_kind: false,
            },
        ) + &copied_event_line(
            "right-child",
            Some("shared-root"),
            "right continuation",
            CopyEnvelope {
                session_id: "right-session",
                cwd: "/workspace/right",
                slug: "seed-right",
                has_session_kind: false,
            },
        ),
    )
    .unwrap();
    let mut ops = MemoryOpSink::new();
    let mut cursors = MemoryCursorStore::new();

    let report = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(report.raw_ops, 4);
    let projection = HistoryProjection::from_ops(ops.ops);
    let rows = projection.nodes();

    assert_eq!(rows.len(), 3);
    let shared = rows
        .iter()
        .find(|row| row.summary() == "shared root")
        .unwrap();
    let shared_key = shared.node_key();
    for summary in ["left continuation", "right continuation"] {
        let branch = rows.iter().find(|row| row.summary() == summary).unwrap();
        assert_eq!(
            projection.lifted_parent_keys(branch),
            vec![shared_key.clone()]
        );
    }
}

#[test]
fn each_rewrite_uses_the_next_durable_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    let mut ops = MemoryOpSink::new();
    let mut cursors = MemoryCursorStore::new();

    std::fs::write(
        &path,
        event_line("event-1", None, "one") + &event_line("event-2", Some("event-1"), "two"),
    )
    .unwrap();
    let _: editchain_import::model::ImportReport = import(dir.path(), &mut ops, &mut cursors);

    std::fs::write(&path, event_line("rewrite-1", None, "r1")).unwrap();
    let first_rewrite = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(first_rewrite.raw_ops, 1);
    let key = source_key(dir.path(), &path);
    assert_eq!(cursors.get_generation(&key).unwrap(), 1);

    // Grow first so the next rewrite can be detected as another truncation.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(event_line("rewrite-1b", Some("rewrite-1"), "padding").as_bytes())
        .unwrap();
    let _: editchain_import::model::ImportReport = import(dir.path(), &mut ops, &mut cursors);
    std::fs::write(&path, event_line("rewrite-2", None, "r2")).unwrap();
    let second_rewrite = import(dir.path(), &mut ops, &mut cursors);

    assert_eq!(second_rewrite.raw_ops, 1);
    assert_eq!(cursors.get_generation(&key).unwrap(), 2);
    let stream = derive_keyed_source_stream(&key, 2);
    let generation_two_raw = stream.op_from_position(SourcePosition::raw(1)).unwrap();
    assert!(ops.ops.iter().any(|op| op.id == generation_two_raw));
}

#[test]
fn failed_rewrite_capture_preserves_both_cursor_and_generation_for_retry() {
    struct FailingBlobSink;
    impl BlobSink for FailingBlobSink {
        fn store_blob(&mut self, _bytes: &[u8]) -> Result<(), ImportError> {
            Err(ImportError::BlobSink("failed capture".into()))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    std::fs::write(&path, event_line("a", None, "first")).unwrap();
    let mut ops = MemoryOpSink::new();
    let mut cursors = MemoryCursorStore::new();
    let _report = import(dir.path(), &mut ops, &mut cursors);
    let key = source_key(dir.path(), &path);
    let accepted = cursors.get_cursor(&key).unwrap();
    let count = ops.ops.len();
    std::fs::write(&path, event_line("b", None, &"x".repeat(INLINE_LIMIT))).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            import_claude_code(
                &request(dir.path()),
                &ImportOptions::default(),
                &mut ops,
                &mut FailingBlobSink,
                &mut cursors
            ),
            Err(ImportError::BlobSink(_))
        ));
        assert_eq!(cursors.get_cursor(&key).unwrap(), accepted);
        assert_eq!(cursors.get_generation(&key).unwrap(), 0);
        assert_eq!(ops.ops.len(), count);
    }
    let retry = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(retry.raw_ops, 1);
    assert_eq!(cursors.get_generation(&key).unwrap(), 1);
}
