//! End-to-end verification of exact subagent spawn linking against a real
//! Claude Code session.
//!
//! This test copies a real session directory (main session + its `subagents/`)
//! into a temp dir, runs the full import pipeline, and asserts that subagent
//! `SpawnedBy` notes are derived from the sidecar's exact `toolUseId`; free-form
//! completion text is deliberately not interpreted as provenance.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Test fixture staging uses fixed paths; unwraps are safe in tests"
)]

use std::path::PathBuf;

use blake3 as _;
use editchain_codec as _;
use editchain_core as _;
use editchain_store as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;
use time as _;
use tokio as _;

use editchain_core::{
    op::{NoteRelationship, OpKind},
    Op,
};
use editchain_import::import::import_claude_code;
use editchain_import::model::{DiscoveryRequest, ImportOptions};
use editchain_import::sink::{MemoryBlobSink, MemoryCursorStore, MemoryOpSink};

/// Path to the real session directory used as the fixture.
///
/// `8a7a3ac4-...` (rtx-pro-6000-bench) reports subagent completion through the
/// `TaskStop`/late-check "not running (status: completed)" format, which is the
/// path this fix targets. (The `016bbaad-...` session uses the `TaskOutput`
/// polling format, which the original code already handled.)
const SESSION_DIR: &str =
    "/mnt/hot/ambientlight/.claude/projects/-mnt-hot-ambientlight-repos-rtx-pro-6000-bench/8a7a3ac4-6369-48d6-9684-f8475b7d551d";

/// Copy a real session dir (main `.jsonl` + `subagents/`) into a temp dir so
/// discovery finds it in the expected layout.
fn stage_session(tmp: &std::path::Path) -> PathBuf {
    let src = std::path::Path::new(SESSION_DIR);
    let session_id = src.file_name().unwrap().to_string_lossy().to_string();

    // Main session file lives at <tmp>/<session-id>.jsonl.
    let main_src = src.with_extension("jsonl");
    let main_dst = tmp.join(format!("{session_id}.jsonl"));
    let _unused = std::fs::copy(&main_src, &main_dst).expect("copy main session");

    // Subagents live at <tmp>/<session-id>/subagents/agent-*.jsonl (+ meta).
    let sub_src = src.join("subagents");
    if sub_src.exists() {
        let sub_dst = tmp.join(&session_id).join("subagents");
        std::fs::create_dir_all(&sub_dst).expect("create subagents dir");
        for entry in std::fs::read_dir(&sub_src).expect("read subagents") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().to_string();
            // Only copy transcript + meta files; skip subdirectories.
            if !name.ends_with(".jsonl") && !name.ends_with(".meta.json") {
                continue;
            }
            let _unused = std::fs::copy(entry.path(), sub_dst.join(entry.file_name()))
                .expect("copy subagent file");
        }
    }

    tmp.to_path_buf()
}

/// Count relationship notes of a given kind.
fn count_notes(ops: &[Op], rel: NoteRelationship) -> usize {
    ops.iter()
        .filter(|op| matches!(&op.kind, OpKind::Note(n) if n.relationship == rel))
        .count()
}

#[test]
fn real_session_produces_exact_spawn_edges() {
    if !std::path::Path::new(SESSION_DIR).exists() {
        // Real session not present on this machine — skip (e.g. CI).
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let sessions_dir = stage_session(tmp.path());

    let request = DiscoveryRequest {
        workspace_path: PathBuf::from("/mnt/hot/ambientlight/repos/editchain"),
        sessions_dir,
        chain_dir: PathBuf::from("/tmp/unused-chain"),
    };
    let options = ImportOptions::default();
    let mut ops_sink = MemoryOpSink::new();
    let mut blobs_sink = MemoryBlobSink::new();
    let mut cursors = MemoryCursorStore::new();

    let report = import_claude_code(
        &request,
        &options,
        &mut ops_sink,
        &mut blobs_sink,
        &mut cursors,
    )
    .expect("import should succeed");

    // Sanity: we actually imported something.
    assert!(report.files_discovered >= 2, "expected main + subagents");
    assert!(!ops_sink.ops.is_empty(), "expected normalized ops");

    // This fixture has three sidecars with exact toolUseIds. Every one emits a
    // SpawnedBy fact; the old substring-derived reconnect is intentionally gone.
    let branches = count_notes(&ops_sink.ops, NoteRelationship::SpawnedBy);
    let reconnects = count_notes(&ops_sink.ops, NoteRelationship::ReconnectsTo);
    assert_eq!(
        branches, 3,
        "expected 3 exact SpawnedBy notes; got {branches}"
    );
    assert_eq!(
        reconnects, 0,
        "free-form completion text must not create reconnect provenance"
    );

    // SPEC §5 gate: the notes must actually drive projection. Build a
    // `HistoryProjection` from the imported ops and assert the structural notes
    // (a) are read as virtual edges and (b) do not surface as standalone rows.
    let projection = editchain_project::HistoryProjection::from_ops(ops_sink.ops.clone());
    let nodes = projection.nodes();
    let note_rows = nodes
        .iter()
        .filter(|n| matches!(n.kind().as_str(), "note"))
        .count();
    assert_eq!(
        note_rows, 0,
        "structural relationship notes must fold out of rendered rows; got {note_rows} Note rows"
    );
    // At least one structural note is indexed and reachable as a virtual parent.
    let any_virtual = projection
        .relationship_notes()
        .values()
        .any(|notes| !notes.is_empty());
    assert!(
        any_virtual,
        "expected relationship notes to be indexed for virtual edges"
    );
}
