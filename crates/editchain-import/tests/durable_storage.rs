//! Durable storage integration tests: filesystem blob/cursor persistence and
//! unchanged re-import idempotency through the Codex pipeline (Unix).
#![cfg(unix)]
#![expect(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::wildcard_enum_match_arm,
    dead_code,
    reason = "test assertions index and unwrap on known-length vectors"
)]

mod common;

use editchain_store as _;
use std::path::{Path, PathBuf};

use blake3 as _;
use editchain_codec::frame::encode_op;
use editchain_project as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;
use time as _;
use tokio as _;

use editchain_core::op::{ImportOp, OpKind};
use editchain_core::payload::{ContentId, Payload};
use editchain_core::Op;

use editchain_import::codex::{import_codex, CodexDiscoveryRequest, HelperCommand};
use editchain_import::cursor::canonical_source_key;
use editchain_import::ids::hash_raw;
use editchain_import::model::{ImportOptions, ImportReport};
use editchain_import::sink::{CursorStore, CursorValue, FsBlobSink, FsCursorStore, MemoryOpSink};

use common::*;

/// Everything collected by one durable import run.
struct DurableRun {
    /// Import report.
    report: ImportReport,
    /// Ops emitted, in order.
    ops: MemoryOpSink,
    /// Durable blob storage (re-created fresh per run, like a process restart).
    blobs: FsBlobSink,
}

fn assert_generation(ops: &[Op], generation: u32) {
    assert!(!ops.is_empty(), "capture must emit operations");
    for op in ops {
        if matches!(&op.kind, OpKind::Note(note) if note.relationship == editchain_core::NoteRelationship::ProviderEvidence)
        {
            assert_eq!(op.parents.iter().count(), 1, "evidence has one source");
            assert!(
                op.parents.iter().all(|source| source.boot == generation),
                "evidence must reference the captured generation"
            );
        } else {
            assert_eq!(op.id.boot, generation, "physical generation must match");
        }
    }
}

/// Run a full Codex import with durable blob/cursor stores rooted at `chain`.
///
/// Each call constructs brand-new store instances over the same directories,
/// simulating a process restart between runs. Mirrors the import command's
/// ordering: import, then commit the staged cursors only after the chain
/// append has succeeded.
fn run_import(
    root: &Path,
    helper: &HelperCommand,
    options: &ImportOptions,
    chain: &Path,
) -> DurableRun {
    run_import_inner(root, helper, options, chain, true)
}

/// Like [`run_import`] but leaves the staged cursor writes uncommitted, as if
/// the process failed between import and the command's post-append commit.
fn run_import_without_commit(
    root: &Path,
    helper: &HelperCommand,
    options: &ImportOptions,
    chain: &Path,
) -> DurableRun {
    run_import_inner(root, helper, options, chain, false)
}

fn run_import_inner(
    root: &Path,
    helper: &HelperCommand,
    options: &ImportOptions,
    chain: &Path,
    commit_cursors: bool,
) -> DurableRun {
    let mut ops = MemoryOpSink::new();
    let mut blobs = FsBlobSink::new(chain.join("blobs")).unwrap();
    let mut cursors = FsCursorStore::new(chain.join("cursors")).unwrap();
    let request = CodexDiscoveryRequest {
        repositories: &(),
        workspace_path: PathBuf::from("/workspace"),
        raw_root: root.to_path_buf(),
    };
    let report = import_codex(
        &request,
        options,
        helper,
        &mut ops,
        &mut blobs,
        &mut cursors,
    )
    .unwrap();
    if commit_cursors {
        cursors.commit().unwrap();
    }
    DurableRun { report, ops, blobs }
}

fn source_key(root: &Path, source: &Path) -> String {
    canonical_source_key("codex", root, source).unwrap()
}

/// Run the fake helper through `/bin/sh` with no prefix args.
fn helper_in(dir: &tempfile::TempDir, awk: &str) -> HelperCommand {
    let script = write_fake_helper(dir.path(), "fake-helper.sh", awk);
    sh_helper(&script, &[])
}

/// Line bytes with trailing newline, as stored in the raw lane.
fn ln(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(b'\n');
    v
}

/// An event line whose payload is large enough to spill to blob storage.
fn big_event_line(token: char, size: usize) -> String {
    format!(
        "{{\"type\":\"event_msg\",\"payload\":{{\"blob\":\"{}\"}}}}",
        token.to_string().repeat(size)
    )
}

/// A `session_meta` line carrying the owning thread id.
fn session_meta_line(thread: &str, session_id: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-08-26T12:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"{session_id}\",\"id\":\"{thread}\",\"timestamp\":\"t\",\"cwd\":\"/tmp\"}}}}"
    )
}

/// Resolve an op's raw payload bytes through the durable blob store.
fn spilled_raw_bytes(op: &Op, blobs: &FsBlobSink) -> Vec<u8> {
    match &op.kind {
        OpKind::Import(ImportOp {
            raw_ref: Payload::Blob(bref),
            ..
        }) => match bref.id {
            ContentId::Hash256(hash) => blobs.get(&hash).unwrap().unwrap(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

#[test]
fn durable_import_roundtrips_spilled_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let raw_root = dir.path().join("sessions");
    std::fs::create_dir_all(&raw_root).unwrap();

    let meta = session_meta_line("thread-1", "PARENT-session");
    let big1 = big_event_line('x', 5000);
    let big2 = big_event_line('y', 6000);
    write_rollout(
        &raw_root,
        "rollout-1.jsonl",
        &[meta, big1.clone(), big2.clone()],
    );

    let chain = dir.path().join("chain");
    let run = run_import(
        &raw_root,
        &helper_in(&dir, &messages_awk("thread-1")),
        &ImportOptions::default(),
        &chain,
    );

    assert_eq!(run.report.files_discovered, 1);
    assert_eq!(run.report.files_processed, 1);
    assert_eq!(run.report.raw_ops, 3);
    assert_eq!(run.blobs.len().unwrap(), 2);

    // The two spilled raw lines are byte-exact through the durable blob store.
    assert_eq!(spilled_raw_bytes(&run.ops.ops[1], &run.blobs), ln(&big1));
    assert_eq!(spilled_raw_bytes(&run.ops.ops[2], &run.blobs), ln(&big2));
    // Blob filenames are the BLAKE3 hashes of their content.
    assert!(run.blobs.path_for(&hash_raw(&ln(&big1))).is_file());
}

#[test]
fn unchanged_reimport_after_restart_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let raw_root = dir.path().join("sessions");
    std::fs::create_dir_all(&raw_root).unwrap();

    let meta = session_meta_line("thread-1", "PARENT-session");
    let big1 = big_event_line('x', 5000);
    write_rollout(&raw_root, "rollout-1.jsonl", &[meta, big1.clone()]);

    let chain = dir.path().join("chain");
    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let options = ImportOptions::default();

    let first = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(first.report.files_processed, 1);
    assert_eq!(first.report.raw_ops, 2);
    assert!(!first.ops.ops.is_empty());

    // Second run with fresh store instances (process restart): the unchanged
    // rollout is skipped entirely, so nothing new is emitted or stored.
    let second = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(second.report.files_discovered, 1);
    assert_eq!(second.report.files_processed, 0);
    assert_eq!(second.report.raw_ops, 0);
    assert!(second.ops.ops.is_empty());
    assert_eq!(second.blobs.len().unwrap(), 1);
    assert_eq!(
        second.blobs.get(&hash_raw(&ln(&big1))).unwrap().unwrap(),
        ln(&big1)
    );
}

#[test]
fn incremental_append_after_restart_reads_only_new_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let raw_root = dir.path().join("sessions");
    std::fs::create_dir_all(&raw_root).unwrap();

    let meta = session_meta_line("thread-1", "PARENT-session");
    let big1 = big_event_line('x', 5000);
    write_rollout(&raw_root, "rollout-1.jsonl", &[meta.clone(), big1.clone()]);

    let chain = dir.path().join("chain");
    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let options = ImportOptions::default();

    let first = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(first.report.raw_ops, 2);
    assert_eq!(first.blobs.len().unwrap(), 1);

    // The source grows: rewrite with the same earlier lines plus one new big
    // line (what an in-progress session looks like on the next import).
    let big2 = big_event_line('y', 6000);
    write_rollout(&raw_root, "rollout-1.jsonl", &[meta, big1, big2.clone()]);

    let second = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(second.report.files_discovered, 1);
    assert_eq!(second.report.files_processed, 1);
    assert_eq!(second.report.raw_ops, 1);
    // The appended raw record, its normalized item, derivation, and source extent.
    assert_eq!(second.ops.ops.len(), 4);
    assert_eq!(second.report.evidence_ops, 2);
    // Only the appended line is emitted, byte-exact, from the durable store.
    assert_eq!(
        spilled_raw_bytes(&second.ops.ops[0], &second.blobs),
        ln(&big2)
    );
    // The new blob joined the existing one; nothing was re-stored or lost.
    assert_eq!(second.blobs.len().unwrap(), 2);
    assert!(second.blobs.path_for(&hash_raw(&ln(&big2))).is_file());
}

#[test]
fn uncommitted_staged_cursors_disappear_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let raw_root = dir.path().join("sessions");
    std::fs::create_dir_all(&raw_root).unwrap();

    let meta = session_meta_line("thread-1", "PARENT-session");
    let big1 = big_event_line('x', 5000);
    write_rollout(&raw_root, "rollout-1.jsonl", &[meta.clone(), big1.clone()]);

    let chain = dir.path().join("chain");
    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let options = ImportOptions::default();

    // Import emits ops and stages a cursor, but the commit after the chain
    // append never happens (simulated append failure / crash).
    let first = run_import_without_commit(&raw_root, &helper, &options, &chain);
    assert_eq!(first.report.files_processed, 1);
    assert!(!first.ops.ops.is_empty());

    // A fresh store over the same directory sees no cursor: the staged
    // mutation never reached disk.
    let rollout = raw_root.join("rollout-1.jsonl");
    let cursor_key = source_key(&raw_root, &rollout);
    let reopened = FsCursorStore::new(chain.join("cursors")).unwrap();
    assert!(reopened.get_cursor(&cursor_key).unwrap().is_none());

    // A restart therefore re-reads the same file: no operations were skipped.
    let second = run_import_without_commit(&raw_root, &helper, &options, &chain);
    assert_eq!(second.report.files_processed, 1);
    assert_eq!(second.report.raw_ops, 2);
}

#[test]
fn committed_cursors_persist_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let raw_root = dir.path().join("sessions");
    std::fs::create_dir_all(&raw_root).unwrap();

    let meta = session_meta_line("thread-1", "PARENT-session");
    let big1 = big_event_line('x', 5000);
    write_rollout(&raw_root, "rollout-1.jsonl", &[meta, big1]);

    let chain = dir.path().join("chain");
    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let options = ImportOptions::default();

    let first = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(first.report.raw_ops, 2);

    // The commit after a successful append made the cursor durable.
    let rollout = raw_root.join("rollout-1.jsonl");
    let cursor_key = source_key(&raw_root, &rollout);
    let reopened = FsCursorStore::new(chain.join("cursors")).unwrap();
    let cursor = reopened.get_cursor(&cursor_key).unwrap().unwrap();
    assert_eq!(cursor.ops_emitted, 2);
    assert_eq!(
        cursor.file_size,
        std::fs::metadata(raw_root.join("rollout-1.jsonl"))
            .unwrap()
            .len()
    );

    // A restart sees the committed cursor and skips the unchanged file.
    let second = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(second.report.files_processed, 0);
    assert!(second.ops.ops.is_empty());
}

#[test]
fn rewritten_rollout_reimport_is_deterministic_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let raw_root = dir.path().join("sessions");
    std::fs::create_dir_all(&raw_root).unwrap();
    let rollout = raw_root.join("rollout-1.jsonl");
    let cursor_key = source_key(&raw_root, &rollout);

    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let options = ImportOptions::default();
    let chain = dir.path().join("chain");

    // Generation 0 import, committed durably.
    write_rollout(
        &raw_root,
        "rollout-1.jsonl",
        &[
            session_meta_line("thread-1", "s"),
            big_event_line('x', 5000),
            big_event_line('y', 6000),
        ],
    );
    let first = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(first.report.raw_ops, 3);
    assert_generation(&first.ops.ops, 0);
    let original_size = std::fs::metadata(&rollout).unwrap().len();

    // Rewrite the source: truncated and replaced with different content.
    write_rollout(
        &raw_root,
        "rollout-1.jsonl",
        &[
            session_meta_line("thread-1", "s"),
            big_event_line('z', 7000),
        ],
    );

    // Crash between the rewrite import and the cursor commit: ops were staged
    // but never committed. Re-running must re-emit the exact same op ids.
    let crash1 = run_import_without_commit(&raw_root, &helper, &options, &chain);
    assert_eq!(crash1.report.raw_ops, 2);
    assert_generation(&crash1.ops.ops, 1);

    // The staged cursor and generation never reached disk: the durable cursor
    // is still the pre-rewrite generation-0 one (the file's committed size and
    // op count), so a restart re-reads the rewritten file in full.
    let reopened = FsCursorStore::new(chain.join("cursors")).unwrap();
    let stale = reopened.get_cursor(&cursor_key).unwrap().unwrap();
    assert_eq!(stale.ops_emitted, 3);
    assert_eq!(
        stale.file_size, original_size,
        "gen0 cursor still records the original pre-rewrite size"
    );
    assert_eq!(reopened.get_generation(&cursor_key).unwrap(), 0);

    let crash2 = run_import_without_commit(&raw_root, &helper, &options, &chain);
    assert_eq!(crash2.ops.ops, crash1.ops.ops, "deterministic replay ids");
    assert_eq!(crash2.report.raw_ops, 2);

    // A committed run persists cursor + generation and is then idempotent.
    let committed = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(committed.ops.ops, crash1.ops.ops);
    let reopened = FsCursorStore::new(chain.join("cursors")).unwrap();
    assert_eq!(reopened.get_generation(&cursor_key).unwrap(), 1);
    let cursor = reopened.get_cursor(&cursor_key).unwrap().unwrap();
    assert_eq!(cursor.ops_emitted, 2);
    assert_eq!(cursor.file_size, std::fs::metadata(&rollout).unwrap().len());

    let skip = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(skip.report.files_processed, 0);
    assert!(skip.ops.ops.is_empty());
}

#[test]
fn cursor_reset_reimports_source_with_current_generation_ids() {
    let dir = tempfile::tempdir().unwrap();
    let raw_root = dir.path().join("sessions");
    std::fs::create_dir_all(&raw_root).unwrap();
    let rollout = raw_root.join("rollout-1.jsonl");
    let cursor_key = source_key(&raw_root, &rollout);

    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let options = ImportOptions::default();
    let chain = dir.path().join("chain");

    write_rollout(
        &raw_root,
        "rollout-1.jsonl",
        &[session_meta_line("thread-1", "s"), big_event_line('A', 10)],
    );
    let first = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(first.report.raw_ops, 2);

    // Truncating rewrite starts and commits generation 1.
    write_rollout(
        &raw_root,
        "rollout-1.jsonl",
        &[session_meta_line("thread-1", "s")],
    );
    let rewritten = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(rewritten.report.raw_ops, 1);
    assert_generation(&rewritten.ops.ops, 1);

    // Reset procedure: delete the source's cursor file only. The generation
    // counter is retained, so the re-import replays at the current generation's
    // ids (an exact replay the OpSet canonicalizes) instead of falling back
    // into the original boot-0 id space.
    let store = FsCursorStore::new(chain.join("cursors")).unwrap();
    let cursor_file = store.cursor_path(&cursor_key);
    assert!(cursor_file.is_file());
    std::fs::remove_file(&cursor_file).unwrap();

    let reimported = run_import(&raw_root, &helper, &options, &chain);
    assert_eq!(reimported.report.files_processed, 1);
    assert_eq!(reimported.ops.ops, rewritten.ops.ops);
    let reopened = FsCursorStore::new(chain.join("cursors")).unwrap();
    assert_eq!(reopened.get_generation(&cursor_key).unwrap(), 1);
}

#[test]
fn commit_journals_the_paired_checkpoint_before_materializing_legacy_files() {
    let dir = tempfile::tempdir().unwrap();
    let cursor_dir = dir.path().join("chain/cursors");
    let key = "/workspace/rollout-1.jsonl";

    let old_cursor = CursorValue {
        accepted_generation: None,
        file_size: 42,
        byte_offset: 40,
        ops_emitted: 7,
        content_hash: [7u8; 32],
        content_hash_version: 1,
        source_node: Some(editchain_core::NodeId(7)),
        normalization_version: 0,
        materialization: None,
        session_title_hash: None,
    };
    let new_cursor = CursorValue {
        accepted_generation: None,
        file_size: 9,
        byte_offset: 8,
        ops_emitted: 2,
        content_hash: [9u8; 32],
        content_hash_version: 1,
        source_node: Some(editchain_core::NodeId(7)),
        normalization_version: 0,
        materialization: None,
        session_title_hash: None,
    };

    // Durable baseline: a generation-0 cursor covering the pre-rewrite read.
    {
        let mut store = FsCursorStore::new(&cursor_dir).unwrap();
        store.set_generation(key, 0).unwrap();
        store.set_cursor(key, &old_cursor).unwrap();
        store.commit().unwrap();
    }

    // Stage a rewrite (generation bump + new cursor) and fail the commit in
    // the between-writes window: blocking the cursor's temp-file path makes
    // the cursor write fail AFTER generations.json has been durably written,
    // leaving the old cursor file on disk untouched.
    {
        let mut store = FsCursorStore::new(&cursor_dir).unwrap();
        store.set_generation(key, 1).unwrap();
        store.set_cursor(key, &new_cursor).unwrap();
        let blocked = store
            .cursor_path(key)
            .with_extension(format!("tmp.{}", std::process::id()));
        std::fs::create_dir_all(&blocked).unwrap();
        let err = store.commit().expect_err("cursor write must fail");
        assert!(
            err.to_string().contains("writing"),
            "the failing step must be the cursor write: {err}"
        );
        std::fs::remove_dir(&blocked).unwrap();

        // The legacy cursor file still contains the old value. The durable
        // journal owns the paired checkpoint; opening finishes its recovery.
        let stored: CursorValue =
            serde_json::from_slice(&std::fs::read(store.cursor_path(key)).unwrap()).unwrap();
        assert_eq!(stored, old_cursor);
        let reopened = FsCursorStore::new(&cursor_dir).unwrap();
        assert_eq!(reopened.get_generation(key).unwrap(), 1);
        assert_eq!(reopened.get_cursor(key).unwrap().unwrap(), new_cursor);
        assert!(!cursor_dir.join("checkpoint.pending.json").exists());

        // In-memory state stays coherent after the partial commit: the staged
        // generation is durable and readable, the failed cursor is still
        // staged (read-your-writes) and pending for a retry.
        assert_eq!(store.get_generation(key).unwrap(), 1);
        assert_eq!(store.get_cursor(key).unwrap().unwrap(), new_cursor);
        assert!(store.has_pending());

        // A retry (crash recovery) completes the cursor write.
        store.commit().unwrap();
        assert!(!store.has_pending());
    }

    let reopened = FsCursorStore::new(&cursor_dir).unwrap();
    assert_eq!(reopened.get_generation(key).unwrap(), 1);
    assert_eq!(reopened.get_cursor(key).unwrap().unwrap(), new_cursor);
}

#[test]
fn batch_discards_failed_capture_and_checkpoints_only_after_durable_acceptance() {
    use editchain_import::batch::{DurableAdmission, DurableOpSink, ImportBatch};
    use editchain_import::{ImportError, MemoryCursorStore};
    struct Writer {
        fail: bool,
        calls: usize,
    }
    impl DurableOpSink for Writer {
        fn append_durable(&mut self, ops: &[Op]) -> Result<DurableAdmission, ImportError> {
            self.calls = self.calls.saturating_add(1);
            if self.fail {
                return Err(ImportError::OpSink("append failed".into()));
            }
            Ok(DurableAdmission {
                written: ops.len(),
                ..DurableAdmission::default()
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("empty.jsonl");
    std::fs::write(&source, b"").unwrap();
    let (_, _, mut cursor) =
        editchain_import::source_read::read_session_file(&source, None).unwrap();
    cursor.accepted_generation = Some(3);
    let mut base = MemoryCursorStore::new();
    let failed = ImportBatch::capture(&base, |_ops, pending| {
        pending.set_generation("source", 3)?;
        pending.set_cursor("source", &cursor)?;
        Err(ImportError::OpSink("later source failed".into()))
    });
    assert!(failed.is_err());
    assert_eq!(base.get_generation("source").unwrap(), 0);
    assert!(base.get_cursor("source").unwrap().is_none());

    let capture = |base: &dyn CursorStore| {
        ImportBatch::capture(base, |_ops, pending| {
            pending.set_generation("source", 3)?;
            pending.set_cursor("source", &cursor)?;
            assert_eq!(pending.get_generation("source")?, 3);
            assert_eq!(pending.get_cursor("source")?, Some(cursor.clone()));
            Ok(ImportReport::default())
        })
        .unwrap()
    };
    let mut writer = Writer {
        fail: true,
        calls: 0,
    };
    assert!(capture(&base).persist(&mut writer, &mut base).is_err());
    assert_eq!(base.get_generation("source").unwrap(), 0);
    assert!(base.get_cursor("source").unwrap().is_none());
    writer.fail = false;
    let outcome = capture(&base).persist(&mut writer, &mut base).unwrap();
    assert_eq!(
        writer.calls, 2,
        "even empty sources require the persistence handoff"
    );
    assert_eq!(outcome.admission.written, 0);
    assert_eq!(base.get_cursor("source").unwrap(), Some(cursor));
    assert_eq!(base.get_generation("source").unwrap(), 3);
}

#[test]
fn batch_limit_failure_discards_completed_files_and_retry_preserves_bytes() {
    use editchain_import::batch::ImportBatch;
    use editchain_import::sink::BatchLimits;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    std::fs::create_dir(&root).unwrap();
    for name in ["rollout-1.jsonl", "rollout-2.jsonl"] {
        write_rollout(
            &root,
            name,
            &[
                session_meta_line("thread", "session"),
                big_event_line('a', 1),
            ],
        );
    }
    let first_key = source_key(&root, &root.join("rollout-1.jsonl"));
    let second_key = source_key(&root, &root.join("rollout-2.jsonl"));
    let base = FsCursorStore::new(dir.path().join("cursors")).unwrap();
    let mut blobs = FsBlobSink::new(dir.path().join("blobs")).unwrap();
    let helper = helper_in(&dir, &messages_awk("thread"));
    let request = CodexDiscoveryRequest {
        repositories: &(),
        workspace_path: "/workspace".into(),
        raw_root: root,
    };
    let mut capture = |limits| {
        ImportBatch::capture_bounded(&base, limits, |ops, pending| {
            let result = import_codex(
                &request,
                &ImportOptions::default(),
                &helper,
                ops,
                &mut blobs,
                pending,
            );
            assert!(
                pending.get_cursor(&first_key).unwrap().is_some(),
                "the first file completed inside the private capture"
            );
            result
        })
    };
    let expected = capture(BatchLimits::default()).unwrap();
    assert_eq!(expected.operations().len(), 12);
    let bytes = expected
        .operations()
        .iter()
        .map(|op| encode_op(op).unwrap())
        .collect::<Vec<_>>();
    let encoded_bytes = u64::try_from(bytes.iter().map(Vec::len).sum::<usize>()).unwrap();
    for limits in [
        BatchLimits {
            operations: 7,
            ..BatchLimits::default()
        },
        BatchLimits {
            encoded_bytes: encoded_bytes.saturating_sub(1),
            ..BatchLimits::default()
        },
    ] {
        assert!(capture(limits).is_err());
        for key in [&first_key, &second_key] {
            assert!(base.get_cursor(key).unwrap().is_none());
            assert!(base.get_reservation(key).unwrap().is_none());
            assert_eq!(base.get_generation(key).unwrap(), 0);
        }
        assert!(!base.has_pending());
    }
    let retry = capture(BatchLimits {
        operations: 12,
        encoded_bytes,
    })
    .unwrap();
    assert_eq!(
        retry
            .operations()
            .iter()
            .map(|op| encode_op(op).unwrap())
            .collect::<Vec<_>>(),
        bytes
    );
    let extra = retry.operations()[0].clone();
    let retry = retry.extend_operations([extra.clone()]).unwrap();
    assert_eq!(retry.operations().len(), 12);
    assert_eq!(retry.report().duplicates, 1);
    let mut extra = extra;
    extra.id.seq = u64::MAX;
    assert!(retry.extend_operations([extra]).is_err());
    assert!(!base.has_pending());
    assert!(base.get_cursor(&first_key).unwrap().is_none());
}

#[test]
fn both_checkpoint_crash_windows_preserve_generation_identity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    let chain = dir.path().join("chain");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("rollout-1.jsonl");
    write_rollout(&root, "rollout-1.jsonl", &[big_event_line('A', 1)]);
    let helper = helper_in(&dir, &messages_awk("t"));
    let first = run_import(&root, &helper, &ImportOptions::default(), &chain);
    assert_eq!(first.report.raw_ops, 1);
    let key = canonical_source_key("codex", &root, &path).unwrap();
    let cursor_dir = chain.join("cursors");
    let mut cursors = FsCursorStore::new(&cursor_dir).unwrap();
    let accepted = cursors.get_cursor(&key).unwrap().unwrap();
    assert_eq!(accepted.accepted_generation, Some(0));
    write_rollout(&root, "rollout-1.jsonl", &[big_event_line('B', 1)]);
    let request = CodexDiscoveryRequest {
        repositories: &(),
        workspace_path: "/workspace".into(),
        raw_root: root.clone(),
    };
    let mut blobs = FsBlobSink::new(chain.join("blobs")).unwrap();
    let mut capture = |cursors: &mut FsCursorStore| {
        let mut ops = MemoryOpSink::new();
        let report = import_codex(
            &request,
            &ImportOptions::default(),
            &helper,
            &mut ops,
            &mut blobs,
            cursors,
        )
        .unwrap();
        assert_eq!(report.raw_ops, 1);
        ops.ops
    };
    let proposed = capture(&mut cursors);
    // Failure before the journal is durable: the same source must replay the
    // exact same envelopes, including the proposed generation.
    let blocked_journal = cursor_dir
        .join("checkpoint.pending.json")
        .with_extension(format!("tmp.{}", std::process::id()));
    std::fs::create_dir(&blocked_journal).unwrap();
    assert!(cursors.commit().is_err());
    std::fs::remove_dir(&blocked_journal).unwrap();
    drop(cursors);
    let mut retry = FsCursorStore::new(&cursor_dir).unwrap();
    assert_eq!(retry.get_generation(&key).unwrap(), 0);
    assert_eq!(retry.get_cursor(&key).unwrap(), Some(accepted.clone()));
    assert_eq!(capture(&mut retry), proposed);

    // Failure after journal durability but before the legacy cursor write.
    let blocked_cursor = retry
        .cursor_path(&key)
        .with_extension(format!("tmp.{}", std::process::id()));
    std::fs::create_dir(&blocked_cursor).unwrap();
    assert!(retry.commit().is_err());
    std::fs::remove_dir(&blocked_cursor).unwrap();
    let stored: CursorValue =
        serde_json::from_slice(&std::fs::read(retry.cursor_path(&key)).unwrap()).unwrap();
    assert_eq!(stored, accepted);
    drop(retry);
    // A third source version arrives before restart. Recovery must retain B's
    // completed generation before considering C, avoiding reuse of B's IDs.
    write_rollout(&root, "rollout-1.jsonl", &[big_event_line('C', 1)]);
    let mut recovered = FsCursorStore::new(&cursor_dir).unwrap();
    assert_eq!(recovered.get_generation(&key).unwrap(), 1);
    let restored = recovered.get_cursor(&key).unwrap().unwrap();
    assert_eq!(restored.accepted_generation, Some(1));
    assert_eq!(
        restored.content_hash,
        hash_raw(&ln(&big_event_line('B', 1)))
    );
    assert!(!cursor_dir.join("checkpoint.pending.json").exists());
    let third = capture(&mut recovered);
    assert!(third
        .iter()
        .filter(|op| op.id.seq == 1 << 16)
        .all(|op| op.id.boot == 2));
    assert!(third
        .iter()
        .all(|new| proposed.iter().all(|old| old.id != new.id)));
    recovered.commit().unwrap();
    // Explicit cursor reset still reuses the last reserved generation.
    std::fs::remove_file(recovered.cursor_path(&key)).unwrap();
    let reset = run_import(&root, &helper, &ImportOptions::default(), &chain);
    assert_eq!(reset.ops.ops, third);
}

#[test]
fn unsupported_checkpoint_journal_is_retained_and_blocks_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("checkpoint.pending.json");
    let bytes = br#"{"version":2,"cursors":{},"generations":{}}"#;
    std::fs::write(&journal, bytes).unwrap();
    let error = FsCursorStore::new(dir.path()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read(journal).unwrap(), bytes);
}
