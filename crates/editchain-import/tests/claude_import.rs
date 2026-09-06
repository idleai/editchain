//! End-to-end contracts for Claude source generations, incremental continuity,
//! and exact topology migration.
#![expect(
    clippy::unwrap_used,
    reason = "integration fixtures require setup and import success"
)]

use std::io::Write as _;
use std::path::Path;

use blake3 as _;
use editchain_core::{NoteRelationship, OpKind, ParentSet};
use editchain_import::claude_code::reader::read_session_file;
use editchain_import::claude_code::topology::CLAUDE_NORMALIZATION_VERSION;
use editchain_import::cursor::canonical_source_key;
use editchain_import::ids::{derive_keyed_source_stream, SourcePosition};
use editchain_import::import::import_claude_code;
use editchain_import::model::{DiscoveryRequest, ImportOptions};
use editchain_import::sink::{CursorStore, MemoryBlobSink, MemoryCursorStore, MemoryOpSink};
use editchain_project as _;
use proptest as _;
use serde as _;
use sha2 as _;

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
    serde_json::json!({
        "type": "user",
        "uuid": uuid,
        "parentUuid": parent,
        "sessionId": "session-1",
        "timestamp": "2026-07-10T00:00:00.000Z",
        "message": { "role": "user", "content": text },
    })
    .to_string()
        + "\n"
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
