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
use editchain_codec::frame::encode_op;
use editchain_core::{NoteRelationship, Op, OpId, OpKind, ParentSet, Payload, Tags};
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

fn payload_fingerprint(op: &Op) -> Option<String> {
    let OpKind::Note(note) = &op.kind else {
        return None;
    };
    let Payload::Inline(evidence) = &note.content else {
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

fn assistant_line(blocks: &serde_json::Value) -> String {
    serde_json::json!({
        "type": "assistant", "uuid": "assistant-1", "sessionId": "session-1",
        "timestamp": "2026-07-10T00:00:01.000Z",
        "message": { "role": "assistant", "model": "model", "content": blocks },
    })
    .to_string()
        + "\n"
}

fn content_bytes(ops: &[Op]) -> std::collections::BTreeMap<OpId, Vec<u8>> {
    ops.iter()
        .filter(|op| {
            !op.tags.matches_any(Tags::PRIVATE)
                && matches!(
                    op.kind,
                    OpKind::Message(_) | OpKind::Tool(_) | OpKind::Command(_)
                )
        })
        .map(|op| (op.id, encode_op(op).unwrap()))
        .collect()
}

#[test]
fn reasoning_backfill_preserves_public_block_bytes_and_source_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    let line = assistant_line(&serde_json::json!([
        { "type": "thinking", "thinking": "private reasoning" },
        { "type": "text", "text": "public first" },
        { "type": "tool_use", "id": "call-1", "name": "Bash", "input": { "command": "pwd" } },
        { "type": "text", "text": "public last" },
    ]));
    std::fs::write(&path, &line).unwrap();
    let mut ops = MemoryOpSink::new();
    let mut cursors = MemoryCursorStore::new();
    let _report = import(dir.path(), &mut ops, &mut cursors);
    let public = content_bytes(&ops.ops);
    assert_eq!(public.len(), 4);
    assert!(!ops.ops.iter().any(|op| op.tags.matches_any(Tags::PRIVATE)));
    let options = ImportOptions {
        include_thinking: true,
        ..ImportOptions::default()
    };
    let report = import_claude_code(
        &request(dir.path()),
        &options,
        &mut ops,
        &mut MemoryBlobSink::new(),
        &mut cursors,
    )
    .unwrap();
    assert_eq!(report.raw_ops, 0);
    assert_eq!(report.normalized_ops, 1);
    assert_eq!(report.duplicates, 4);
    assert_eq!(content_bytes(&ops.ops), public);
    assert_eq!(
        ops.ops
            .iter()
            .filter(|op| op.tags.matches_any(Tags::PRIVATE))
            .count(),
        1
    );
    let key = source_key(dir.path(), &path);
    let checkpoint = cursors
        .get_cursor(&key)
        .unwrap()
        .unwrap()
        .materialization
        .unwrap();
    assert_eq!(checkpoint.contract, "claude-blocks-v1");
    assert!(checkpoint.includes_thinking);
    let _report = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(
        ops.ops
            .iter()
            .filter(|op| op.tags.matches_any(Tags::PRIVATE))
            .count(),
        1
    );

    let mut reversed = ops.ops.clone();
    reversed.reverse();
    let forward = HistoryProjection::from_ops(ops.ops).nodes();
    let backward = HistoryProjection::from_ops(reversed).nodes();
    let rows = |rows: &[editchain_project::HistoryNode]| {
        rows.iter()
            .map(|row| (row.node_key(), row.summary()))
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&forward), rows(&backward));
    assert_eq!(forward.len(), 1);
    assert_eq!(forward.first().unwrap().summary(), "private reasoning");
}

#[test]
fn named_claude_materialization_is_independent_of_append_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    let lines = [
        event_line("user-1", None, "hello"),
        assistant_line(&serde_json::json!([
            { "type": "thinking", "thinking": "reasoning" },
            { "type": "text", "text": "response" },
        ])),
        "malformed record\n".to_owned(),
    ];
    let mut expected = None;
    for boundaries in [vec![3], vec![1, 2, 3], vec![1, 3]] {
        let mut ops = MemoryOpSink::new();
        let mut cursors = MemoryCursorStore::new();
        for through in boundaries {
            std::fs::write(
                &path,
                lines.iter().take(through).cloned().collect::<String>(),
            )
            .unwrap();
            let _report = import(dir.path(), &mut ops, &mut cursors);
        }
        let actual: std::collections::BTreeMap<_, _> = ops
            .ops
            .iter()
            .map(|op| (op.id, encode_op(op).unwrap()))
            .collect();
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
}

#[test]
fn raw_only_append_and_failed_content_backfill_preserve_accepted_coverage() {
    struct RawOnlyBlobs {
        raw: Vec<u8>,
    }
    impl BlobSink for RawOnlyBlobs {
        fn store_blob(&mut self, bytes: &[u8]) -> Result<(), ImportError> {
            if bytes == self.raw {
                Ok(())
            } else {
                Err(ImportError::BlobSink("derived blob failed".into()))
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    let large = "x".repeat(INLINE_LIMIT.saturating_add(1));
    let source = assistant_line(&serde_json::json!([
        { "type": "text", "text": large },
        { "type": "tool_use", "id": "call-1", "name": "Bash", "input": { "command": large } },
    ]));
    std::fs::write(&path, &source).unwrap();
    let key = source_key(dir.path(), &path);
    let mut cursors = MemoryCursorStore::new();
    let mut ops = MemoryOpSink::new();
    let mut blobs = ContentAddressedBlobSink::new();
    let raw_only = ImportOptions {
        normalize: false,
        ..ImportOptions::default()
    };
    let _report = import_claude_code(
        &request(dir.path()),
        &raw_only,
        &mut ops,
        &mut blobs,
        &mut cursors,
    )
    .unwrap();
    let accepted = cursors.get_cursor(&key).unwrap().unwrap();
    assert_eq!(accepted.materialization, None);
    let raw_bytes: Vec<_> = ops.ops.iter().map(|op| encode_op(op).unwrap()).collect();
    let result = import_claude_code(
        &request(dir.path()),
        &ImportOptions::default(),
        &mut ops,
        &mut RawOnlyBlobs {
            raw: source.as_bytes().to_vec(),
        },
        &mut cursors,
    );
    assert!(matches!(result, Err(ImportError::BlobSink(_))));
    assert_eq!(cursors.get_cursor(&key).unwrap(), Some(accepted));
    assert_eq!(
        ops.ops
            .iter()
            .map(|op| encode_op(op).unwrap())
            .collect::<Vec<_>>(),
        raw_bytes
    );
    let retry = import_claude_code(
        &request(dir.path()),
        &ImportOptions::default(),
        &mut ops,
        &mut blobs,
        &mut cursors,
    )
    .unwrap();
    assert_eq!(retry.raw_ops, 0);
    let payloads: Vec<_> = ops
        .ops
        .iter()
        .filter_map(|op| match &op.kind {
            OpKind::Message(message) => Some(&message.content),
            OpKind::Tool(tool) => Some(&tool.content),
            OpKind::Command(command) => Some(&command.content),
            OpKind::ChainStart(_)
            | OpKind::Actor(_)
            | OpKind::File(_)
            | OpKind::Reflection(_)
            | OpKind::Import(_)
            | OpKind::Note(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_) => None,
        })
        .collect();
    assert_eq!(payloads.len(), 3);
    for payload in payloads {
        let reference = if let Payload::Blob(reference) = payload {
            Some(reference)
        } else {
            None
        }
        .unwrap();
        let hash = if let editchain_core::ContentId::Hash256(hash) = reference.id {
            Some(hash)
        } else {
            None
        }
        .unwrap();
        let bytes = blobs.get(&hash).unwrap();
        assert_eq!(bytes.len(), usize::try_from(reference.len).unwrap());
        assert!(bytes
            .windows(large.len())
            .any(|window| window == large.as_bytes()));
    }
    let checkpoint = cursors
        .get_cursor(&key)
        .unwrap()
        .unwrap()
        .materialization
        .unwrap();
    assert_eq!(checkpoint.through, 1);
    let appended = event_line("user-2", Some("assistant-1"), "follow up");
    std::fs::write(&path, source + &appended).unwrap();
    let _report = import_claude_code(
        &request(dir.path()),
        &raw_only,
        &mut ops,
        &mut blobs,
        &mut cursors,
    )
    .unwrap();
    assert_eq!(
        cursors.get_cursor(&key).unwrap().unwrap().materialization,
        Some(checkpoint)
    );
    let backfill = import_claude_code(
        &request(dir.path()),
        &ImportOptions::default(),
        &mut ops,
        &mut blobs,
        &mut cursors,
    )
    .unwrap();
    assert_eq!(backfill.raw_ops, 0);
    assert_eq!(
        cursors
            .get_cursor(&key)
            .unwrap()
            .unwrap()
            .materialization
            .unwrap()
            .through,
        2
    );
    let second_raw = derive_keyed_source_stream(&key, 0)
        .op_from_position(SourcePosition::raw(2))
        .unwrap();
    assert!(ops.ops.iter().any(|op| op.parents == ParentSet::One(second_raw)
        && matches!(&op.kind, OpKind::Note(note) if note.relationship == NoteRelationship::ProviderParent)),
        "raw-only gaps also require provider topology backfill");
}

#[test]
fn legacy_content_remains_stored_and_complete_manifests_control_replacement() {
    use editchain_import::claude_code::envelope::parse_envelope;
    use editchain_import::claude_code::normalize::{normalize_envelope, NormalizeOptions};
    use editchain_import::sink::OpSink as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    let source = event_line("event-1", None, "public text");
    std::fs::write(&path, &source).unwrap();
    let key = source_key(dir.path(), &path);
    let stream = derive_keyed_source_stream(&key, 0);
    let (raw, legacy) = normalize_envelope(
        &parse_envelope(source.as_bytes()).unwrap(),
        editchain_import::hash_raw(source.as_bytes()),
        source.as_bytes(),
        &stream,
        1,
        &NormalizeOptions::default(),
        &mut MemoryBlobSink::new(),
        "session-1",
    )
    .unwrap();
    let legacy_id = legacy.first().unwrap().id;
    let legacy_bytes = encode_op(legacy.first().unwrap()).unwrap();
    let mut ops = MemoryOpSink::new();
    for op in std::iter::once(raw).chain(legacy) {
        let _admission = ops.accept_op(&op).unwrap();
    }
    let (_, _, mut cursor) = read_session_file(&path, None).unwrap();
    cursor.normalization_version = CLAUDE_NORMALIZATION_VERSION;
    let mut cursors = MemoryCursorStore::new();
    cursors.set_cursor(&key, &cursor).unwrap();
    let report = import(dir.path(), &mut ops, &mut cursors);
    assert_eq!(report.raw_ops, 0);
    assert_eq!(report.normalized_ops, 1);
    assert_eq!(report.evidence_ops, 1);
    let modern = ops
        .ops
        .iter()
        .find(|op| op.id != legacy_id && matches!(op.kind, OpKind::Message(_)))
        .unwrap();
    let modern_id = modern.id;
    assert_ne!(modern.id.node, stream.node);
    assert_eq!(
        encode_op(ops.ops.iter().find(|op| op.id == legacy_id).unwrap()).unwrap(),
        legacy_bytes
    );
    let projection = HistoryProjection::from_ops(ops.ops.clone());
    assert!(projection.ops().iter().any(|op| op.id == legacy_id));
    assert_eq!(projection.nodes().first().unwrap().summary(), "public text");
    let incomplete: Vec<_> = ops
        .ops
        .iter()
        .filter(|op| op.id != modern_id)
        .cloned()
        .collect();
    let incomplete = HistoryProjection::from_ops(incomplete);
    assert_ne!(
        incomplete.nodes().first().unwrap().summary(),
        "public text",
        "incomplete materialization cannot revive the legacy content"
    );
    let mut cursor = cursors.get_cursor(&key).unwrap().unwrap();
    cursor.materialization.as_mut().unwrap().contract = "claude-blocks-v99".into();
    cursors.set_cursor(&key, &cursor).unwrap();
    let count = ops.ops.len();
    assert!(matches!(
        import_claude_code(
            &request(dir.path()),
            &ImportOptions::default(),
            &mut ops,
            &mut MemoryBlobSink::new(),
            &mut cursors
        ),
        Err(ImportError::CursorStore(_))
    ));
    assert_eq!(ops.ops.len(), count);
    assert_eq!(cursors.get_cursor(&key).unwrap(), Some(cursor));
}

#[test]
fn content_blocks_beyond_legacy_lane_capacity_keep_unique_occurrence_ids() {
    use editchain_import::claude_code::envelope::parse_envelope;
    use editchain_import::claude_code::normalize::{normalize_envelope, NormalizeOptions};

    let count = usize::from(u16::MAX).saturating_add(1);
    let blocks = vec![serde_json::json!({ "type": "text", "text": "x" }); count];
    let source = assistant_line(&serde_json::Value::Array(blocks));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    std::fs::write(&path, &source).unwrap();
    let key = source_key(dir.path(), &path);
    let stream = derive_keyed_source_stream(&key, 0);
    let legacy = normalize_envelope(
        &parse_envelope(source.as_bytes()).unwrap(),
        editchain_import::hash_raw(source.as_bytes()),
        source.as_bytes(),
        &stream,
        1,
        &NormalizeOptions::default(),
        &mut MemoryBlobSink::new(),
        "session-1",
    );
    assert!(
        matches!(legacy, Err(ImportError::OpSink(_))),
        "legacy lanes fail before wrapping"
    );
    let mut ops = MemoryOpSink::new();
    let report = import(dir.path(), &mut ops, &mut MemoryCursorStore::new());
    assert_eq!(report.op_conflicts, 0);
    assert_eq!(content_bytes(&ops.ops).len(), count);
    assert!(ops
        .ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::Message(_)))
        .all(|op| op.id.node != stream.node));
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
                    && matches!(&import.raw_ref, Payload::Blob(blob)
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
fn unchanged_legacy_cursor_replays_topology_and_backfills_named_content() {
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
    assert_eq!(report.normalized_ops, 5);
    assert_eq!(report.evidence_ops, 2);
    assert_eq!(
        ops.ops
            .iter()
            .filter(|op| matches!(op.kind, OpKind::Message(_)))
            .count(),
        2
    );
    assert_eq!(ops.ops.iter().filter(|op| matches!(&op.kind, OpKind::Note(note)
        if matches!(note.relationship, NoteRelationship::OccurrenceOf | NoteRelationship::ProviderParent))).count(), 3);
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
fn version_two_cursor_supplements_topology_and_backfills_named_content() {
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
    assert_eq!(report.normalized_ops, 4);
    assert_eq!(report.evidence_ops, 2);
    let topology: Vec<_> = ops
        .ops
        .iter()
        .filter(|op| {
            matches!(&op.kind, OpKind::Note(note)
        if note.relationship != NoteRelationship::ProviderEvidence)
        })
        .collect();
    assert_eq!(topology.len(), 2);
    assert!(topology.into_iter().all(|op| {
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
