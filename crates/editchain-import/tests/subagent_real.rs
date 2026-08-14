//! End-to-end verification of subagent branch/reconnect linking against a real
//! Claude Code session.
//!
//! This test copies a real session directory (main session + its `subagents/`)
//! into a temp dir, runs the full import pipeline, and asserts that reconnect
//! edges are created — i.e. that some ops end up with two parents (a branch
//! parent plus a reconnect parent), which is the signature of a subagent that
//! branched off its parent's `Agent` call and reconnected via a completion
//! result.
//!
//! The fixture session (`8a7a3ac4-...`, rtx-pro-6000-bench) reports subagent
//! completion through the `TaskStop`/late-check "not running (status:
//! completed)" format, which is the path this fix targets. It has 3 subagents;
//! with the fix, 4 ops end up with two parents.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Test fixture staging uses fixed paths; unwraps are safe in tests"
)]

use std::path::PathBuf;

use blake3 as _;
use editchain_core as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

use editchain_core::{Op, ParentSet};
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

/// Count ops that have two parents — the signature of a linked subagent
/// (branch parent + reconnect parent).
fn count_two_parent_ops(ops: &[Op]) -> usize {
    ops.iter()
        .filter(|op| matches!(op.parents, ParentSet::Two(..)))
        .count()
}

#[test]
fn real_session_produces_reconnect_edges() {
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

    // The linking post-pass runs inside import_claude_code. Verify reconnect
    // edges exist. This session has 3 subagents; with the fix, 4 ops end up
    // with two parents (branch + reconnect). Without the "not running"
    // completion fix, only 3 do (the TaskOutput-format subagents reconnect but
    // the TaskStop-format one does not). Asserting the exact count catches a
    // regression in either completion path.
    let two_parents = count_two_parent_ops(&ops_sink.ops);
    assert_eq!(
        two_parents, 4,
        "expected 4 ops with two parents (branch + reconnect); got {two_parents}"
    );
}
