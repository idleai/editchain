//! End-to-end Codex import tests using a fake helper bridge (Unix).
#![cfg(unix)]
#![expect(
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::panic,
    clippy::unwrap_used,
    clippy::wildcard_enum_match_arm,
    reason = "test helpers index/pass-by-value/panic/unwrap on known-length fixture vectors"
)]

mod common;

use blake3 as _;
use editchain_core as _;
use editchain_project as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use tempfile as _;

use editchain_core::clock::Clock;
use editchain_core::op::{CommandStage, OpKind, ToolStage};
use editchain_core::payload::Payload;
use editchain_core::scope::ScopeRef;
use editchain_core::tags::Tags;

use editchain_core::parents::ParentSet;
use editchain_import::claude_code::normalize::parse_source_time;
use editchain_import::codex::{import_codex, CodexDiscoveryRequest, HelperCommand};
use editchain_import::cursor::canonical_source_key;
use editchain_import::error::ImportError;
use editchain_import::ids::{
    derive_actor_id, derive_keyed_source_stream, derive_path_id, derive_session_id,
    derive_source_stream, derive_turn_id, SourcePosition, SourceStream,
};
use editchain_import::model::ImportOptions;
use editchain_import::sink::{
    ContentAddressedBlobSink, CursorStore, MemoryCursorStore, MemoryOpSink,
};

use common::*;

fn helper_in(dir: &tempfile::TempDir, awk: &str) -> HelperCommand {
    let script = write_fake_helper(dir.path(), "fake-helper.sh", awk);
    sh_helper(&script, &[])
}

fn helper_with_args(dir: &tempfile::TempDir, awk: &str) -> HelperCommand {
    let script = write_fake_helper(dir.path(), "fake-helper.sh", awk);
    sh_helper(
        &script,
        &["--format".to_string(), "editchain-v1".to_string()],
    )
}

fn fixed_helper(dir: &tempfile::TempDir, projection: &[u8]) -> HelperCommand {
    let script = write_fixed_helper(dir.path(), "fake-helper.sh", projection);
    sh_helper(&script, &[])
}

fn source_key(root: &Path, path: &Path) -> String {
    canonical_source_key("codex", root, path).unwrap()
}

fn source_stream(root: &Path, path: &Path, boot: u32) -> SourceStream {
    derive_keyed_source_stream(&source_key(root, path), boot)
}

/// Line bytes with trailing newline, as stored in the raw lane.
fn ln(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(b'\n');
    v
}

fn session_meta_line(thread: &str, session_id: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-08-26T12:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"{session_id}\",\"id\":\"{thread}\",\"timestamp\":\"t\",\"cwd\":\"/tmp\"}}}}"
    )
}

fn event_line(token: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-08-26T12:00:01.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"token\":\"{token}\",\"session_id\":\"parent-session\"}}}}"
    )
}

fn lifecycle_event_line(event_type: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-08-26T12:00:02.000Z",
        "type": "event_msg",
        "payload": {"type": event_type},
    })
    .to_string()
}

fn token_usage_line() -> String {
    let usage = serde_json::json!({
        "input_tokens": 10,
        "cached_input_tokens": 2,
        "cache_write_input_tokens": 0,
        "output_tokens": 3,
        "reasoning_output_tokens": 1,
        "total_tokens": 13,
    });
    serde_json::json!({
        "timestamp": "2026-08-26T12:00:02.000Z",
        "type": "token_usage_record",
        "payload": {
            "thread_id": "0195cda5-433d-7f9a-9d7b-a9f15b60c2e2",
            "turn_id": "turn-1",
            "session_id": "0195cda5-433d-7f9a-9d7b-a9f15b60c2e2",
            "root_turn_id": "turn-1",
            "response_id": "response-1",
            "usage": usage,
            "turn_token_usage": usage,
            "thread_token_usage": usage,
        },
    })
    .to_string()
}

/// Serialize one `editchain-v1` line record built from typed values.
fn line_record(
    ordinal: u64,
    changed_items: Vec<serde_json::Value>,
    session_meta: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": "editchain-v1",
        "recordType": "line",
        "sourcePath": "x",
        "sourceOrdinal": ordinal,
        "decode": {"status": "ok"},
        "projection": {
            "changedItems": changed_items,
            "changedTurns": [],
            "removedTurnIds": [],
            "sessionMeta": session_meta,
        },
    })
}

/// Newline-join projection records into a helper stdout buffer.
fn projection_bytes(records: &[serde_json::Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        out.extend_from_slice(&serde_json::to_vec(record).unwrap());
        out.push(b'\n');
    }
    out
}

#[test]
fn session_start_git_metadata_emits_one_exact_based_on_link() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let git_marker = workspace.join(".git");
    std::fs::create_dir_all(&git_marker).unwrap();
    let rollouts = dir.path().join("rollouts");
    std::fs::create_dir_all(&rollouts).unwrap();
    write_rollout(
        &rollouts,
        "rollout-thread-1.jsonl",
        &[session_meta_line("thread-1", "s")],
    );

    let commit_hash = "0123456789abcdef0123456789abcdef01234567";
    let projection = projection_bytes(&[line_record(
        1,
        Vec::new(),
        Some(serde_json::json!({
            "sessionId": "s",
            "threadId": "thread-1",
            "cwd": workspace.to_string_lossy(),
            "git": {
                "commitHash": commit_hash,
                "branch": "r4",
                "repositoryUrl": "https://github.com/idleai/editchain.git"
            }
        })),
    )]);
    let helper = fixed_helper(&dir, &projection);
    let imported = import_workspace(&rollouts, &workspace, &helper);

    let links: Vec<_> = imported
        .ops
        .ops
        .iter()
        .filter_map(|op| match &op.kind {
            OpKind::GitLink(link) => Some((op, link)),
            _ => None,
        })
        .collect();
    assert_eq!(links.len(), 1, "one session gets one Git base relation");
    let (link_op, link) = links[0];
    let stream = source_stream(&rollouts, &rollouts.join("rollout-thread-1.jsonl"), 0);
    let session_start = stream.op_from_position(SourcePosition::raw(1)).unwrap();
    assert_eq!(link.source, session_start);
    assert_eq!(link_op.parents, ParentSet::One(session_start));
    assert_eq!(
        link.target_oid,
        editchain_core::GitOid::from_hex(commit_hash).unwrap()
    );
    assert!(matches!(link.kind, editchain_core::GitLinkKind::BasedOn));

    let canonical_marker = git_marker.canonicalize().unwrap();
    let digest = Sha256::digest(canonical_marker.to_string_lossy().as_bytes());
    let mut repository_bytes = [0u8; 8];
    repository_bytes.copy_from_slice(&digest[..8]);
    assert_eq!(
        link.target_repo,
        editchain_core::RepositoryId(u64::from_le_bytes(repository_bytes))
    );
    assert_eq!(imported.report.raw_ops, 1);
    assert_eq!(imported.report.normalized_ops, 1);
}

#[test]
fn legacy_cursor_backfills_session_git_link_once_without_replaying_rows() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    let rollouts = dir.path().join("rollouts");
    std::fs::create_dir_all(&rollouts).unwrap();
    let rollout = rollouts.join("rollout-thread-1.jsonl");
    write_rollout(
        &rollouts,
        "rollout-thread-1.jsonl",
        &[session_meta_line("thread-1", "s")],
    );
    let projection = projection_bytes(&[line_record(
        1,
        Vec::new(),
        Some(serde_json::json!({
            "sessionId": "s",
            "threadId": "thread-1",
            "cwd": workspace.to_string_lossy(),
            "git": {
                "commitHash": "0123456789abcdef0123456789abcdef01234567"
            }
        })),
    )]);
    let helper = fixed_helper(&dir, &projection);
    let options = ImportOptions::default();
    let mut cursors = MemoryCursorStore::new();

    let initial =
        import_workspace_into(&rollouts, &workspace, &helper, &options, &mut cursors).unwrap();
    assert_eq!(initial.report.raw_ops, 1);
    let cursor_key = source_key(&rollouts, &rollout);
    let mut legacy = cursors.get_cursor(&cursor_key).unwrap().unwrap();
    legacy.normalization_version = 0;
    cursors.set_cursor(&cursor_key, &legacy).unwrap();

    let backfill =
        import_workspace_into(&rollouts, &workspace, &helper, &options, &mut cursors).unwrap();
    assert_eq!(backfill.report.files_processed, 1);
    assert_eq!(backfill.report.raw_ops, 0);
    assert_eq!(backfill.report.normalized_ops, 1);
    assert_eq!(
        backfill
            .ops
            .ops
            .iter()
            .filter(|op| matches!(op.kind, OpKind::GitLink(_)))
            .count(),
        1
    );
    assert_eq!(
        cursors
            .get_cursor(&cursor_key)
            .unwrap()
            .unwrap()
            .normalization_version,
        5
    );

    let current =
        import_workspace_into(&rollouts, &workspace, &helper, &options, &mut cursors).unwrap();
    assert_eq!(current.report.files_processed, 0);
    assert!(current.ops.ops.is_empty());
}

#[test]
fn version_one_cursor_upgrades_topology_without_replaying_git_link() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    let rollouts = dir.path().join("rollouts");
    std::fs::create_dir_all(&rollouts).unwrap();
    let rollout = rollouts.join("rollout-thread-1.jsonl");
    write_rollout(
        &rollouts,
        "rollout-thread-1.jsonl",
        &[session_meta_line("thread-1", "s")],
    );
    let projection = projection_bytes(&[line_record(
        1,
        Vec::new(),
        Some(serde_json::json!({
            "sessionId": "s",
            "threadId": "thread-1",
            "cwd": workspace.to_string_lossy(),
            "git": {
                "commitHash": "0123456789abcdef0123456789abcdef01234567"
            }
        })),
    )]);
    let helper = fixed_helper(&dir, &projection);
    let options = ImportOptions::default();
    let mut cursors = MemoryCursorStore::new();

    let initial =
        import_workspace_into(&rollouts, &workspace, &helper, &options, &mut cursors).unwrap();
    assert_eq!(initial.report.raw_ops, 1);
    assert_eq!(
        initial
            .ops
            .ops
            .iter()
            .filter(|op| matches!(op.kind, OpKind::GitLink(_)))
            .count(),
        1
    );

    let cursor_key = source_key(&rollouts, &rollout);
    let mut version_one = cursors.get_cursor(&cursor_key).unwrap().unwrap();
    version_one.normalization_version = 1;
    cursors.set_cursor(&cursor_key, &version_one).unwrap();

    let upgrade =
        import_workspace_into(&rollouts, &workspace, &helper, &options, &mut cursors).unwrap();
    assert_eq!(upgrade.report.files_processed, 1);
    assert_eq!(upgrade.report.raw_ops, 0);
    assert!(
        upgrade
            .ops
            .ops
            .iter()
            .all(|op| !matches!(op.kind, OpKind::GitLink(_))),
        "the current metadata checkpoint must not replay the v1 Git fact"
    );
    assert_eq!(
        cursors
            .get_cursor(&cursor_key)
            .unwrap()
            .unwrap()
            .normalization_version,
        5
    );
}

#[test]
fn session_index_title_is_captured_and_title_only_changes_reproject() {
    let dir = tempfile::tempdir().unwrap();
    let rollout = dir.path().join("rollout-1.jsonl");
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[session_meta_line("thread-1", "s")],
    );
    let index = dir.path().join("session_index.jsonl");
    std::fs::write(
        &index,
        "{\"id\":\"thread-1\",\"thread_name\":\"First title\",\"updated_at\":\"2026-08-26T12:05:00Z\"}\n",
    )
    .unwrap();
    let helper = fixed_helper(&dir, &session_projection("thread-1", None));
    let options = ImportOptions::default();
    let mut cursors = MemoryCursorStore::new();

    let first = import_with_options_into(dir.path(), &helper, &options, &mut cursors);
    assert_eq!(first.report.raw_ops, 1);
    assert_eq!(first.report.normalized_ops, 1);
    let title_op = first
        .ops
        .ops
        .iter()
        .find(|op| {
            matches!(&op.kind, OpKind::Import(import)
                if matches!(&import.raw_ref, Payload::Inline(raw)
                    if serde_json::from_slice::<serde_json::Value>(raw).ok()
                        .is_some_and(|value| value["type"] == "session_title")))
        })
        .expect("portable session title op");
    assert_eq!(
        title_op.scope,
        ScopeRef::Session(derive_session_id("thread-1"))
    );
    assert_eq!(
        title_op.parents,
        ParentSet::One(
            source_stream(dir.path(), &rollout, 0)
                .op_from_position(SourcePosition::raw(1))
                .unwrap()
        )
    );
    let OpKind::Import(title_import) = &title_op.kind else {
        panic!("title is an import metadata op");
    };
    let Payload::Inline(raw) = &title_import.raw_ref else {
        panic!("small title stays inline");
    };
    let title: serde_json::Value = serde_json::from_slice(raw).unwrap();
    assert_eq!(title["title"], "First title");

    let unchanged = import_with_options_into(dir.path(), &helper, &options, &mut cursors);
    assert_eq!(unchanged.report.files_processed, 0);
    assert!(unchanged.ops.ops.is_empty());

    std::fs::write(
        &index,
        concat!(
            "{\"id\":\"thread-1\",\"thread_name\":\"First title\",\"updated_at\":\"2026-08-26T12:05:00Z\"}\n",
            "{\"id\":\"thread-1\",\"thread_name\":\"Renamed\",\"updated_at\":\"2026-08-26T12:06:00Z\"}\n",
        ),
    )
    .unwrap();
    let renamed = import_with_options_into(dir.path(), &helper, &options, &mut cursors);
    assert_eq!(renamed.report.files_processed, 1);
    assert_eq!(renamed.report.raw_ops, 0);
    assert_eq!(renamed.report.normalized_ops, 1);
    let renamed_title = renamed.ops.ops.iter().find_map(|op| {
        let OpKind::Import(import) = &op.kind else {
            return None;
        };
        let Payload::Inline(raw) = &import.raw_ref else {
            return None;
        };
        serde_json::from_slice::<serde_json::Value>(raw)
            .ok()
            .filter(|value| value["type"] == "session_title")
    });
    assert_eq!(
        renamed_title.as_ref().map(|value| &value["title"]),
        Some(&serde_json::json!("Renamed"))
    );
}

#[test]
fn legacy_cursor_migrates_once_and_survives_sessions_root_relocation() {
    let dir = tempfile::tempdir().unwrap();
    let relative = Path::new("2026/09/05/rollout-thread-1.jsonl");
    let archive_root = dir.path().join("archive");
    let live_root = dir.path().join("live");
    let archive_path = archive_root.join(relative);
    let live_path = live_root.join(relative);
    std::fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(live_path.parent().unwrap()).unwrap();
    let source = format!(
        "{}\n{}\n",
        session_meta_line("thread-1", "s"),
        event_line("A")
    );
    std::fs::write(&archive_path, &source).unwrap();
    std::fs::write(&live_path, &source).unwrap();

    let (_lines, _bytes, mut legacy_cursor) =
        editchain_import::claude_code::reader::read_session_file(&archive_path, None).unwrap();
    legacy_cursor.normalization_version = 1;
    legacy_cursor.source_node = None;
    let legacy_key = archive_path.to_string_lossy().to_string();
    let legacy_stream = derive_source_stream("/workspace", &legacy_key, 0);
    let mut cursors = MemoryCursorStore::new();
    cursors.set_cursor(&legacy_key, &legacy_cursor).unwrap();

    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let archive_upgrade = import_with_options_into(
        &archive_root,
        &helper,
        &ImportOptions::default(),
        &mut cursors,
    );
    assert_eq!(archive_upgrade.report.raw_ops, 0);

    let archive_key = source_key(&archive_root, &archive_path);
    let live_key = source_key(&live_root, &live_path);
    assert_eq!(archive_key, live_key);
    let migrated = cursors.get_cursor(&archive_key).unwrap().unwrap();
    assert_eq!(migrated.source_node, Some(legacy_stream.node));
    assert_eq!(migrated.content_hash_version, 1);

    let relocated =
        import_with_options_into(&live_root, &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(relocated.report.files_processed, 0);
    assert!(relocated.ops.ops.is_empty());

    let mut live = std::fs::OpenOptions::new()
        .append(true)
        .open(&live_path)
        .unwrap();
    writeln!(live, "{}", event_line("B")).unwrap();
    drop(live);
    let appended =
        import_with_options_into(&live_root, &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(appended.report.raw_ops, 1);
    let expected_parent = legacy_stream
        .op_from_position(SourcePosition::raw(2))
        .unwrap();
    assert_eq!(appended.ops.ops[0].parents, ParentSet::One(expected_parent));
    assert_eq!(appended.ops.ops[0].id.node, legacy_stream.node);
}

#[test]
fn full_import_preserves_raw_bytes_and_spills_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let big1 = format!(
        "{{\"type\":\"event_msg\",\"payload\":{{\"blob\":\"{}\"}}}}",
        "x".repeat(5000)
    );
    let big2 = format!(
        "{{\"type\":\"event_msg\",\"payload\":{{\"blob\":\"{}\"}}}}",
        "y".repeat(6000)
    );
    let line1 = session_meta_line("thread-1", "PARENT-session");
    let raw_lines = [line1.clone(), big1.clone(), big2.clone()];
    write_rollout(dir.path(), "rollout-1.jsonl", &raw_lines);

    let harness = import(dir.path(), &helper_in(&dir, &messages_awk("thread-1")));

    assert_eq!(harness.report.files_discovered, 1);
    assert_eq!(harness.report.files_processed, 1);
    assert_eq!(harness.report.raw_ops, 3);
    assert_eq!(harness.report.normalized_ops, 2);
    assert_eq!(harness.report.malformed, 0);
    assert_eq!(harness.ops.ops.len(), 5);

    // Raw lane: session_meta inline, big lines spilled to blobs, byte-exact.
    assert_eq!(raw_bytes(&harness.ops.ops[0], &harness.blobs), ln(&line1));
    assert_eq!(harness.blobs.len(), 2);
    assert_eq!(raw_bytes(&harness.ops.ops[1], &harness.blobs), ln(&big1));
    assert_eq!(raw_bytes(&harness.ops.ops[2], &harness.blobs), ln(&big2));

    // Raw chain per physical file.
    assert_eq!(harness.ops.ops[0].parents, ParentSet::None);
    assert_eq!(
        harness.ops.ops[1].parents,
        ParentSet::One(harness.ops.ops[0].id)
    );
    assert_eq!(
        harness.ops.ops[2].parents,
        ParentSet::One(harness.ops.ops[1].id)
    );

    // Scope and actor lanes.
    let scope = ScopeRef::Session(derive_session_id("thread-1"));
    assert_eq!(harness.ops.ops[0].scope, scope);
    assert_eq!(harness.ops.ops[0].actor, derive_actor_id("system:thread-1"));
    assert_eq!(
        harness.ops.ops[1].actor,
        derive_actor_id("codex:event_msg:thread-1")
    );
    assert!(harness.ops.ops[0]
        .tags
        .matches_all(Tags::IMPORT | Tags::META));
    assert!(!harness.ops.ops[1]
        .tags
        .matches_any(Tags::META | Tags::STRUCTURAL));
    assert_eq!(
        harness.ops.ops[0].clock,
        Clock::UnixMs(parse_source_time("2026-08-26T12:00:00.000Z").unwrap())
    );

    // Normalized messages anchored to their first-seen raw ops and scoped to
    // their persisted turn identity (thread:turn-1).
    let turn_scope = ScopeRef::Turn(derive_turn_id("thread-1:turn-1"));
    for (i, expected_text) in [(3usize, "line-2"), (4, "line-3")] {
        let op = &harness.ops.ops[i];
        assert_eq!(op.parents, ParentSet::One(harness.ops.ops[i - 2].id));
        assert_eq!(op.scope, turn_scope);
        assert!(op.tags.matches_all(Tags::AGENT | Tags::MESSAGE));
        match &op.kind {
            OpKind::Message(m) => assert_eq!(
                m.content,
                Payload::Inline(expected_text.as_bytes().to_vec())
            ),
            other => panic!("expected message op, got {other:?}"),
        }
    }
}

#[test]
fn token_usage_and_terminal_events_fold_into_the_last_semantic_turn() {
    let dir = tempfile::tempdir().unwrap();
    let raw_lines = [
        session_meta_line("thread-1", "s"),
        event_line("SEMANTIC"),
        token_usage_line(),
        lifecycle_event_line("token_count"),
        lifecycle_event_line("task_complete"),
        lifecycle_event_line("turn_aborted"),
        lifecycle_event_line("task_started"),
    ];
    write_rollout(dir.path(), "rollout-1.jsonl", &raw_lines);

    let projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({"sessionId": "s", "threadId": "thread-1"})),
        ),
        line_record(
            2,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {"kind": "agentMessage", "id": "item-2", "text": "done"},
            })],
            None,
        ),
        line_record(3, Vec::new(), None),
        line_record(4, Vec::new(), None),
        line_record(5, Vec::new(), None),
        line_record(6, Vec::new(), None),
        line_record(7, Vec::new(), None),
    ]);
    let harness = import(dir.path(), &fixed_helper(&dir, &projection));

    assert_eq!(harness.report.raw_ops, 7);
    assert_eq!(harness.report.normalized_ops, 1);
    let raw = &harness.ops.ops[..7];
    assert!(!raw[1].tags.matches_any(Tags::META));
    for op in &raw[2..6] {
        assert!(
            op.tags.matches_all(Tags::IMPORT | Tags::META),
            "usage and terminal lifecycle records must be foldable metadata: {op:?}"
        );
    }
    assert!(
        !raw[6].tags.matches_any(Tags::META),
        "task_started is a turn prologue and must not fold backward"
    );
    assert_eq!(
        raw[2].actor,
        derive_actor_id("system:thread-1"),
        "top-level usage records belong to the system metadata lane"
    );

    let history = editchain_project::HistoryProjection::from_ops(harness.ops.ops.clone());
    let semantic_key = raw[1].id.to_string();
    let semantic = history
        .nodes()
        .into_iter()
        .find(|node| node.node_key() == semantic_key)
        .expect("semantic turn row");
    let bundled_ids: Vec<_> = semantic.sub_ops().iter().map(|op| op.id).collect();
    assert_eq!(
        bundled_ids,
        vec![raw[2].id, raw[3].id, raw[4].id, raw[5].id]
    );
    assert_eq!(
        history.nodes().len(),
        3,
        "session metadata, semantic turn, and task_started should remain top-level"
    );
}

#[test]
fn helper_prefix_args_are_passed_before_rollout_path() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[session_meta_line("thread-1", "s")],
    );
    // The fake helper takes the last argument as the file, so prefix args are
    // exercised without breaking invocation.
    let harness = import(
        dir.path(),
        &helper_with_args(&dir, &messages_awk("thread-1")),
    );
    assert_eq!(harness.report.raw_ops, 1);
    assert_eq!(harness.report.normalized_ops, 0);
}

#[test]
fn session_scope_uses_bridge_thread_not_payload_session_id() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[
            session_meta_line("thread-1", "PARENT-session"),
            event_line("A"),
        ],
    );
    let harness = import(dir.path(), &helper_in(&dir, &messages_awk("thread-1")));
    let scope = ScopeRef::Session(derive_session_id("thread-1"));
    let turn_scope = ScopeRef::Turn(derive_turn_id("thread-1:turn-1"));
    for op in &harness.ops.ops {
        let expected = if matches!(op.kind, OpKind::Import(_)) {
            scope
        } else {
            turn_scope
        };
        assert_eq!(
            op.scope, expected,
            "raw ops scope to the owning thread; normalized ops persist their turn identity"
        );
        assert_ne!(
            op.scope,
            ScopeRef::Session(derive_session_id("PARENT-session"))
        );
    }
}

#[test]
fn raw_session_meta_fallback_when_bridge_has_no_thread_metadata() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[
            session_meta_line("thread-1", "PARENT-session"),
            event_line("A"),
        ],
    );
    let no_meta_awk = r#"
{
  if ($0 ~ /"type":"session_meta"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"sessionMeta\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
    next
  }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"eventMsg\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-%d\",\"text\":\"line-%d\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR, NR, NR
}
"#;
    let helper = helper_in(&dir, no_meta_awk);
    let mut cursors = MemoryCursorStore::new();
    let harness =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    let scope = ScopeRef::Session(derive_session_id("thread-1"));
    let turn_scope = ScopeRef::Turn(derive_turn_id("thread-1:turn-1"));
    for op in &harness.ops.ops {
        let expected = if matches!(op.kind, OpKind::Import(_)) {
            scope
        } else {
            turn_scope
        };
        assert_eq!(op.scope, expected);
    }

    let path = dir.path().join("rollout-1.jsonl");
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{}", event_line("B")).unwrap();
    drop(file);
    let appended =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    for op in &appended.ops.ops {
        let expected = if matches!(op.kind, OpKind::Import(_)) {
            scope
        } else {
            turn_scope
        };
        assert_eq!(
            op.scope, expected,
            "raw session-meta fallback remains stable after the cursor"
        );
    }
}

#[test]
fn session_fallback_to_rollout_filename_stem() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(dir.path(), "rollout-solo-1.jsonl", &[event_line("A")]);
    let no_meta_awk = r#"
{
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"eventMsg\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-%d\",\"text\":\"line-%d\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR, NR, NR
}
"#;
    let harness = import(dir.path(), &helper_in(&dir, no_meta_awk));
    let scope = ScopeRef::Session(derive_session_id("rollout-solo-1"));
    assert_eq!(harness.ops.ops.len(), 2);
    for op in &harness.ops.ops {
        let expected = if matches!(op.kind, OpKind::Import(_)) {
            scope
        } else {
            ScopeRef::Turn(derive_turn_id("rollout-solo-1:turn-1"))
        };
        assert_eq!(op.scope, expected);
    }
}

#[test]
fn bridge_thread_beats_raw_session_meta() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[session_meta_line("raw-thread", "s")],
    );
    let harness = import(dir.path(), &helper_in(&dir, &messages_awk("bridge-thread")));
    assert_eq!(
        harness.ops.ops[0].scope,
        ScopeRef::Session(derive_session_id("bridge-thread"))
    );
}

#[test]
fn repeated_upserts_fold_echo_and_completion_repeats() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[
            session_meta_line("thread-1", "s"),
            event_line("ECHO_A_FIRST"),
            event_line("ECHO_A_SECOND"),
            event_line("COMPACT_B_FIRST"),
            event_line("COMPACT_B_REPLAY"),
            event_line("COMPACT_B_REPEAT"),
        ],
    );
    let awk = r#"
{
  if ($0 ~ /"token":"ECHO_A_FIRST"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"eventMsg\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-a\",\"text\":\"first\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"ECHO_A_SECOND"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"responseItem\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-a\",\"text\":\"first second\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"COMPACT_B_FIRST"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"eventMsg\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-b\",\"text\":\"b-first\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"COMPACT_B_REPLAY"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"compacted\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-b\",\"text\":\"b-final\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"COMPACT_B_REPEAT"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"responseItem\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-b\",\"text\":\"b-final\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"sessionMeta\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}
"#;
    let harness = import(dir.path(), &helper_in(&dir, awk));
    assert_eq!(harness.report.raw_ops, 6);
    assert_eq!(
        harness.report.normalized_ops, 2,
        "echo pair + compaction replay fold to one item each"
    );
    let messages: Vec<_> = harness
        .ops
        .ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::Message(_)))
        .collect();
    assert_eq!(messages.len(), 2);
    let message_text = |op: &editchain_core::Op| match &op.kind {
        OpKind::Message(m) => match &m.content {
            Payload::Inline(b) => String::from_utf8_lossy(b).into_owned(),
            other => panic!("unexpected payload {other:?}"),
        },
        _ => panic!("expected message op"),
    };
    assert_eq!(message_text(messages[0]), "first second");
    assert_eq!(message_text(messages[1]), "b-final");
    // Anchored at first-seen ordinals: derived(2,1) and derived(4,1).
    let path = dir.path().join("rollout-1.jsonl");
    let stream = source_stream(dir.path(), &path, 0);
    assert_eq!(
        messages[0].id,
        stream
            .op_from_position(SourcePosition::derived(2, 1))
            .unwrap()
    );
    assert_eq!(
        messages[1].id,
        stream
            .op_from_position(SourcePosition::derived(4, 1))
            .unwrap()
    );
}

#[test]
fn removed_turn_ids_rollback_items() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[
            session_meta_line("thread-1", "s"),
            event_line("T1_A"),
            event_line("T1_B"),
            event_line("ROLLBACK"),
            event_line("T2_C"),
        ],
    );
    let awk = r#"
{
  if ($0 ~ /"token":"T1_A"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"a\",\"text\":\"a\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"T1_B"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"b\",\"text\":\"b\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"ROLLBACK"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[\"turn-1\"]}}\n", NR; next }
  if ($0 ~ /"token":"T2_C"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-2\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"c\",\"text\":\"c\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}
"#;
    let harness = import(dir.path(), &helper_in(&dir, awk));
    assert_eq!(harness.report.normalized_ops, 1);
    let messages: Vec<_> = harness
        .ops
        .ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::Message(_)))
        .collect();
    assert_eq!(messages.len(), 1);
    match &messages[0].kind {
        OpKind::Message(m) => {
            assert_eq!(m.content, Payload::Inline(b"c".to_vec()));
        }
        _ => panic!("expected message op"),
    }
}

#[test]
fn distinct_files_with_reused_item_ids_never_dedup() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-a.jsonl",
        &[session_meta_line("thread-a", "s"), event_line("FILE_A")],
    );
    write_rollout(
        dir.path(),
        "rollout-b.jsonl",
        &[session_meta_line("thread-b", "s"), event_line("FILE_B")],
    );
    let awk = r#"
{
  if ($0 ~ /"token":"FILE_A"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-shared\",\"text\":\"from-a\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"FILE_B"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-shared\",\"text\":\"from-b\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}
"#;
    let harness = import(dir.path(), &helper_in(&dir, awk));
    assert_eq!(harness.report.files_discovered, 2);
    assert_eq!(harness.report.raw_ops, 4);
    assert_eq!(
        harness.report.normalized_ops, 2,
        "reused item ids across files are never deduplicated"
    );
    let mut texts: Vec<String> = harness
        .ops
        .ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::Message(_)))
        .map(|op| match &op.kind {
            OpKind::Message(m) => match &m.content {
                Payload::Inline(b) => String::from_utf8_lossy(b).into_owned(),
                other => panic!("unexpected payload {other:?}"),
            },
            _ => panic!("expected message op"),
        })
        .collect();
    texts.sort();
    assert_eq!(texts, vec!["from-a".to_string(), "from-b".to_string()]);
}

#[test]
fn legacy_and_paginated_physical_ordinals() {
    let dir = tempfile::tempdir().unwrap();
    // Legacy: no optional Codex ordinal anywhere.
    write_rollout(
        dir.path(),
        "rollout-legacy.jsonl",
        &[session_meta_line("t-l", "s"), event_line("LEGACY")],
    );

    let harness = import(dir.path(), &helper_in(&dir, &messages_awk("thread-1")));
    assert_eq!(
        harness.report.raw_ops, 2,
        "legacy file has 2 physical lines"
    );
    assert_eq!(harness.report.normalized_ops, 1, "one message item");
    assert_eq!(harness.report.malformed, 0);

    // Paginated: payload carries its own Codex ordinal that differs from the
    // physical line number; projection ordinals must be physical. Imported in
    // isolation so op identity can be checked without sibling-file ambiguity.
    let paged_dir = tempfile::tempdir().unwrap();
    let paged_meta = session_meta_line("t-p", "s");
    let paged_line =
        r#"{"timestamp":"t","type":"event_msg","payload":{"type":"agent_message","ordinal":100,"token":"PAGED"}}"#
            .to_string();
    write_rollout(
        paged_dir.path(),
        "rollout-paged.jsonl",
        &[paged_meta, paged_line],
    );
    let paged = import(
        paged_dir.path(),
        &helper_in(&paged_dir, &messages_awk("thread-1")),
    );
    assert_eq!(paged.report.raw_ops, 2);
    assert_eq!(paged.report.normalized_ops, 1);
    let paged_path = paged_dir.path().join("rollout-paged.jsonl");
    let stream = source_stream(paged_dir.path(), &paged_path, 0);
    let paged_msg = paged
        .ops
        .ops
        .iter()
        .find(|op| matches!(&op.kind, OpKind::Message(m) if m.content == Payload::Inline(b"line-2".to_vec())))
        .unwrap();
    assert_eq!(
        paged_msg.id,
        stream
            .op_from_position(SourcePosition::derived(2, 1))
            .unwrap(),
        "normalized op uses the physical line ordinal, not the Codex ordinal"
    );
}

#[test]
fn deterministic_ids_and_second_import_idempotency() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[session_meta_line("thread-1", "s"), event_line("A")],
    );

    let a = import(dir.path(), &helper_in(&dir, &messages_awk("thread-1")));
    let b = import(dir.path(), &helper_in(&dir, &messages_awk("thread-1")));
    assert_eq!(
        a.ops.ops, b.ops.ops,
        "deterministic ids and content across runs"
    );

    // Idempotent second import: unchanged files are skipped via cursors.
    let mut cursors = MemoryCursorStore::new();
    let first = import_with_options_into(
        dir.path(),
        &helper_in(&dir, &messages_awk("thread-1")),
        &ImportOptions::default(),
        &mut cursors,
    );
    let second = import_with_options_into(
        dir.path(),
        &helper_in(&dir, &messages_awk("thread-1")),
        &ImportOptions::default(),
        &mut cursors,
    );
    assert_eq!(second.report.files_processed, 0, "unchanged file skipped");
    assert!(second.ops.ops.is_empty());
    assert_eq!(first.report.files_processed, 1);
}

#[test]
fn incremental_append_chains_across_batches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-incr.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            session_meta_line("thread-1", "s"),
            event_line("A")
        ),
    )
    .unwrap();

    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let mut cursors = MemoryCursorStore::new();
    let run1 =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(run1.report.raw_ops, 2);
    assert_eq!(run1.report.normalized_ops, 1);

    // Append two more lines; the helper re-projects the whole file.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "{}", event_line("B")).unwrap();
    writeln!(f, "{}", event_line("C")).unwrap();
    drop(f);

    let run2 =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(run2.report.files_processed, 1);
    assert_eq!(
        run2.report.raw_ops, 2,
        "only the appended lines are re-read"
    );
    assert_eq!(
        run2.report.normalized_ops, 2,
        "only items first seen after the cursor"
    );

    // Raw chain continuity across the cursor boundary: first new raw op parents
    // to the raw op at ordinal 2 from run 1.
    let stream = source_stream(dir.path(), &path, 0);
    let expected_prev = stream.op_from_position(SourcePosition::raw(2)).unwrap();
    assert_eq!(run2.ops.ops[0].parents, ParentSet::One(expected_prev));
    assert_eq!(run2.ops.ops[1].parents, ParentSet::One(run2.ops.ops[0].id));

    // Cursor advanced.
    let key = source_key(dir.path(), &path);
    let cursor = cursors.get_cursor(&key).unwrap().unwrap();
    assert_eq!(cursor.ops_emitted, 4);
}

#[test]
fn incremental_import_rejects_helper_output_that_omits_the_new_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-incr.jsonl");
    std::fs::write(&path, format!("{}\n", session_meta_line("thread-1", "s"))).unwrap();
    let first_projection = b"{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{\"threadId\":\"thread-1\"}}}\n";
    let mut cursors = MemoryCursorStore::new();
    let first = import_with_options_into(
        dir.path(),
        &fixed_helper(&dir, first_projection),
        &ImportOptions::default(),
        &mut cursors,
    );
    assert_eq!(first.report.raw_ops, 1);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{}", event_line("NEW")).unwrap();
    drop(file);

    let err = try_import(
        dir.path(),
        &fixed_helper(&dir, first_projection),
        &ImportOptions::default(),
        &mut cursors,
    )
    .expect_err("missing projection for the appended line must fail");
    assert!(matches!(
        err,
        ImportError::ProjectionProtocol { ref detail, .. }
            if detail.contains("source ordinal 2")
    ));
    let key = source_key(dir.path(), &path);
    let cursor = cursors.get_cursor(&key).unwrap().unwrap();
    assert_eq!(
        cursor.ops_emitted, 1,
        "failed import must not advance cursor"
    );
}

#[test]
fn incremental_append_emits_deterministic_update_for_item_changed_after_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-upd.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            session_meta_line("thread-1", "s"),
            event_line("A")
        ),
    )
    .unwrap();

    let awk = r#"
{
  if ($0 ~ /"type":"session_meta"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"sessionMeta\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{\"sessionId\":\"s\",\"threadId\":\"thread-1\"}}}\n", NR
    next
  }
  if ($0 ~ /"token":"B"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"msg-1\",\"text\":\"first second\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
    next
  }
  if ($0 ~ /"token":"C"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"msg-1\",\"text\":\"first second third\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
    next
  }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"msg-1\",\"text\":\"first\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}
"#;
    let helper = helper_in(&dir, awk);
    let stream = source_stream(dir.path(), &path, 0);
    let mut cursors = MemoryCursorStore::new();

    let run1 =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(run1.report.raw_ops, 2);
    assert_eq!(run1.report.normalized_ops, 1);
    let original = run1
        .ops
        .ops
        .iter()
        .find(|o| matches!(o.kind, OpKind::Message(_)))
        .expect("initial message op");
    assert_eq!(
        original.id,
        stream
            .op_from_position(SourcePosition::derived(2, 1))
            .unwrap()
    );
    match &original.kind {
        OpKind::Message(m) => {
            assert_eq!(m.content, Payload::Inline(b"first".to_vec()));
        }
        _ => panic!("expected message op"),
    }

    // Append a line that re-upserts the same logical item with new content.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "{}", event_line("B")).unwrap();
    drop(f);

    let run2 =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(run2.report.raw_ops, 1, "only the appended line is re-read");
    assert_eq!(
        run2.report.normalized_ops, 1,
        "deterministic update for the item changed after the cursor"
    );
    let update = run2
        .ops
        .ops
        .iter()
        .find(|o| matches!(o.kind, OpKind::Message(_)))
        .expect("update op");
    assert_eq!(
        update.id,
        stream
            .op_from_position(SourcePosition::derived(3, 1))
            .unwrap(),
        "update anchored at the change ordinal (last_seen)"
    );
    assert_eq!(
        update.parents,
        ParentSet::One(stream.op_from_position(SourcePosition::raw(3)).unwrap())
    );
    match &update.kind {
        OpKind::Message(m) => {
            assert_eq!(m.content, Payload::Inline(b"first second".to_vec()));
        }
        _ => panic!("expected message op"),
    }

    // A later append again emits the final content — no stale content remains.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "{}", event_line("C")).unwrap();
    drop(f);

    let run3 =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(run3.report.raw_ops, 1);
    assert_eq!(run3.report.normalized_ops, 1);
    let update3 = run3
        .ops
        .ops
        .iter()
        .find(|o| matches!(o.kind, OpKind::Message(_)))
        .expect("update op");
    assert_eq!(
        update3.id,
        stream
            .op_from_position(SourcePosition::derived(4, 1))
            .unwrap()
    );
    match &update3.kind {
        OpKind::Message(m) => {
            assert_eq!(
                m.content,
                Payload::Inline(b"first second third".to_vec()),
                "update carries the latest content"
            );
        }
        _ => panic!("expected message op"),
    }
}

#[test]
fn helper_nonzero_exit_is_error_without_cursor() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(dir.path(), "rollout-1.jsonl", &[event_line("A")]);
    let script = dir.path().join("fail-helper.sh");
    std::fs::write(&script, "#!/bin/sh\necho boom >&2\nexit 3\n").unwrap();
    chmod_x(&script);
    let helper = sh_helper(&script, &[]);

    let mut cursors = MemoryCursorStore::new();
    let err = try_import(dir.path(), &helper, &ImportOptions::default(), &mut cursors).unwrap_err();
    match err {
        ImportError::HelperFailed {
            path,
            exit_code,
            stderr,
            ..
        } => {
            assert_eq!(path, dir.path().join("rollout-1.jsonl"));
            assert_eq!(exit_code, Some(3));
            assert!(stderr.contains("boom"));
        }
        other => panic!("expected HelperFailed, got {other:?}"),
    }
    let path = dir.path().join("rollout-1.jsonl");
    let key = source_key(dir.path(), &path);
    assert!(
        cursors.get_cursor(&key).unwrap().is_none(),
        "cursor not persisted on helper failure"
    );
}

#[test]
fn schema_mismatch_is_error_without_cursor() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(dir.path(), "rollout-1.jsonl", &[event_line("A")]);
    let bad = b"{\"schemaVersion\":\"editchain-v2\",\"recordType\":\"line\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"}}\n";
    let helper = fixed_helper(&dir, bad);

    let mut cursors = MemoryCursorStore::new();
    let err = try_import(dir.path(), &helper, &ImportOptions::default(), &mut cursors).unwrap_err();
    match err {
        ImportError::ProjectionProtocol { detail, .. } => {
            assert!(detail.contains("schema version"));
        }
        other => panic!("expected ProjectionProtocol, got {other:?}"),
    }
    let path = dir.path().join("rollout-1.jsonl");
    let key = source_key(dir.path(), &path);
    assert!(cursors.get_cursor(&key).unwrap().is_none());
}

#[test]
fn missing_ordinal_is_error_without_cursor() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(dir.path(), "rollout-1.jsonl", &[event_line("A")]);
    let bad = b"{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"decode\":{\"status\":\"ok\"}}\n";
    let helper = fixed_helper(&dir, bad);

    let mut cursors = MemoryCursorStore::new();
    let err = try_import(dir.path(), &helper, &ImportOptions::default(), &mut cursors).unwrap_err();
    match err {
        ImportError::ProjectionProtocol { detail, .. } => {
            assert!(detail.contains("missing ordinal `sourceOrdinal`"));
        }
        other => panic!("expected ProjectionProtocol, got {other:?}"),
    }
    let path = dir.path().join("rollout-1.jsonl");
    let key = source_key(dir.path(), &path);
    assert!(cursors.get_cursor(&key).unwrap().is_none());
}

#[test]
fn trailing_partial_line_is_tolerated_and_aligned() {
    let dir = tempfile::tempdir().unwrap();
    // Two complete lines plus a non-blank partial line (no trailing newline).
    let content = format!(
        "{}\n{}\nPARTIAL",
        session_meta_line("thread-1", "s"),
        event_line("A")
    );
    let path = dir.path().join("rollout-1.jsonl");
    std::fs::write(&path, content).unwrap();

    // The bridge counts the partial line and emits a decode-error record for it.
    let projection =
        "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n\
         {\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":2,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"a\",\"text\":\"hi\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n\
         {\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":3,\"decode\":{\"status\":\"error\",\"diagnostic\":\"invalid JSON\",\"kind\":\"unknownJson\"},\"projection\":{}}\n"
    ;
    let helper = fixed_helper(&dir, projection.as_bytes());
    let harness = import(dir.path(), &helper);

    assert_eq!(harness.report.raw_ops, 2, "partial line has no raw op");
    assert_eq!(harness.report.normalized_ops, 1);
    assert_eq!(
        harness.report.malformed, 1,
        "decode-error partial line is diagnostic"
    );
}

#[test]
fn whitespace_partial_line_emits_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let content = format!(
        "{}\n{}\n  ",
        session_meta_line("thread-1", "s"),
        event_line("A")
    );
    let path = dir.path().join("rollout-1.jsonl");
    std::fs::write(&path, content).unwrap();

    let projection =
        "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n\
         {\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":2,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"a\",\"text\":\"hi\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n"
    ;
    let helper = fixed_helper(&dir, projection.as_bytes());
    let harness = import(dir.path(), &helper);
    assert_eq!(harness.report.raw_ops, 2);
    assert_eq!(harness.report.normalized_ops, 1);
    assert_eq!(harness.report.malformed, 0);
}

#[test]
fn blank_lines_produce_raw_ops_but_no_records() {
    let dir = tempfile::tempdir().unwrap();
    let content = format!(
        "{}\n\n{}\n",
        session_meta_line("thread-1", "s"),
        event_line("A")
    );
    let path = dir.path().join("rollout-1.jsonl");
    std::fs::write(&path, content).unwrap();

    // Records at physical ordinals 1 and 3; line 2 is blank.
    let projection =
        "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n\
         {\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":3,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"a\",\"text\":\"hi\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n"
    ;
    let helper = fixed_helper(&dir, projection.as_bytes());
    let harness = import(dir.path(), &helper);
    assert_eq!(
        harness.report.raw_ops, 3,
        "blank line preserved byte-exact in the raw lane"
    );
    assert_eq!(harness.report.normalized_ops, 1);
    assert_eq!(harness.report.malformed, 0);
}

#[test]
fn reasoning_is_private_and_respects_include_thinking() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[session_meta_line("thread-1", "s"), event_line("REASON")],
    );
    let awk = r#"
{
  if ($0 ~ /"token":"REASON"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"reasoning\",\"id\":\"r-1\",\"summary\":[\"step one\",\"step two\"],\"contentLength\":42}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
    next
  }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}
"#;
    let helper = helper_in(&dir, awk);

    let hidden = import_with_options(
        dir.path(),
        &helper,
        &ImportOptions {
            normalize: true,
            include_thinking: false,
            max_inline_bytes: 4096,
        },
    );
    assert_eq!(
        hidden.report.normalized_ops, 0,
        "reasoning stays raw-only without include_thinking"
    );

    let shown = import_with_options(
        dir.path(),
        &helper,
        &ImportOptions {
            normalize: true,
            include_thinking: true,
            max_inline_bytes: 4096,
        },
    );
    assert_eq!(shown.report.normalized_ops, 1);
    match &shown.ops.ops[2].kind {
        OpKind::Reflection(r) => {
            assert!(shown.ops.ops[2]
                .tags
                .matches_all(Tags::PRIVATE | Tags::REFLECTION));
            assert_eq!(r.summary, Payload::Inline(b"step one\nstep two".to_vec()));
        }
        other => panic!("expected reflection op, got {other:?}"),
    }
}

#[test]
fn kinds_map_full_content_to_neutral_ops() {
    let dir = tempfile::tempdir().unwrap();
    let lines = [
        session_meta_line("thread-1", "s"),
        event_line("K_TOOL"),
        event_line("K_CMD"),
        event_line("K_FILE"),
        event_line("K_IMG"),
        event_line("K_PLAN"),
        event_line("K_SUB"),
        event_line("K_USER"),
        event_line("K_AGENT"),
    ];
    write_rollout(dir.path(), "rollout-1.jsonl", &lines);

    let awk = r#"
{
  if ($0 ~ /"token":"K_TOOL"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"toolCall\",\"id\":\"call-1\",\"tool\":\"shell\",\"status\":\"completed\",\"arguments\":{\"cmd\":\"ls -la\"},\"result\":{\"stdout\":\"total 0\"},\"outputLength\":123}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"K_CMD"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"commandExecution\",\"id\":\"cmd-1\",\"command\":\"ls -la\",\"status\":\"completed\",\"aggregatedOutput\":\"total 0\\n\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"K_FILE"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"fileChange\",\"id\":\"f-1\",\"status\":\"applied\",\"changes\":[{\"path\":\"/tmp/x.txt\",\"kind\":\"update\",\"diff\":\"@@ -1 +1 @@\\n-old\\n+new\"}]}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"K_IMG"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"imageView\",\"id\":\"img-1\",\"path\":\"/tmp/img.png\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"K_PLAN"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"plan\",\"id\":\"p-1\",\"text\":\"1. do thing\\n2. profit\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"K_SUB"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"subAgentActivity\",\"id\":\"sub-1\",\"activityKind\":\"started\",\"agentThreadId\":\"sub-thread\",\"agentPath\":\"/root/sub\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"K_USER"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"userMessage\",\"id\":\"u-1\",\"text\":\"please help\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"K_AGENT"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"m-1\",\"text\":\"on it\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}
"#;
    let harness = import_with_options(
        dir.path(),
        &helper_in(&dir, awk),
        &ImportOptions {
            normalize: true,
            include_thinking: true,
            max_inline_bytes: 4096,
        },
    );
    assert_eq!(harness.report.raw_ops, 9);
    assert_eq!(
        harness.report.normalized_ops, 10,
        "one op per known-kind item plus an annotated path note per file item"
    );

    let ops = &harness.ops.ops;
    let inline = |p: &Payload| match p {
        Payload::Inline(b) => String::from_utf8_lossy(b).into_owned(),
        other => panic!("expected inline payload, got {other:?}"),
    };

    // Tool: identity + name mapped; arguments and result are real content now
    // (the length-only `outputLength` field contributes nothing).
    let tool = ops
        .iter()
        .find(|o| matches!(o.kind, OpKind::Tool(_)))
        .expect("tool op");
    match &tool.kind {
        OpKind::Tool(t) => {
            assert_eq!(t.tool_call_id, Payload::Inline(b"call-1".to_vec()));
            assert_eq!(t.tool_name, Payload::Inline(b"shell".to_vec()));
            assert_eq!(t.stage, ToolStage::Finish);
            let content = inline(&t.content);
            assert!(
                content.contains("{\"cmd\":\"ls -la\"}"),
                "args content: {content}"
            );
            assert!(
                content.contains("{\"stdout\":\"total 0\"}"),
                "result content: {content}"
            );
            assert!(
                !content.contains("outputLength"),
                "length-only fields are not content"
            );
        }
        other => panic!("expected tool op, got {other:?}"),
    }

    let cmd = ops
        .iter()
        .find(|o| matches!(o.kind, OpKind::Command(_)))
        .expect("command op");
    match &cmd.kind {
        OpKind::Command(c) => {
            assert_eq!(c.command_id, Payload::Inline(b"cmd-1".to_vec()));
            let content = inline(&c.content);
            assert_eq!(content, "ls -la\ntotal 0\n");
            assert_eq!(c.stage, CommandStage::Finish);
        }
        other => panic!("expected command op, got {other:?}"),
    }

    let file = ops
        .iter()
        .find(|o| matches!(o.kind, OpKind::File(_)))
        .expect("file op");
    match &file.kind {
        OpKind::File(f) => {
            assert_eq!(f.path, derive_path_id("/tmp/x.txt"));
            assert_eq!(f.stage, editchain_core::op::FileStage::Applied);
            match &f.edit {
                editchain_core::op::FileEdit::UnifiedDiff(diff) => {
                    assert_eq!(inline(diff), "@@ -1 +1 @@\n-old\n+new");
                }
                other => panic!("expected unified diff edit, got {other:?}"),
            }
        }
        other => panic!("expected file op, got {other:?}"),
    }
    // File items persist their provider-neutral path text as an explicit
    // annotation note targeting the file op.
    let file_note = ops.iter().find(|o| {
        matches!(&o.kind, OpKind::Note(n)
            if n.target_ids.first() == Some(&file.id) && n.content == Payload::Inline(b"/tmp/x.txt".to_vec()))
    });
    assert!(
        file_note.is_some(),
        "file path note targets the file op and carries the path text"
    );

    // Plan text is real content now: a non-private reflection summary.
    let plan = ops.iter().find(|o| {
        matches!(&o.kind, OpKind::Reflection(r) if r.summary == Payload::Inline(b"1. do thing\n2. profit".to_vec()))
    });
    assert!(plan.is_some(), "plan maps to a reflection with its text");
    assert!(
        !plan.unwrap().tags.matches_any(Tags::PRIVATE),
        "plan is not private"
    );

    let note = ops
        .iter()
        .find(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.content == Payload::Inline(b"spawned subagent sub-thread (path /root/sub)".to_vec()))
        })
        .expect("subagent note op");
    match &note.kind {
        OpKind::Note(n) => {
            assert_eq!(
                n.content,
                Payload::Inline(b"spawned subagent sub-thread (path /root/sub)".to_vec())
            );
            assert!(
                n.target_ids.is_empty(),
                "subagent activity notes are standalone (no op targets)"
            );
        }
        other => panic!("expected note op, got {other:?}"),
    }

    // User/agent messages keep actor lanes.
    let user = ops.iter().find(|o| matches!(&o.kind, OpKind::Message(m) if m.content == Payload::Inline(b"please help".to_vec()))).expect("user message");
    assert!(user.tags.matches_all(Tags::HUMAN | Tags::MESSAGE));
    let agent = ops.iter().find(|o| matches!(&o.kind, OpKind::Message(m) if m.content == Payload::Inline(b"on it".to_vec()))).expect("agent message");
    assert!(agent.tags.matches_all(Tags::AGENT | Tags::MESSAGE));

    // The imageView path lanes into File too.
    let img = ops
        .iter()
        .find(|o| matches!(&o.kind, OpKind::File(f) if f.path == derive_path_id("/tmp/img.png")));
    assert!(img.is_some());
}

#[test]
fn multi_path_file_change_retains_one_edit_and_path_note_per_file() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-multi-file.jsonl",
        &[session_meta_line("thread-1", "s"), event_line("MULTI_FILE")],
    );
    let projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({
                "sessionId": "s",
                "threadId": "thread-1",
                "cwd": "/workspace"
            })),
        ),
        line_record(
            2,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "fileChange",
                    "id": "files-1",
                    "status": "applied",
                    "changes": [
                        {"path": "src/a.rs", "kind": "update", "diff": "@@ -1 +1 @@\n-old a\n+new a"},
                        {"path": "src/b.rs", "kind": "delete", "diff": "@@ -1 +0,0 @@\n-old b"},
                        {"path": "src/a.rs", "kind": "update", "diff": "@@ -3 +3 @@\n-old c\n+new c"}
                    ]
                }
            })],
            None,
        ),
    ]);
    let imported = import(dir.path(), &fixed_helper(&dir, &projection));
    let files: Vec<_> = imported
        .ops
        .ops
        .iter()
        .filter_map(|op| match &op.kind {
            OpKind::File(file) => Some((op, file)),
            _ => None,
        })
        .collect();
    assert_eq!(files.len(), 2, "every distinct path becomes a FileOp");
    assert_eq!(files[0].1.path, derive_path_id("src/a.rs"));
    assert_eq!(files[1].1.path, derive_path_id("src/b.rs"));
    assert_eq!(files[0].1.stage, editchain_core::op::FileStage::Applied);
    assert_eq!(files[1].1.stage, editchain_core::op::FileStage::Deleted);
    match &files[0].1.edit {
        editchain_core::op::FileEdit::UnifiedDiff(Payload::Inline(diff)) => assert_eq!(
            String::from_utf8_lossy(diff),
            "@@ -1 +1 @@\n-old a\n+new a\n@@ -3 +3 @@\n-old c\n+new c"
        ),
        other => panic!("expected path-specific unified diff, got {other:?}"),
    }
    match &files[1].1.edit {
        editchain_core::op::FileEdit::UnifiedDiff(Payload::Inline(diff)) => {
            assert_eq!(String::from_utf8_lossy(diff), "@@ -1 +0,0 @@\n-old b");
        }
        other => panic!("expected delete unified diff, got {other:?}"),
    }

    let path_notes: Vec<_> = imported
        .ops
        .ops
        .iter()
        .filter_map(|op| match &op.kind {
            OpKind::Note(note)
                if note.relationship == editchain_core::op::NoteRelationship::Explains =>
            {
                Some(note)
            }
            _ => None,
        })
        .collect();
    assert_eq!(path_notes.len(), 2);
    assert_eq!(path_notes[0].target_ids, vec![files[0].0.id]);
    assert_eq!(path_notes[0].content, Payload::Inline(b"src/a.rs".to_vec()));
    assert_eq!(path_notes[1].target_ids, vec![files[1].0.id]);
    assert_eq!(path_notes[1].content, Payload::Inline(b"src/b.rs".to_vec()));
    assert_eq!(imported.report.normalized_ops, 4);
}

#[test]
fn tool_lifecycle_split_uses_first_and_last_seen_lanes() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[
            session_meta_line("thread-1", "s"),
            event_line("K_ARGS"),
            event_line("K_RESULT").replace("12:00:01.000", "12:00:02.000"),
        ],
    );
    let awk = r#"
{
  if ($0 ~ /"token":"K_ARGS"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"toolCall\",\"id\":\"call-1\",\"tool\":\"shell\",\"status\":\"running\",\"arguments\":{\"cmd\":\"git status\"}}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  if ($0 ~ /"token":"K_RESULT"/) { printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"toolCall\",\"id\":\"call-1\",\"tool\":\"shell\",\"status\":\"running\",\"arguments\":{\"cmd\":\"git status\"},\"result\":{\"stdout\":\"clean\"}}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR; next }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}
"#;
    let harness = import(dir.path(), &helper_in(&dir, awk));
    assert_eq!(
        harness.report.normalized_ops, 2,
        "Start (args) + Finish (result)"
    );

    let path = dir.path().join("rollout-1.jsonl");
    let stream = source_stream(dir.path(), &path, 0);
    let start = harness
        .ops
        .ops
        .iter()
        .find(|o| matches!(&o.kind, OpKind::Tool(t) if t.stage == ToolStage::Start))
        .expect("start op");
    assert_eq!(
        start.id,
        stream
            .op_from_position(SourcePosition::derived(2, 1))
            .unwrap(),
        "start anchored at first-seen ordinal"
    );
    assert_eq!(
        start.parents,
        ParentSet::One(stream.op_from_position(SourcePosition::raw(2)).unwrap())
    );
    assert_eq!(
        start.clock,
        Clock::UnixMs(parse_source_time("2026-08-26T12:00:01.000Z").unwrap())
    );
    match &start.kind {
        OpKind::Tool(t) => {
            assert_eq!(
                t.content,
                Payload::Inline(b"{\"cmd\":\"git status\"}".to_vec()),
                "start carries the arguments"
            );
        }
        _ => panic!("expected tool op"),
    }

    let finish = harness
        .ops
        .ops
        .iter()
        .find(|o| matches!(&o.kind, OpKind::Tool(t) if t.stage == ToolStage::Finish))
        .expect("finish op");
    assert_eq!(
        finish.id,
        stream
            .op_from_position(SourcePosition::derived(3, 1))
            .unwrap(),
        "finish anchored at last-seen ordinal with a deterministic lane"
    );
    assert_eq!(
        finish.parents,
        ParentSet::One(stream.op_from_position(SourcePosition::raw(3)).unwrap())
    );
    assert_eq!(
        finish.clock,
        Clock::UnixMs(parse_source_time("2026-08-26T12:00:02.000Z").unwrap()),
        "finish uses the last-seen line's timestamp"
    );
    match &finish.kind {
        OpKind::Tool(t) => {
            assert_eq!(
                t.content,
                Payload::Inline(b"{\"stdout\":\"clean\"}".to_vec()),
                "finish carries the result"
            );
        }
        _ => panic!("expected tool op"),
    }
}

#[test]
fn inter_agent_and_compaction_lines_normalize_to_note_and_reflection() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[
            session_meta_line("thread-1", "s"),
            event_line("IA"),
            event_line("COMPACT"),
        ],
    );
    let awk = r#"
{
  if ($0 ~ /"type":"session_meta"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"sessionMeta\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{\"sessionId\":\"s\",\"threadId\":\"thread-1\"}}}\n", NR
    next
  }
  if ($0 ~ /"token":"IA"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"interAgent\":{\"id\":\"ia-1\",\"author\":\"sub\",\"recipient\":\"main\",\"content\":\"syncing with main agent\"}}}\n", NR
    next
  }
  if ($0 ~ /"token":"COMPACT"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"contextCompaction\",\"id\":\"cc-1\"}}],\"changedTurns\":[],\"removedTurnIds\":[],\"compacted\":{\"message\":\"context compacted: earlier turns summarized\",\"replacementCount\":2}}}\n", NR
    next
  }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}

"#;
    let harness = import(dir.path(), &helper_in(&dir, awk));
    assert_eq!(harness.report.raw_ops, 3);
    assert_eq!(
        harness.report.normalized_ops, 2,
        "inter-agent note + compaction reflection"
    );

    let ops = &harness.ops.ops;
    let path = dir.path().join("rollout-1.jsonl");
    let stream = source_stream(dir.path(), &path, 0);

    let note = ops
        .iter()
        .find(|o| matches!(o.kind, OpKind::Note(_)))
        .expect("inter-agent note");
    assert_eq!(
        note.id,
        stream
            .op_from_position(SourcePosition::derived(2, 1))
            .unwrap(),
        "inter-agent note anchored at its physical line with a deterministic lane"
    );
    assert_eq!(
        note.parents,
        ParentSet::One(stream.op_from_position(SourcePosition::raw(2)).unwrap())
    );
    assert!(note.tags.matches_all(Tags::NOTE));
    // Inter-agent lines render as a readable author → recipient summary; the
    // raw lane keeps the physical line byte-exact regardless.
    match &note.kind {
        OpKind::Note(n) => {
            assert_eq!(
                n.content,
                Payload::Inline(b"sub \xe2\x86\x92 main: syncing with main agent".to_vec())
            );
        }
        _ => panic!("expected note op"),
    }

    let reflection = ops
        .iter()
        .find(|o| matches!(o.kind, OpKind::Reflection(_)))
        .expect("compaction reflection");
    assert_eq!(
        reflection.id,
        stream
            .op_from_position(SourcePosition::derived(3, 1))
            .unwrap(),
        "compaction reflection anchored at its physical line"
    );
    assert!(reflection.tags.matches_all(Tags::REFLECTION));
    assert!(
        !reflection.tags.matches_any(Tags::PRIVATE),
        "compaction summary is not private"
    );
    match &reflection.kind {
        OpKind::Reflection(r) => {
            assert_eq!(
                r.summary,
                Payload::Inline(b"context compacted: earlier turns summarized".to_vec())
            );
        }
        _ => panic!("expected reflection op"),
    }
}

#[test]
fn normalized_items_on_the_same_line_use_distinct_derived_lanes() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[session_meta_line("thread-1", "s"), event_line("PAIR")],
    );
    let projection = concat!(
        "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{\"threadId\":\"thread-1\"}}}\n",
        "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":2,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"userMessage\",\"id\":\"u-1\",\"text\":\"first\"}},{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"a-1\",\"text\":\"second\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n"
    );
    let harness = import(dir.path(), &fixed_helper(&dir, projection.as_bytes()));

    let path = dir.path().join("rollout-1.jsonl");
    let stream = source_stream(dir.path(), &path, 0);
    let mut ids = harness
        .ops
        .ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::Message(_)))
        .map(|op| op.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();

    let mut expected = vec![
        stream
            .op_from_position(SourcePosition::derived(2, 1))
            .unwrap(),
        stream
            .op_from_position(SourcePosition::derived(2, 2))
            .unwrap(),
    ];
    expected.sort_unstable();
    assert_eq!(
        ids, expected,
        "same-line normalized op ids must not collide"
    );
}

#[test]
fn current_collab_spawn_links_exact_child_and_reconnects_to() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-parent.jsonl",
        &[
            session_meta_line("parent-1", "s"),
            event_line("P_SPAWN"),
            event_line("P_COLLAB"),
        ],
    );
    write_rollout(
        dir.path(),
        "rollout-sub.jsonl",
        &[session_meta_line("sub-1", "s"), event_line("SUB_WORK")],
    );

    // Projection streams are built with serde_json, so no bridge JSON ever
    // passes through shell/awk quoting. Current Codex rollouts identify the
    // exact child directly on the `spawnAgent` collab call. A later collab
    // call's per-child `agentsStates` marks it completed — the child's own
    // status is the only completion signal.
    let parent_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({"sessionId": "s", "threadId": "parent-1"})),
        ),
        line_record(
            2,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "collabToolCall",
                    "id": "spawn-1",
                    "tool": "spawnAgent",
                    "status": "completed",
                    "senderThreadId": "parent-1",
                    "receiverThreadIds": ["sub-1"],
                    "agentsStates": {"sub-1": {"status": "pendingInit"}},
                }
            })],
            None,
        ),
        line_record(
            3,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "collabToolCall",
                    "id": "call-1",
                    "tool": "wait",
                    "status": "completed",
                    "senderThreadId": "parent-1",
                    "receiverThreadIds": ["sub-1"],
                    "agentsStates": {"sub-1": {"status": "completed", "message": "done"}},
                }
            })],
            None,
        ),
    ]);
    let sub_projection = projection_bytes(&[
        // Real copied-subagent metadata: parentThreadId == forkedFromId. The
        // explicit subagent provenance suppresses the ForkOf edge.
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({
                "sessionId": "s",
                "threadId": "sub-1",
                "parentThreadId": "parent-1",
                "forkedFromId": "parent-1",
                "agentPath": "/root/sub",
            })),
        ),
        line_record(
            2,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {"kind": "agentMessage", "id": "item-2", "text": "sub work"},
            })],
            None,
        ),
    ]);
    let helper = write_dispatching_helper(
        dir.path(),
        "dispatch-helper.sh",
        &[
            ("rollout-parent.jsonl", &parent_projection),
            ("rollout-sub.jsonl", &sub_projection),
        ],
    );
    let harness = import(dir.path(), &sh_helper(&helper, &[]));

    // SpawnedBy: causal parent = the subagent thread's first raw occurrence;
    // target = the parent thread's exact raw `spawnAgent` occurrence.
    let spawned_by = harness
        .ops
        .ops
        .iter()
        .find(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.relationship == editchain_core::op::NoteRelationship::SpawnedBy)
        })
        .expect("SpawnedBy fact");
    let sub_stream = source_stream(dir.path(), &dir.path().join("rollout-sub.jsonl"), 0);
    let parent_stream = source_stream(dir.path(), &dir.path().join("rollout-parent.jsonl"), 0);
    assert_eq!(
        spawned_by.parents,
        ParentSet::One(sub_stream.op_from_position(SourcePosition::raw(1)).unwrap())
    );
    match &spawned_by.kind {
        OpKind::Note(note) => {
            assert_eq!(
                note.target_ids,
                vec![parent_stream
                    .op_from_position(SourcePosition::raw(2))
                    .unwrap()]
            );
            assert!(matches!(&note.content, Payload::Inline(bytes)
                if String::from_utf8_lossy(bytes).contains("collabToolCall.spawnAgent")));
        }
        _ => panic!("expected note op"),
    }
    // Relationship notes are session-scoped, never turn-scoped.
    assert_eq!(
        spawned_by.scope,
        ScopeRef::Session(derive_session_id("sub-1"))
    );

    // ReconnectsTo: causal parent = the raw occurrence whose agentsStates marks
    // the child completed; target = the child's last physical occurrence.
    let reconnects_to = harness
        .ops
        .ops
        .iter()
        .find(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.relationship == editchain_core::op::NoteRelationship::ReconnectsTo)
        })
        .expect("ReconnectsTo note");
    assert_eq!(
        reconnects_to.parents,
        ParentSet::One(
            parent_stream
                .op_from_position(SourcePosition::raw(3))
                .unwrap()
        )
    );
    match &reconnects_to.kind {
        OpKind::Note(note) => {
            assert_eq!(
                note.target_ids,
                vec![sub_stream.op_from_position(SourcePosition::raw(2)).unwrap()]
            );
        }
        _ => panic!("expected note op"),
    }
    assert_eq!(
        reconnects_to.scope,
        ScopeRef::Session(derive_session_id("parent-1")),
        "relationship notes are session-scoped to the owning thread"
    );

    // The copied forkedFromId is retained as an execution fact, but never
    // converted into guessed row geometry.
    assert!(
        !harness.ops.ops.iter().any(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.relationship == editchain_core::op::NoteRelationship::ForkOf)
        }),
        "forkedFromId must not manufacture ForkOf row geometry"
    );
    assert!(harness.ops.ops.iter().any(|o| {
        matches!(&o.kind, OpKind::Note(n) if n.relationship == editchain_core::op::NoteRelationship::ForkedFrom)
    }));
}

#[test]
fn fork_metadata_emits_exact_execution_fact_without_clock_boundary() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-trunk.jsonl",
        &[
            session_meta_line("trunk-1", "s"),
            event_line("A"),
            event_line("B").replace("12:00:01.000", "12:00:05.000"),
        ],
    );
    write_rollout(
        dir.path(),
        "rollout-branch.jsonl",
        &[
            session_meta_line("branch-1", "s").replace("12:00:00.000", "12:00:03.000"),
            event_line("C").replace("12:00:01.000", "12:00:04.000"),
        ],
    );
    let awk = r#"
{
  if (FILENAME ~ /rollout-trunk\.jsonl/) {
    if ($0 ~ /"type":"session_meta"/) {
      printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"sessionMeta\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{\"sessionId\":\"s\",\"threadId\":\"trunk-1\"}}}\n", NR
      next
    }
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-%d\",\"text\":\"trunk-%d\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR, NR, NR
    next
  }
  if (FILENAME ~ /rollout-branch\.jsonl/) {
    if ($0 ~ /"type":"session_meta"/) {
      printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"sessionMeta\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{\"sessionId\":\"s\",\"threadId\":\"branch-1\",\"forkedFromId\":\"trunk-1\"}}}\n", NR
      next
    }
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-%d\",\"text\":\"branch-%d\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR, NR, NR
    next
  }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR
}
"#;
    let harness = import(dir.path(), &helper_in(&dir, awk));

    let forked_from = harness
        .ops
        .ops
        .iter()
        .find(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.relationship == editchain_core::op::NoteRelationship::ForkedFrom)
        })
        .expect("ForkedFrom fact");
    let branch_stream = source_stream(dir.path(), &dir.path().join("rollout-branch.jsonl"), 0);
    // The child execution occurrence is exact; the target is a stable execution
    // entity, not whichever trunk row happened to precede the branch clock.
    assert_eq!(
        forked_from.parents,
        ParentSet::One(
            branch_stream
                .op_from_position(SourcePosition::raw(1))
                .unwrap()
        )
    );
    match &forked_from.kind {
        OpKind::Note(note) => {
            assert_eq!(note.target_ids.len(), 1);
            assert!(matches!(&note.content, Payload::Inline(bytes)
                if String::from_utf8_lossy(bytes).contains("codex-topology-v2")));
        }
        _ => panic!("expected note op"),
    }
    assert!(!harness.ops.ops.iter().any(|o| {
        matches!(&o.kind, OpKind::Note(n) if n.relationship == editchain_core::op::NoteRelationship::ForkOf)
    }));
}

#[test]
fn legacy_list_agents_completion_links_via_started_marker_agent_path() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-parent.jsonl",
        &[
            session_meta_line("parent-1", "s"),
            event_line("P_SPAWN"),
            event_line("P_LIST"),
        ],
    );
    write_rollout(
        dir.path(),
        "rollout-sub.jsonl",
        &[session_meta_line("sub-1", "s"), event_line("SUB_WORK")],
    );

    // Legacy pre-R2 evidence: the collaboration list_agents output is a
    // JSON-encoded string; only the agent whose agent_status carries
    // `completed` counts. The completed agent_name is mapped to the started
    // marker's agentPath in the same thread.
    let completed = serde_json::to_string(&serde_json::json!({
        "agents": [
            {"agent_name": "/root/sub", "agent_status": {"completed": "sub finished"}},
            {"agent_name": "/root/other", "agent_status": "running"},
        ]
    }))
    .unwrap();
    let parent_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({"sessionId": "s", "threadId": "parent-1"})),
        ),
        line_record(
            2,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "subAgentActivity",
                    "id": "spawn-1",
                    "activityKind": "started",
                    "agentThreadId": "sub-1",
                    "agentPath": "/root/sub",
                }
            })],
            None,
        ),
        line_record(
            3,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "toolCall",
                    "id": "call-list",
                    "tool": "list_agents",
                    "namespace": "collaboration",
                    "status": "completed",
                    "arguments": {},
                    "result": completed,
                }
            })],
            None,
        ),
    ]);
    let sub_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({
                "sessionId": "s",
                "threadId": "sub-1",
                "parentThreadId": "parent-1",
                "agentPath": "/root/sub",
            })),
        ),
        line_record(
            2,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {"kind": "agentMessage", "id": "item-2", "text": "sub work"},
            })],
            None,
        ),
    ]);
    let helper = write_dispatching_helper(
        dir.path(),
        "dispatch-helper.sh",
        &[
            ("rollout-parent.jsonl", &parent_projection),
            ("rollout-sub.jsonl", &sub_projection),
        ],
    );
    let harness = import(dir.path(), &sh_helper(&helper, &[]));
    let sub_stream = source_stream(dir.path(), &dir.path().join("rollout-sub.jsonl"), 0);

    let reconnects: Vec<_> = harness
        .ops
        .ops
        .iter()
        .filter(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.relationship == editchain_core::op::NoteRelationship::ReconnectsTo)
        })
        .collect();
    assert_eq!(reconnects.len(), 1, "only the completed agent reconnects");
    assert_eq!(
        reconnects[0].parents,
        ParentSet::One(
            source_stream(dir.path(), &dir.path().join("rollout-parent.jsonl"), 0)
                .op_from_position(SourcePosition::raw(3))
                .unwrap()
        ),
        "the physical list_agents result occurrence is the completion endpoint"
    );
    match &reconnects[0].kind {
        OpKind::Note(note) => {
            assert_eq!(
                note.target_ids,
                vec![sub_stream.op_from_position(SourcePosition::raw(2)).unwrap()]
            );
        }
        _ => panic!("expected note op"),
    }

    // The raw started occurrence is the exact SpawnedBy target.
    let spawned_by = harness
        .ops
        .ops
        .iter()
        .find(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.relationship == editchain_core::op::NoteRelationship::SpawnedBy)
        })
        .expect("SpawnedBy fact");
    match &spawned_by.kind {
        OpKind::Note(note) => assert_eq!(
            note.target_ids,
            vec![
                source_stream(dir.path(), &dir.path().join("rollout-parent.jsonl"), 0,)
                    .op_from_position(SourcePosition::raw(2))
                    .unwrap()
            ]
        ),
        _ => panic!("expected note op"),
    }
    assert_eq!(
        spawned_by.parents,
        ParentSet::One(sub_stream.op_from_position(SourcePosition::raw(1)).unwrap())
    );
}

#[test]
fn turn_identity_is_persisted_on_ops_with_a_turn_metadata_note() {
    let dir = tempfile::tempdir().unwrap();
    write_rollout(
        dir.path(),
        "rollout-1.jsonl",
        &[session_meta_line("thread-1", "s"), event_line("TURN")],
    );
    let projection = concat!(
        "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{\"threadId\":\"thread-1\"}}}\n",
        "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":2,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"userMessage\",\"id\":\"u-1\",\"text\":\"first\"}},{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"a-1\",\"text\":\"second\"}}],\"changedTurns\":[{\"turnId\":\"turn-1\",\"status\":\"completed\",\"startedAt\":100,\"completedAt\":200,\"durationMs\":100}],\"removedTurnIds\":[]}}\n"
    );
    let harness = import(dir.path(), &fixed_helper(&dir, projection.as_bytes()));

    // Both items persist their turn identity in the op envelope.
    let turn_scope = ScopeRef::Turn(derive_turn_id("thread-1:turn-1"));
    let messages: Vec<_> = harness
        .ops
        .ops
        .iter()
        .filter(|o| matches!(o.kind, OpKind::Message(_)))
        .collect();
    assert_eq!(messages.len(), 2);
    for op in &messages {
        assert_eq!(op.scope, turn_scope);
    }

    // The turn metadata note records the lifecycle summary explicitly.
    let turn_note = harness
        .ops
        .ops
        .iter()
        .find(|o| {
            matches!(&o.kind, OpKind::Note(n)
                if n.content == Payload::Inline(b"turn-1: completed (2 items)".to_vec()))
        })
        .expect("turn metadata note");
    assert_eq!(turn_note.scope, turn_scope);
    let path = dir.path().join("rollout-1.jsonl");
    let stream = source_stream(dir.path(), &path, 0);
    assert_eq!(
        turn_note.id,
        stream
            .op_from_position(SourcePosition::derived(2, 3))
            .unwrap(),
        "turn note takes the lane after the two same-line item lanes"
    );
}

/// Build a one-record projection carrying a `sessionMeta` with an optional cwd.
fn session_projection(thread: &str, cwd: Option<&str>) -> Vec<u8> {
    let mut meta = serde_json::json!({"sessionId": "s", "threadId": thread});
    if let Some(cwd) = cwd {
        meta["cwd"] = serde_json::json!(cwd);
    }
    projection_bytes(&[line_record(1, Vec::new(), Some(meta))])
}

/// Import a raw root with a custom workspace (fresh cursors).
fn import_workspace(root: &Path, workspace: &Path, helper: &HelperCommand) -> Harness {
    let mut cursors = MemoryCursorStore::new();
    import_workspace_into(
        root,
        workspace,
        helper,
        &ImportOptions::default(),
        &mut cursors,
    )
    .unwrap()
}

/// Import a raw root with a custom workspace into an existing cursor store.
fn import_workspace_into(
    root: &Path,
    workspace: &Path,
    helper: &HelperCommand,
    options: &ImportOptions,
    cursors: &mut MemoryCursorStore,
) -> Result<Harness, ImportError> {
    let mut ops_sink = MemoryOpSink::new();
    let mut blobs = ContentAddressedBlobSink::new();
    let request = CodexDiscoveryRequest {
        workspace_path: workspace.to_path_buf(),
        raw_root: root.to_path_buf(),
    };
    let report = import_codex(
        &request,
        options,
        helper,
        &mut ops_sink,
        &mut blobs,
        cursors,
    )?;
    Ok(Harness {
        report,
        ops: ops_sink,
        blobs,
    })
}

#[test]
fn workspace_filter_includes_equal_nested_and_missing_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let nested = workspace.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    // A prefix-sibling (`workspace-other`) must never match the workspace.
    let sibling = dir.path().join("workspace-other");
    std::fs::create_dir_all(&sibling).unwrap();

    write_rollout(
        dir.path(),
        "rollout-equal.jsonl",
        &[session_meta_line("equal-1", "s")],
    );
    write_rollout(
        dir.path(),
        "rollout-nested.jsonl",
        &[session_meta_line("nested-1", "s")],
    );
    write_rollout(
        dir.path(),
        "rollout-foreign.jsonl",
        &[session_meta_line("foreign-1", "s")],
    );
    write_rollout(
        dir.path(),
        "rollout-missing.jsonl",
        &[session_meta_line("missing-1", "s")],
    );
    write_rollout(
        dir.path(),
        "rollout-nocwd.jsonl",
        &[session_meta_line("nocwd-1", "s")],
    );
    write_rollout(
        dir.path(),
        "rollout-relative.jsonl",
        &[session_meta_line("relative-1", "s")],
    );

    let equal_cwd = workspace.to_string_lossy().into_owned();
    let nested_cwd = nested.to_string_lossy().into_owned();
    let sibling_cwd = sibling.to_string_lossy().into_owned();
    // Explicitly present and outside, but unresolvable on this machine (a
    // session recorded elsewhere): still lexically outside, so excluded.
    let remote_cwd = "/nonexistent/outside/workspace".to_string();
    let helper = write_dispatching_helper(
        dir.path(),
        "dispatch-helper.sh",
        &[
            (
                "rollout-equal.jsonl",
                &session_projection("equal-1", Some(equal_cwd.as_str())),
            ),
            (
                "rollout-nested.jsonl",
                &session_projection("nested-1", Some(nested_cwd.as_str())),
            ),
            (
                "rollout-foreign.jsonl",
                &session_projection("foreign-1", Some(sibling_cwd.as_str())),
            ),
            (
                "rollout-missing.jsonl",
                &session_projection("missing-1", Some(remote_cwd.as_str())),
            ),
            ("rollout-nocwd.jsonl", &session_projection("nocwd-1", None)),
            (
                "rollout-relative.jsonl",
                &session_projection("relative-1", Some("relative/dir")),
            ),
        ],
    );

    let helper_cmd = sh_helper(&helper, &[]);
    let harness = import_workspace(dir.path(), &workspace, &helper_cmd);

    // Equal, nested, missing-cwd, and relative (unclassifiable) rollouts are
    // imported; foreign and prefix-sibling cwds are excluded before any op or
    // cursor is written. Path-sorted discovery order is: equal, foreign
    // (excluded), missing (excluded), nested, nocwd, relative.
    assert_eq!(harness.report.files_discovered, 6);
    assert_eq!(harness.report.files_processed, 4);
    assert_eq!(harness.report.raw_ops, 4);
    assert_eq!(harness.report.normalized_ops, 0);
    assert_eq!(harness.report.malformed, 0);
    assert_eq!(harness.ops.ops.len(), 4);
    assert_eq!(
        raw_bytes(&harness.ops.ops[0], &harness.blobs),
        ln(&session_meta_line("equal-1", "s"))
    );
    assert_eq!(
        raw_bytes(&harness.ops.ops[1], &harness.blobs),
        ln(&session_meta_line("nested-1", "s"))
    );
    assert_eq!(
        raw_bytes(&harness.ops.ops[2], &harness.blobs),
        ln(&session_meta_line("nocwd-1", "s"))
    );
    assert_eq!(
        raw_bytes(&harness.ops.ops[3], &harness.blobs),
        ln(&session_meta_line("relative-1", "s"))
    );
}

#[test]
fn workspace_filter_is_idempotent_and_never_writes_foreign_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let foreign = dir.path().join("other");
    std::fs::create_dir_all(&foreign).unwrap();

    write_rollout(
        dir.path(),
        "rollout-own.jsonl",
        &[session_meta_line("own-1", "s")],
    );
    write_rollout(
        dir.path(),
        "rollout-foreign.jsonl",
        &[session_meta_line("foreign-1", "s")],
    );

    let own_cwd = workspace.to_string_lossy().into_owned();
    let foreign_cwd = foreign.to_string_lossy().into_owned();
    let helper = write_dispatching_helper(
        dir.path(),
        "dispatch-helper.sh",
        &[
            (
                "rollout-own.jsonl",
                &session_projection("own-1", Some(own_cwd.as_str())),
            ),
            (
                "rollout-foreign.jsonl",
                &session_projection("foreign-1", Some(foreign_cwd.as_str())),
            ),
        ],
    );

    let helper_cmd = sh_helper(&helper, &[]);
    let mut cursors = MemoryCursorStore::new();
    let first = import_workspace_into(
        dir.path(),
        &workspace,
        &helper_cmd,
        &ImportOptions::default(),
        &mut cursors,
    )
    .unwrap();
    assert_eq!(first.report.files_discovered, 2);
    assert_eq!(first.report.files_processed, 1);
    assert_eq!(first.report.raw_ops, 1);
    assert_eq!(
        raw_bytes(&first.ops.ops[0], &first.blobs),
        ln(&session_meta_line("own-1", "s"))
    );

    // Excluded rollouts never get cursors, so a later widened workspace still
    // imports them from scratch and reruns stay deterministic.
    let own_path = dir.path().join("rollout-own.jsonl");
    let own_key = source_key(dir.path(), &own_path);
    let foreign_path = dir.path().join("rollout-foreign.jsonl");
    let foreign_key = source_key(dir.path(), &foreign_path);
    assert!(cursors.get_cursor(&own_key).unwrap().is_some());
    assert!(
        cursors.get_cursor(&foreign_key).unwrap().is_none(),
        "foreign rollouts are filtered before any cursor is written"
    );

    // Rerun: the included file is skipped via its cursor and the foreign file
    // is re-filtered out; nothing new is emitted.
    let second = import_workspace_into(
        dir.path(),
        &workspace,
        &helper_cmd,
        &ImportOptions::default(),
        &mut cursors,
    )
    .unwrap();
    assert_eq!(second.report.files_processed, 0);
    assert_eq!(second.report.raw_ops, 0);
    assert!(second.ops.ops.is_empty());
}

#[test]
fn rewritten_rollout_reimports_at_new_generation_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-rewrite.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n{}\n",
            session_meta_line("thread-1", "s"),
            event_line("A"),
            event_line("B")
        ),
    )
    .unwrap();

    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let mut cursors = MemoryCursorStore::new();

    let first =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(first.report.raw_ops, 3);
    assert!(
        first.ops.ops.iter().all(|op| op.id.boot == 0),
        "original import uses generation 0"
    );

    // Truncate and rewrite the source from scratch with different content.
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            session_meta_line("thread-1", "s"),
            event_line("C")
        ),
    )
    .unwrap();

    let second =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(second.report.files_processed, 1);
    assert_eq!(second.report.raw_ops, 2);
    assert!(
        second.ops.ops.iter().all(|op| op.id.boot == 1),
        "rewritten file re-imports under a new deterministic boot generation"
    );
    // New generation op ids never collide with the old generation's ids.
    assert_ne!(second.ops.ops[0].id.boot, first.ops.ops[0].id.boot);

    // Deterministic ids: the new generation's op ids are a pure function of
    // the path, the persisted generation counter, and the file content, so a
    // crash between this run and its cursor commit replays the exact same ids
    // (covered end-to-end in `durable_storage.rs`).

    // The generation bump is persisted with the cursor.
    let key = source_key(dir.path(), &path);
    assert_eq!(cursors.get_generation(&key).unwrap(), 1);

    // Idempotent third run: the rewritten file is now unchanged and skipped.
    let third =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(third.report.files_processed, 0);
    assert!(third.ops.ops.is_empty());

    // Cursor is not corrupted: it matches the rewritten file's shape.
    let cursor = cursors.get_cursor(&key).unwrap().unwrap();
    assert_eq!(cursor.ops_emitted, 2);
    assert_eq!(cursor.file_size, std::fs::metadata(&path).unwrap().len());
}

#[test]
fn rewritten_rollout_does_not_block_unrelated_rollouts() {
    let dir = tempfile::tempdir().unwrap();
    let rewrite_path = dir.path().join("rollout-a.jsonl");
    let append_path = dir.path().join("rollout-b.jsonl");
    std::fs::write(
        &rewrite_path,
        format!(
            "{}\n{}\n",
            session_meta_line("thread-a", "s"),
            event_line("A")
        ),
    )
    .unwrap();
    std::fs::write(
        &append_path,
        format!(
            "{}\n{}\n",
            session_meta_line("thread-b", "s"),
            event_line("X")
        ),
    )
    .unwrap();

    let helper = helper_in(&dir, &messages_awk("thread-a"));
    let mut cursors = MemoryCursorStore::new();
    let first =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(first.report.files_processed, 2);

    // Rewrite rollout-a (truncated) and append a line to rollout-b.
    std::fs::write(
        &rewrite_path,
        format!("{}\n", session_meta_line("thread-a", "s")),
    )
    .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&append_path)
        .unwrap();
    writeln!(file, "{}", event_line("Y")).unwrap();
    drop(file);

    // One run processes both: the rewrite re-imports rollout-a at a new
    // generation and the append of rollout-b still goes through — neither
    // blocks the other.
    let second =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(second.report.files_processed, 2);
    let rewritten: Vec<_> = second
        .ops
        .ops
        .iter()
        .filter(|op| op.id.node.0 == source_stream(dir.path(), &rewrite_path, 1).node.0)
        .collect();
    assert!(!rewritten.is_empty());
    assert!(rewritten.iter().all(|op| op.id.boot == 1));
    let appended: Vec<_> = second
        .ops
        .ops
        .iter()
        .filter(|op| op.id.node.0 == source_stream(dir.path(), &append_path, 0).node.0)
        .collect();
    assert!(!appended.is_empty());
    assert!(appended.iter().all(|op| op.id.boot == 0));
}

#[test]
fn append_after_rewrite_continues_the_new_generation_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-append.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            session_meta_line("thread-1", "s"),
            event_line("A")
        ),
    )
    .unwrap();

    let helper = helper_in(&dir, &messages_awk("thread-1"));
    let mut cursors = MemoryCursorStore::new();
    let first =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(first.report.raw_ops, 2);

    // Rewrite to a single line, then append a second line (grow the file).
    std::fs::write(&path, format!("{}\n", session_meta_line("thread-1", "s"))).unwrap();
    let rewritten =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(rewritten.report.raw_ops, 1);
    assert_eq!(rewritten.ops.ops[0].id.boot, 1);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{}", event_line("B")).unwrap();
    drop(file);

    let appended =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(appended.report.files_processed, 1);
    assert_eq!(appended.report.raw_ops, 1);
    // The append continues the boot-1 stream: same node, same boot, next seq.
    let stream = source_stream(dir.path(), &path, 1);
    let expected_prev = stream.op_from_position(SourcePosition::raw(1)).unwrap();
    assert_eq!(appended.ops.ops[0].id.boot, 1);
    assert_eq!(appended.ops.ops[0].parents, ParentSet::One(expected_prev));

    // Idempotent afterwards.
    let third =
        import_with_options_into(dir.path(), &helper, &ImportOptions::default(), &mut cursors);
    assert_eq!(third.report.files_processed, 0);
    assert!(third.ops.ops.is_empty());
}
