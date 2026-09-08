//! Integration tests for the VS Code service workspace loading.

#![expect(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "Test helpers and assertions; panics and expects are acceptable in tests"
)]

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_codec as _;
use editchain_git as _;
use editchain_import as _;
use editchain_index as _;
use editchain_project as _;
use editchain_protocol as _;
use editchain_query as _;
use gix as _;
use serde as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, CommandOp, CommandStage, FileEdit, FileOp, FileStage, GitLink, GitLinkKind,
    GitOid, ImportOp, MessageOp, NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind, ParentSet,
    Payload, ReflectionOp, ScopeRef, SessionId, Tags, ToolOp, ToolStage,
};
use editchain_import::{derive_path_id, BlobSink as _};
use editchain_project::filter::ChainFilter;
use editchain_project::taxonomy::{ActivityKind, ChainState, Outcome, RecordRole, Visibility};
use editchain_project::HistoryProjection;
use editchain_protocol::{
    ActivityBundleKind, FileChangeDto, FileDiffDto, HistoryRow, HistoryWindow, Request,
    RequestBody, ResponseBody, SearchFiltersDto,
};
use editchain_vscode_service::{
    parse_git_oid, parse_repository_id, prepare_render_snapshot, resolve_git_commit,
    HistoryWindowOptions, Workspace,
};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn write_page(chain_dir: &Path, page: &editchain_codec::page::Page) {
    write_page_sequence(chain_dir, 0, page);
}

fn write_page_sequence(chain_dir: &Path, sequence: u32, page: &editchain_codec::page::Page) {
    std::fs::create_dir_all(chain_dir).expect("create chain dir");
    std::fs::write(
        chain_dir.join(format!("{sequence:06}.eclog")),
        editchain_codec::page::encode_page(page),
    )
    .expect("write segment");
}

#[test]
fn prepared_snapshot_matches_live_projection_supports_details_and_invalidates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let first = msg_op(41, 1, b"snapshot first");
    let second = msg_op(41, 2, b"snapshot second");
    let mut page = editchain_codec::page::Page::new(0);
    page.add_record(0, editchain_codec::frame::encode_op(&first).unwrap());
    page.add_record(0, editchain_codec::frame::encode_op(&second).unwrap());
    write_page(&chain_dir, &page);

    // The parity filter must equal the fixed viewer filter (hide_trace and
    // hide_undated) so the prepared snapshot actually serves the compared
    // windows.
    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    );
    let mut live = Workspace::open(tmp.path().to_str().unwrap(), ".editchain").unwrap();
    let expected = live.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &filter,
        include_layout: true,
    });
    let expected_details = live
        .node_details(Some(first.id.to_string()), None)
        .expect("live details");

    let report =
        prepare_render_snapshot(tmp.path(), Path::new(".editchain")).expect("prepare snapshot");
    assert!(!report.reused);
    assert_eq!(report.rows, expected.total);
    assert!(report.bytes > 0);
    let reused =
        prepare_render_snapshot(tmp.path(), Path::new(".editchain")).expect("reuse snapshot");
    assert!(reused.reused);
    assert_eq!(reused.path, report.path);

    let mut cached = Workspace::open(tmp.path().to_str().unwrap(), ".editchain").unwrap();
    let actual = cached.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &filter,
        include_layout: true,
    });
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(
        serde_json::to_value(
            cached
                .node_details(Some(first.id.to_string()), None)
                .expect("snapshot details")
        )
        .unwrap(),
        serde_json::to_value(expected_details).unwrap()
    );

    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_string_lossy().into_owned(),
                chain_dir: ".editchain".to_string(),
            }),
        })
        .expect("snapshot open");
    let ResponseBody::Ok(open_body) = open.body else {
        panic!("snapshot open failed");
    };
    assert_eq!(open_body["render_snapshot"], "hit");

    let third = msg_op(41, 3, b"snapshot invalidation");
    let mut appended = editchain_codec::page::Page::new(1);
    appended.add_record(0, editchain_codec::frame::encode_op(&third).unwrap());
    write_page_sequence(&chain_dir, 1, &appended);
    let stale_open = server
        .handle(&Request {
            id: 2,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_string_lossy().into_owned(),
                chain_dir: ".editchain".to_string(),
            }),
        })
        .expect("stale snapshot fallback");
    let ResponseBody::Ok(stale_body) = stale_open.body else {
        panic!("stale snapshot open failed");
    };
    assert_eq!(stale_body["render_snapshot"], "miss");
    assert_eq!(stale_body["chain_generation"], 3);
}

/// An empty filter that hides nothing (used to keep existing tests focused on
/// windowing rather than filtering).
fn no_filter() -> ChainFilter {
    ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        false,
        false,
    )
}

fn msg_op(node: u64, seq: u64, text: &[u8]) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_000 + seq),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// 2^53 + 1 — the first integer JavaScript's IEEE-754 doubles round.
const OVER_2_53: u64 = 9_007_199_254_740_993;

/// Create a temporary git repository with one commit and return its path.
fn make_git_repo(dir: &Path) -> std::path::PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).expect("create repo dir");
    run(&repo, &["init", "-q"]);
    std::fs::write(repo.join("file.txt"), b"hello\n").expect("write file");
    run(&repo, &["add", "file.txt"]);
    run(
        &repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "commit",
            "-q",
            "-m",
            "initial commit",
        ],
    );
    repo
}

#[test]
fn imported_agent_edit_rows_materialize_recorded_snippets_without_fabricating_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let raw_id = OpId::new(NodeId(71), 0, 1);
    let raw = Op {
        id: raw_id,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_001),
        scope: ScopeRef::Session(SessionId(71)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(br#"{"type":"assistant"}"#.to_vec()),
            raw_hash: None,
        }),
    };
    let edit = Op {
        id: OpId::new(NodeId(71), 0, 2),
        parents: ParentSet::One(raw_id),
        actor: ActorId(2),
        clock: Clock::UnixMs(1_700_000_001),
        scope: ScopeRef::Session(SessionId(71)),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(b"tool-1".to_vec()),
            tool_name: Payload::Inline(b"Edit".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Inline(
                br#"{"file_path":"src/lib.rs","old_string":"fn old() {}","new_string":"fn new() {}"}"#
                    .to_vec(),
            ),
        }),
    };
    let mut page = editchain_codec::page::Page::new(0);
    page.add_record(0, editchain_codec::frame::encode_op(&raw).unwrap());
    page.add_record(0, editchain_codec::frame::encode_op(&edit).unwrap());
    write_page(&chain_dir, &page);

    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_string_lossy().into_owned(),
                chain_dir: ".editchain".to_string(),
            }),
        })
        .expect("open imported chain");
    assert!(matches!(open.body, ResponseBody::Ok(_)));
    let response = server
        .handle(&Request {
            id: 2,
            body: RequestBody::GetWindow(editchain_protocol::GetWindowRequest {
                offset: 0,
                limit: 100,
                hide_submodules: true,
                filter: None,
                include_layout: true,
            }),
        })
        .expect("agent history window");
    let ResponseBody::Ok(value) = response.body else {
        panic!("expected history window, got {:?}", response.body);
    };
    let window: HistoryWindow = serde_json::from_value(value).expect("decode history window");
    let file_row = window
        .rows
        .iter()
        .find(|row| row.file_change.is_some())
        .expect("agent file row");
    assert!(file_row.is_subop);
    assert_eq!(file_row.kind, "file");
    let change = file_row.file_change.clone().expect("file identity");
    assert_eq!(change.path, "src/lib.rs");
    assert_eq!(change.op_id.as_deref(), Some(edit.id.to_string().as_str()));
    assert!(change.partial);

    let response = server
        .handle(&Request {
            id: 3,
            body: RequestBody::GetFileDiff(editchain_protocol::GetFileDiffRequest { change }),
        })
        .expect("materialize agent diff");
    let ResponseBody::Ok(value) = response.body else {
        panic!("expected agent diff, got {:?}", response.body);
    };
    let diff: FileDiffDto = serde_json::from_value(value).expect("decode agent diff");
    assert_eq!(diff.before, "fn old() {}");
    assert_eq!(diff.after, "fn new() {}");
    assert!(diff.partial);
    assert!(diff
        .note
        .as_deref()
        .is_some_and(|note| note.contains("full before/after")));
}

#[test]
fn agent_edit_uses_exact_session_git_baseline_when_available() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = make_git_repo(tmp.path());
    let chain_dir = repo.join(".editchain");
    let session = SessionId(73);
    let raw_id = OpId::new(NodeId(73), 0, 1);
    let edit_id = OpId::new(NodeId(73), 0, 2);
    let link_id = OpId::new(NodeId(73), 0, 3);
    let repository = editchain_git::repository_id_from_path(&repo.join(".git"));
    let commit_hex = git_stdout(&repo, &["rev-parse", "HEAD"]);
    let commit_oid = GitOid::from_hex(&commit_hex).expect("full commit oid");
    let raw = Op {
        id: raw_id,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_001),
        scope: ScopeRef::Session(session),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                serde_json::json!({
                    "type": "session_meta",
                    "payload": { "cwd": repo.to_string_lossy() }
                })
                .to_string()
                .into_bytes(),
            ),
            raw_hash: None,
        }),
    };
    let edit = Op {
        id: edit_id,
        parents: ParentSet::One(raw_id),
        actor: ActorId(2),
        clock: Clock::UnixMs(1_700_000_002),
        scope: ScopeRef::Session(session),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(b"tool-anchored".to_vec()),
            tool_name: Payload::Inline(b"Edit".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Inline(
                br#"{"file_path":"file.txt","old_string":"hello\n","new_string":"hello from the agent\n"}"#
                    .to_vec(),
            ),
        }),
    };
    let link = Op {
        id: link_id,
        parents: ParentSet::One(raw_id),
        actor: ActorId(1),
        clock: Clock::None,
        scope: ScopeRef::Session(session),
        tags: Tags::IMPORT | Tags::META,
        kind: OpKind::GitLink(GitLink {
            source: raw_id,
            target_repo: repository,
            target_oid: commit_oid,
            kind: GitLinkKind::BasedOn,
        }),
    };
    let mut page = editchain_codec::page::Page::new(0);
    for op in [&raw, &edit, &link] {
        page.add_record(0, editchain_codec::frame::encode_op(op).unwrap());
    }
    write_page(&chain_dir, &page);

    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: repo.to_string_lossy().into_owned(),
                chain_dir: ".editchain".to_string(),
            }),
        })
        .expect("open Git-anchored agent chain");
    assert!(matches!(open.body, ResponseBody::Ok(_)));
    let window = server
        .handle(&Request {
            id: 2,
            body: RequestBody::GetWindow(editchain_protocol::GetWindowRequest {
                offset: 0,
                limit: 100,
                hide_submodules: true,
                filter: None,
                include_layout: true,
            }),
        })
        .expect("anchored agent history window");
    let ResponseBody::Ok(value) = window.body else {
        panic!("expected history window, got {:?}", window.body);
    };
    let window: HistoryWindow = serde_json::from_value(value).expect("decode history window");
    let change = window
        .rows
        .iter()
        .filter_map(|row| row.file_change.clone())
        .find(|change| change.source == editchain_protocol::FileChangeSource::Agent)
        .expect("agent file row");
    assert_eq!(change.path, "file.txt");
    assert_eq!(
        change.repository.as_deref(),
        Some(repository.0.to_string().as_str())
    );
    assert_eq!(change.repository_path.as_deref(), Some("file.txt"));
    assert_eq!(change.commit_oid.as_deref(), Some(commit_hex.as_str()));

    let response = server
        .handle(&Request {
            id: 3,
            body: RequestBody::GetFileDiff(editchain_protocol::GetFileDiffRequest { change }),
        })
        .expect("materialize Git-anchored agent diff");
    let ResponseBody::Ok(value) = response.body else {
        panic!("expected anchored agent diff, got {:?}", response.body);
    };
    let diff: FileDiffDto = serde_json::from_value(value).expect("decode agent diff");
    assert_eq!(diff.before, "hello\n");
    assert_eq!(diff.after, "hello from the agent\n");
    assert!(diff.partial, "sequential agent state is still conservative");
    assert!(diff
        .note
        .as_deref()
        .is_some_and(|note| note.contains("session's exact Git baseline")));
}

#[test]
fn legacy_codex_multi_file_record_recovers_every_path_from_raw_evidence() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let raw_id = OpId::new(NodeId(72), 0, 1);
    let legacy_file_id = OpId::new(NodeId(72), 0, 2);
    let raw_json = serde_json::json!({
        "timestamp": "2026-08-26T12:00:00Z",
        "type": "event_msg",
        "payload": {
            "type": "item_completed",
            "turn_id": "turn-1",
            "item": {
                "type": "FileChange",
                "id": "file-1",
                "changes": {
                    "src/a.rs": {
                        "type": "update",
                        "unified_diff": "@@ -1 +1 @@\n-old a\n+new a"
                    },
                    "src/b.rs": {
                        "type": "delete",
                        "content": "old b\n"
                    }
                },
                "status": "completed"
            }
        }
    })
    .to_string();
    let raw = Op {
        id: raw_id,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_001),
        scope: ScopeRef::Session(SessionId(72)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(raw_json.into_bytes()),
            raw_hash: None,
        }),
    };
    // Version-four Codex normalization retained only the first path and joined
    // every path's hunks into this one FileOp. The service must suppress that
    // lossy child in favor of the still-exact raw record above.
    let legacy_file = Op {
        id: legacy_file_id,
        parents: ParentSet::One(raw_id),
        actor: ActorId(2),
        clock: Clock::UnixMs(1_700_000_001),
        scope: ScopeRef::Session(SessionId(72)),
        tags: Tags::AGENT | Tags::FILE,
        kind: OpKind::File(FileOp {
            path: derive_path_id("src/a.rs"),
            stage: FileStage::Applied,
            base: None,
            after: None,
            edit: FileEdit::UnifiedDiff(Payload::Inline(
                b"@@ -1 +1 @@\n-old a\n+new a\n@@ -1 +0,0 @@\n-old b".to_vec(),
            )),
        }),
    };
    let legacy_path = Op {
        id: OpId::new(NodeId(72), 0, 3),
        parents: ParentSet::One(raw_id),
        actor: ActorId(2),
        clock: Clock::UnixMs(1_700_000_001),
        scope: ScopeRef::Session(SessionId(72)),
        tags: Tags::AGENT | Tags::META,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![legacy_file_id],
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(b"src/a.rs".to_vec()),
        }),
    };
    let mut page = editchain_codec::page::Page::new(0);
    for op in [&raw, &legacy_file, &legacy_path] {
        page.add_record(0, editchain_codec::frame::encode_op(op).unwrap());
    }
    write_page(&chain_dir, &page);

    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_string_lossy().into_owned(),
                chain_dir: ".editchain".to_string(),
            }),
        })
        .expect("open legacy Codex chain");
    assert!(matches!(open.body, ResponseBody::Ok(_)));
    let response = server
        .handle(&Request {
            id: 2,
            body: RequestBody::GetWindow(editchain_protocol::GetWindowRequest {
                offset: 0,
                limit: 100,
                hide_submodules: true,
                filter: None,
                include_layout: true,
            }),
        })
        .expect("legacy Codex history window");
    let ResponseBody::Ok(value) = response.body else {
        panic!("expected history window, got {:?}", response.body);
    };
    let window: HistoryWindow = serde_json::from_value(value).expect("decode history window");
    let changes: Vec<FileChangeDto> = window
        .rows
        .iter()
        .filter_map(|row| row.file_change.clone())
        .collect();
    assert_eq!(changes.len(), 2, "raw evidence restores both changed paths");
    assert_eq!(changes[0].path, "src/a.rs");
    assert_eq!(
        changes[0].status,
        editchain_protocol::FileChangeStatus::Modified
    );
    assert!(changes[0].partial);
    assert_eq!(changes[1].path, "src/b.rs");
    assert_eq!(
        changes[1].status,
        editchain_protocol::FileChangeStatus::Deleted
    );
    assert!(
        !changes[1].partial,
        "Codex retained complete deleted content"
    );
    assert!(changes
        .iter()
        .all(|change| change.op_id.as_deref() == Some(raw_id.to_string().as_str())));

    let update = server
        .handle(&Request {
            id: 3,
            body: RequestBody::GetFileDiff(editchain_protocol::GetFileDiffRequest {
                change: changes[0].clone(),
            }),
        })
        .expect("materialize raw Codex update");
    let ResponseBody::Ok(value) = update.body else {
        panic!("expected update diff, got {:?}", update.body);
    };
    let update: FileDiffDto = serde_json::from_value(value).expect("decode update diff");
    assert_eq!(update.before, "old a");
    assert_eq!(update.after, "new a");
    assert!(update.partial);

    let deletion = server
        .handle(&Request {
            id: 4,
            body: RequestBody::GetFileDiff(editchain_protocol::GetFileDiffRequest {
                change: changes[1].clone(),
            }),
        })
        .expect("materialize raw Codex deletion");
    let ResponseBody::Ok(value) = deletion.body else {
        panic!("expected deletion diff, got {:?}", deletion.body);
    };
    let deletion: FileDiffDto = serde_json::from_value(value).expect("decode deletion diff");
    assert_eq!(deletion.before, "old b\n");
    assert!(deletion.after.is_empty());
    assert!(!deletion.partial);
}

#[test]
fn git_commit_rows_expand_to_files_and_materialize_exact_native_diff_sides() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = make_git_repo(tmp.path());
    std::fs::write(repo.join("file.txt"), b"hello from the second commit\n").expect("modify file");
    run(&repo, &["add", "file.txt"]);
    run(
        &repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "commit",
            "-q",
            "-m",
            "modify file",
        ],
    );
    let commit_oid = git_stdout(&repo, &["rev-parse", "HEAD"]);
    std::fs::create_dir_all(repo.join(".editchain")).expect("chain directory");

    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: repo.to_string_lossy().into_owned(),
                chain_dir: ".editchain".to_string(),
            }),
        })
        .expect("open repository");
    assert!(matches!(open.body, ResponseBody::Ok(_)));
    let window = server
        .handle(&Request {
            id: 2,
            body: RequestBody::GetWindow(editchain_protocol::GetWindowRequest {
                offset: 0,
                limit: 100,
                hide_submodules: true,
                filter: None,
                include_layout: true,
            }),
        })
        .expect("history window");
    let ResponseBody::Ok(value) = window.body else {
        panic!("expected history window, got {:?}", window.body);
    };
    let window: HistoryWindow = serde_json::from_value(value).expect("decode history window");
    let parent_index = window
        .rows
        .iter()
        .position(|row| row.git_oid.as_deref() == Some(commit_oid.as_str()) && !row.is_subop)
        .expect("new commit row");
    let parent = &window.rows[parent_index];
    assert_eq!(
        parent.sub_ops.len(),
        1,
        "commit advertises one changed path"
    );
    assert_eq!(parent.sub_ops[0].summary, "file.txt");
    let file_row = window
        .rows
        .get(parent_index + 1)
        .expect("expanded file row");
    assert!(file_row.is_subop);
    assert_eq!(file_row.kind, "file");
    assert_eq!(file_row.summary, "file.txt");
    let change = file_row.file_change.clone().expect("file-change identity");
    assert_eq!(change.path, "file.txt");
    assert_eq!(change.repository_path.as_deref(), Some("file.txt"));
    assert_eq!(change.commit_oid.as_deref(), Some(commit_oid.as_str()));
    assert!(!change.partial);

    let response = server
        .handle(&Request {
            id: 3,
            body: RequestBody::GetFileDiff(editchain_protocol::GetFileDiffRequest {
                change: change.clone(),
            }),
        })
        .expect("materialize file diff");
    let ResponseBody::Ok(value) = response.body else {
        panic!("expected exact file diff, got {:?}", response.body);
    };
    let diff: FileDiffDto = serde_json::from_value(value).expect("decode file diff");
    assert_eq!(diff.before, "hello\n");
    assert_eq!(diff.after, "hello from the second commit\n");
    assert!(!diff.binary);
    assert!(!diff.partial);
    assert!(diff.note.is_none());

    let mut tampered: FileChangeDto = change;
    tampered.path = "another-file.txt".to_string();
    let rejected = server
        .handle(&Request {
            id: 4,
            body: RequestBody::GetFileDiff(editchain_protocol::GetFileDiffRequest {
                change: tampered,
            }),
        })
        .expect("tampered request returns protocol error");
    assert!(matches!(rejected.body, ResponseBody::Error(_)));
}

fn run(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout)
        .expect("git stdout is UTF-8")
        .trim()
        .to_string()
}

#[test]
fn open_resolves_exact_session_base_outside_current_head_history() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = make_git_repo(tmp.path());
    let session_base_hex = git_stdout(&repo, &["rev-parse", "HEAD"]);

    run(&repo, &["checkout", "-q", "--orphan", "other"]);
    std::fs::write(repo.join("file.txt"), b"unrelated branch\n").expect("write branch file");
    run(&repo, &["add", "file.txt"]);
    run(
        &repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "commit",
            "-q",
            "-m",
            "unrelated head",
        ],
    );
    assert_ne!(session_base_hex, git_stdout(&repo, &["rev-parse", "HEAD"]));

    let repository = editchain_git::repository_id_from_path(&repo.join(".git"));
    let session_base = GitOid::from_hex(&session_base_hex).expect("full commit oid");
    let source = Op {
        id: OpId::new(NodeId(44), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1),
        scope: ScopeRef::Session(SessionId(44)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(b"session_meta".to_vec()),
            raw_hash: None,
        }),
    };
    let link_record = Op {
        id: OpId::new(NodeId(44), 0, 2),
        parents: ParentSet::One(source.id),
        actor: ActorId(1),
        clock: Clock::None,
        scope: source.scope,
        tags: Tags::IMPORT | Tags::META,
        kind: OpKind::GitLink(GitLink {
            source: source.id,
            target_repo: repository,
            target_oid: session_base,
            kind: GitLinkKind::BasedOn,
        }),
    };
    let mut page = editchain_codec::page::Page::new(0);
    page.add_record(0, editchain_codec::frame::encode_op(&source).unwrap());
    page.add_record(0, editchain_codec::frame::encode_op(&link_record).unwrap());
    write_page(&repo.join(".editchain"), &page);

    let workspace = Workspace::open(repo.to_str().unwrap(), ".editchain").unwrap();
    assert!(
        workspace
            .projection
            .git
            .commit(repository, &session_base)
            .is_some(),
        "the exact durable target must load even when HEAD cannot reach it"
    );
    let session_node = workspace
        .projection
        .nodes()
        .into_iter()
        .find(|node| node.node_key() == source.id.to_string())
        .expect("session start node");
    assert!(
        workspace
            .projection
            .lifted_parent_keys(&session_node)
            .contains(&session_base.to_hex()),
        "the exact BasedOn relation must branch the session from its start commit"
    );
}

#[test]
fn op_identifiers_above_2_53_round_trip_exactly_through_window_details_and_search() {
    let big_op = msg_op(OVER_2_53, 42, b"needle-exact-id");
    let projection = HistoryProjection::from_ops(vec![big_op.clone()]);
    let mut ws = Workspace::from_projection(projection);

    // History window: the op id must be the exact decimal string, never a
    // number that JavaScript could round.
    let filter = no_filter();
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 10,
        hide_submodules: false,
        filter: &filter,
        include_layout: true,
    });
    let row = window
        .rows
        .iter()
        .find(|r| r.op_id.is_some())
        .expect("op row");
    assert_eq!(row.op_id.as_deref(), Some(big_op.id.to_string().as_str()));
    assert_eq!(row.op_id.as_deref(), Some("9007199254740993:0:42"));

    // Node details resolve from the exact string and echo it back exactly.
    let details = ws
        .node_details(Some(big_op.id.to_string()), None)
        .expect("details");
    assert_eq!(details.op_id.as_deref(), Some("9007199254740993:0:42"));
    assert_eq!(
        details.parents,
        big_op
            .parents
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );

    // Search: the scored chunk's identifiers must serialize as exact strings.
    let state = editchain_vscode_service::build_lexical_index(&ws).unwrap();
    let filters = editchain_query::search::SearchFilters {
        kinds: None,
        sources: None,
        sessions: None,
        actors: None,
        paths: None,
        after: None,
        before: None,
        include_raw: false,
        include_private: false,
    };
    let results = state
        .index
        .search_internal("needle-exact-id", &filters, 5)
        .unwrap();
    let hit = results
        .iter()
        .find(|r| r.op_id == big_op.id)
        .expect("search hit for big op");
    let dto = editchain_vscode_service::search_hit_from_chunk(hit, &state.git_identities);
    assert_eq!(dto.op_id, big_op.id.to_string());
    assert_eq!(dto.op_id, "9007199254740993:0:42");
    assert_eq!(dto.chunk_id, format!("{}:0", big_op.id));
    assert_eq!(dto.actor_id, big_op.actor.0.to_string());
    assert!(
        dto.git_oid.is_none(),
        "EditChain hit must not carry git identity"
    );
    assert!(
        dto.repository.is_none(),
        "EditChain hit must not carry git identity"
    );

    // The full protocol path (Server::handle over a real chain dir) must
    // return the same exact strings inside an Ok envelope.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let mut page = editchain_codec::page::Page::new(0);
    page.add_record(0, editchain_codec::frame::encode_op(&big_op).unwrap());
    write_page(&chain_dir, &page);
    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_str().expect("utf8").to_string(),
                chain_dir: ".editchain".to_string(),
            }),
        })
        .expect("open");
    assert!(matches!(open.body, ResponseBody::Ok(_)));
    let search = server
        .handle(&Request {
            id: 2,
            body: RequestBody::Search(editchain_protocol::SearchRequest {
                query: "needle-exact-id".to_string(),
                mode: editchain_query::search::SearchMode::Lexical,
                top_k: 5,
                filters: SearchFiltersDto::default(),
            }),
        })
        .expect("search");
    let ResponseBody::Ok(value) = search.body else {
        panic!("expected Ok search response, got {:?}", search.body);
    };
    assert_eq!(value["results"][0]["op_id"], "9007199254740993:0:42");
    assert_eq!(value["results"][0]["chunk_id"], "9007199254740993:0:42:0");
}

#[test]
fn find_in_history_protocol_path_resolves_visible_rows_and_reports_truncation() {
    // Two message ops in one chain: both contain the needle. The Find-in-Chain
    // request carries the exact view DTO (raw: no hide predicates, submodules
    // hidden as the fixed viewer does) and must resolve both hits to real
    // visible top-level rows with absolute parent-row offsets — and never
    // claim an exact total when the top_k candidate cap truncates.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let first = msg_op(41, 1, b"needle-fi chain row one");
    let second = msg_op(41, 2, b"needle-fi chain row two");
    let mut page = editchain_codec::page::Page::new(0);
    page.add_record(0, editchain_codec::frame::encode_op(&first).unwrap());
    page.add_record(0, editchain_codec::frame::encode_op(&second).unwrap());
    write_page(&chain_dir, &page);

    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_str().expect("utf8").to_string(),
                chain_dir: ".editchain".to_string(),
            }),
        })
        .expect("open");
    assert!(matches!(open.body, ResponseBody::Ok(_)));

    let find = server
        .handle(&Request {
            id: 2,
            body: RequestBody::FindInHistory(editchain_protocol::FindInHistoryRequest {
                query: "needle-fi".to_string(),
                top_k: 50,
                filters: SearchFiltersDto::default(),
                filter: Some(editchain_protocol::ChainFilterDto {
                    summary_pattern: String::new(),
                    kind_pattern: String::new(),
                    include_kind_pattern: String::new(),
                    hide_undated: false,
                    hide_trace: false,
                    splice: true,
                }),
                hide_submodules: true,
            }),
        })
        .expect("find in history");
    let ResponseBody::Ok(value) = find.body else {
        panic!("expected Ok find-in-history response, got {:?}", find.body);
    };
    // Both messages are top-level rows; row 1 = second (newest), row 0 = first.
    assert_eq!(value["returned"], 2usize);
    assert_eq!(value["more"], false);
    let rows: Vec<u64> = value["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|m| m["row"].as_u64().expect("row offset"))
        .collect();
    assert_eq!(rows, vec![0, 1]);
    let keys: Vec<String> = value["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|m| m["node_key"].as_str().expect("node_key").to_string())
        .collect();
    assert_eq!(keys, vec![second.id.to_string(), first.id.to_string()]);
    assert!(
        value["matches"][0]["op_id"].is_string(),
        "op_id must be an exact string"
    );
    assert!(value["matches"][0]["summary"].is_string());

    // A top_k of 1 truncates the candidate list: the response must say `more`
    // rather than claim an exact total.
    let truncated = server
        .handle(&Request {
            id: 3,
            body: RequestBody::FindInHistory(editchain_protocol::FindInHistoryRequest {
                query: "needle-fi".to_string(),
                top_k: 1,
                filters: SearchFiltersDto::default(),
                filter: Some(editchain_protocol::ChainFilterDto {
                    summary_pattern: String::new(),
                    kind_pattern: String::new(),
                    include_kind_pattern: String::new(),
                    hide_undated: false,
                    hide_trace: false,
                    splice: true,
                }),
                hide_submodules: true,
            }),
        })
        .expect("find in history truncated");
    let ResponseBody::Ok(truncated_value) = truncated.body else {
        panic!(
            "expected Ok find-in-history response, got {:?}",
            truncated.body
        );
    };
    assert!(truncated_value["more"].as_bool().expect("more flag"));
    assert_eq!(truncated_value["returned"], 1usize);

    // The legacy Search request still works unchanged alongside it.
    let search = server
        .handle(&Request {
            id: 4,
            body: RequestBody::Search(editchain_protocol::SearchRequest {
                query: "needle-fi".to_string(),
                mode: editchain_query::search::SearchMode::Lexical,
                top_k: 5,
                filters: SearchFiltersDto::default(),
            }),
        })
        .expect("legacy search");
    let ResponseBody::Ok(search_value) = search.body else {
        panic!("expected Ok search response, got {:?}", search.body);
    };
    assert!(search_value["results"].as_array().expect("results").len() >= 2);
}

#[test]
fn git_resolve_uses_exact_string_ids_and_rejects_invalid_input() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _repo = make_git_repo(tmp.path());

    // Open the workspace: git rows carry hex oid + decimal repository strings.
    let mut ws = Workspace::open(tmp.path().to_str().expect("utf8"), "").expect("open");
    assert!(!ws.repositories.is_empty(), "repo should be discovered");
    let filter = no_filter();
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 10,
        hide_submodules: false,
        filter: &filter,
        include_layout: true,
    });
    let row = window
        .rows
        .iter()
        .find(|r| r.git_oid.is_some())
        .expect("git row");
    let oid_hex = row.git_oid.clone().expect("git_oid string");
    let repo_str = row.repository.clone().expect("repository string");
    assert_eq!(oid_hex.len(), 40, "SHA-1 hex is 40 chars");
    assert!(
        repo_str.parse::<u64>().is_ok(),
        "repository must be a decimal string, got {repo_str:?}"
    );

    // Resolve through the exact strings.
    let rid = parse_repository_id(&repo_str).expect("parse repository");
    let oid = parse_git_oid(&oid_hex).expect("parse oid");
    let resolved = resolve_git_commit(&ws, rid, &oid)
        .expect("resolve")
        .expect("commit found");
    assert_eq!(resolved.oid.to_hex(), oid_hex);
    assert_eq!(resolved.repository.0.to_string(), repo_str);

    // Node details for the git commit echo the same exact strings.
    let details = ws.node_details(None, Some(oid)).expect("git details");
    assert_eq!(details.git_oid.as_deref(), Some(oid_hex.as_str()));
    assert_eq!(details.repository.as_deref(), Some(repo_str.as_str()));

    // The protocol path must return Error for invalid IDs, never coerce them.
    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_str().expect("utf8").to_string(),
                chain_dir: String::new(),
            }),
        })
        .expect("open");
    assert!(matches!(open.body, ResponseBody::Ok(_)));
    for (repository, oid) in [
        ("not-a-number".to_string(), oid_hex.clone()),
        (repo_str.clone(), "zzzz".to_string()),
        (repo_str.clone(), "abc".to_string()),
    ] {
        let resp = server
            .handle(&Request {
                id: 2,
                body: RequestBody::ResolveObject(editchain_protocol::ResolveObjectRequest {
                    repository,
                    oid,
                }),
            })
            .expect("resolve");
        assert!(
            matches!(resp.body, ResponseBody::Error(_)),
            "invalid IDs must produce Error, got {:?}",
            resp.body
        );
    }
    // A valid OID that does not exist in the repo is an Error too.
    let missing = "deadbeef".repeat(5);
    let resp = server
        .handle(&Request {
            id: 2,
            body: RequestBody::ResolveObject(editchain_protocol::ResolveObjectRequest {
                repository: repo_str,
                oid: missing,
            }),
        })
        .expect("resolve");
    assert!(matches!(resp.body, ResponseBody::Error(_)));
}

#[test]
fn git_search_hits_carry_real_identity_and_resolve_object_round_trips() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _repo = make_git_repo(tmp.path());

    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_str().expect("utf8").to_string(),
                chain_dir: String::new(),
            }),
        })
        .expect("open");
    assert!(matches!(open.body, ResponseBody::Ok(_)));

    // Search must return the git commit as a real Git hit carrying the exact
    // lowercase oid + decimal repository identity — never a message-shaped hit.
    let search = server
        .handle(&Request {
            id: 2,
            body: RequestBody::Search(editchain_protocol::SearchRequest {
                query: "initial".to_string(),
                mode: editchain_query::search::SearchMode::Lexical,
                top_k: 5,
                filters: SearchFiltersDto::default(),
            }),
        })
        .expect("search");
    let ResponseBody::Ok(value) = search.body else {
        panic!("expected Ok search response, got {:?}", search.body);
    };
    let hits = value["results"].as_array().expect("results array");
    let git_hits: Vec<_> = hits.iter().filter(|h| h["source"] == "Git").collect();
    assert!(
        !git_hits.is_empty(),
        "git commit must be indexed and searchable"
    );
    let hit = git_hits[0];
    assert_eq!(hit["kind"], "git");
    assert_eq!(hit["is_submodule"], false);
    let git_oid = hit["git_oid"].as_str().expect("git_oid string");
    assert_eq!(git_oid.len(), 40, "SHA-1 hex is 40 chars");
    assert_eq!(
        git_oid,
        git_oid.to_ascii_lowercase(),
        "oid must be lowercase hex"
    );
    let repository = hit["repository"].as_str().expect("repository string");
    assert!(
        repository.parse::<u64>().is_ok(),
        "repository must be an exact decimal string, got {repository:?}"
    );
    assert!(hit["op_id"].is_string(), "synthetic op id stays a string");
    assert!(
        hit["git_oid"].is_string() && hit["repository"].is_string(),
        "git identity must serialize as exact strings"
    );

    // ResolveObject round-trip: the exact strings from the search hit resolve
    // the same commit through the service.
    let resolve = server
        .handle(&Request {
            id: 3,
            body: RequestBody::ResolveObject(editchain_protocol::ResolveObjectRequest {
                repository: repository.to_string(),
                oid: git_oid.to_string(),
            }),
        })
        .expect("resolve");
    assert!(
        matches!(resolve.body, ResponseBody::Ok(_)),
        "hit identity must resolve, got {:?}",
        resolve.body
    );
    let ResponseBody::Ok(resolved) = resolve.body else {
        panic!("expected Ok resolve response, got {:?}", resolve.body);
    };
    let rid = parse_repository_id(repository).expect("parse repository");
    let oid = parse_git_oid(git_oid).expect("parse oid");
    let ws = server.workspace.as_ref().expect("workspace open");
    let commit = resolve_git_commit(ws, rid, &oid)
        .expect("resolve helper")
        .expect("commit found");
    assert_eq!(commit.oid.to_hex(), git_oid);
    assert_eq!(commit.repository.0.to_string(), repository);
    // The ResolveObject Ok payload is the JSON-safe `ResolvedObject` DTO:
    // every identity is an exact string, never a raw numeric/structural ID
    // object (the pre-DTO wire form leaked repository u64, GitOid bytes
    // arrays, and OpId node/boot/seq numbers).
    assert_eq!(resolved["repository"], repository);
    assert_eq!(resolved["oid"], git_oid);
    assert!(
        resolved["repository"].is_string(),
        "repository must be a string"
    );
    assert!(resolved["oid"].is_string(), "oid must be a hex string");
    assert!(resolved["tree"].is_string(), "tree must be a hex string");
    assert!(
        resolved["parents"]
            .as_array()
            .is_some_and(|p| p.iter().all(serde_json::Value::is_string)),
        "parents must be lowercase hex strings"
    );
    assert!(
        resolved["imported_record"].is_null() || resolved["imported_record"].is_string(),
        "imported_record must be null or a display string"
    );
    assert!(
        resolved["changed_paths"]
            .as_array()
            .is_some_and(|paths| paths.iter().all(serde_json::Value::is_string)),
        "changed_paths must be decimal strings"
    );
    assert_eq!(resolved["object_format"], "Sha1");
    assert_eq!(resolved["availability"], "Resolved");
    assert!(
        resolved["oid"]["bytes"].is_null(),
        "oid must not leak the raw bytes array"
    );
    assert!(
        resolved["imported_record"]["node"].is_null(),
        "imported_record must not leak a structural OpId"
    );
}

#[test]
fn search_filters_accept_exact_string_ids_and_reject_invalid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut server = editchain_vscode_service::Server::new();
    let open = server
        .handle(&Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: tmp.path().to_str().expect("utf8").to_string(),
                chain_dir: String::new(),
            }),
        })
        .expect("open");
    assert!(matches!(open.body, ResponseBody::Ok(_)));

    // Exact decimal strings above 2^53 convert cleanly (no results for a
    // bogus session, but the request must succeed, not fail validation).
    let ok = server
        .handle(&Request {
            id: 2,
            body: RequestBody::Search(editchain_protocol::SearchRequest {
                query: "anything".to_string(),
                mode: editchain_query::search::SearchMode::Lexical,
                top_k: 5,
                filters: SearchFiltersDto {
                    sessions: Some(vec![OVER_2_53.to_string()]),
                    actors: Some(vec![OVER_2_53.to_string()]),
                    ..SearchFiltersDto::default()
                },
            }),
        })
        .expect("search");
    assert!(matches!(ok.body, ResponseBody::Ok(_)));

    // Non-numeric IDs must produce a service Error.
    for (sessions, actors) in [
        (Some(vec!["abc".to_string()]), None),
        (None, Some(vec!["1.5".to_string()])),
        (Some(vec![String::new()]), None),
    ] {
        let resp = server
            .handle(&Request {
                id: 2,
                body: RequestBody::Search(editchain_protocol::SearchRequest {
                    query: "anything".to_string(),
                    mode: editchain_query::search::SearchMode::Lexical,
                    top_k: 5,
                    filters: SearchFiltersDto {
                        sessions,
                        actors,
                        ..SearchFiltersDto::default()
                    },
                }),
            })
            .expect("search");
        assert!(
            matches!(resp.body, ResponseBody::Error(_)),
            "invalid search IDs must produce Error, got {:?}",
            resp.body
        );
    }
}

#[test]
fn workspace_open_with_empty_chain() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // No chain dir and no git repo — should open with empty projection.
    let ws = Workspace::open(tmp.path().to_str().expect("utf8"), "").expect("open");
    assert!(ws.projection.is_empty());
}

#[test]
fn history_window_returns_rows() {
    // Build a projection directly with two ops.
    let ops = vec![msg_op(1, 1, b"first"), msg_op(1, 2, b"second")];
    let projection = HistoryProjection::from_ops(ops);
    let mut ws = Workspace::from_projection(projection);
    let filter = no_filter();
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 10,
        hide_submodules: false,
        filter: &filter,
        include_layout: true,
    });
    assert_eq!(window.total, 2);
    assert_eq!(window.rows.len(), 2);
}

#[test]
fn op_rows_have_uniform_author_and_short_commit_id() {
    // A message op (MESSAGE tag only) should render a non-blank author label
    // ("system" fallback) and an abbreviated commit id (node:seq) rather than a
    // blank author and full node:boot:seq.
    let ops = vec![msg_op(7, 42, b"hello")];
    let projection = HistoryProjection::from_ops(ops);
    let mut ws = Workspace::from_projection(projection);
    let filter = no_filter();
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 10,
        hide_submodules: false,
        filter: &filter,
        include_layout: true,
    });
    let row = &window.rows[0];
    assert_eq!(row.author, "system");
    assert_eq!(row.commit_id, "7:42");
}

#[test]
fn system_flag_marks_tool_and_import_ops() {
    // A tool op should be flagged is_system; a message op should not.
    let tool = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Inline(b"{}".to_vec()),
        }),
    };
    let msg = msg_op(1, 2, b"hello");
    let projection = HistoryProjection::from_ops(vec![tool, msg]);
    let mut ws = Workspace::from_projection(projection);
    let filter = no_filter();
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 10,
        hide_submodules: false,
        filter: &filter,
        include_layout: true,
    });
    // Rows are newest-first; find by kind.
    let tool_row = window
        .rows
        .iter()
        .find(|r| r.kind == "tool")
        .expect("tool row");
    let msg_row = window
        .rows
        .iter()
        .find(|r| r.kind == "message")
        .expect("message row");
    assert!(tool_row.is_system);
    assert!(!msg_row.is_system);
}

/// A chain whose layout is sensitive to hash-iteration order: a merge of two
/// rootless ops plus two disconnected chains that overlap in time, mirroring
/// the cross-process repro shape (op history + git merges).
fn sensitive_chain_ops() -> Vec<Op> {
    let a = msg_op(1, 1, b"root A");
    let b = msg_op(2, 1, b"root B");
    let m = Op {
        id: OpId::new(NodeId(3), 0, 1),
        parents: ParentSet::Two(a.id, b.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_003),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"merge of two roots".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    let e1 = msg_op(4, 1, b"e1");
    let e2 = Op {
        id: OpId::new(NodeId(4), 0, 2),
        parents: ParentSet::One(e1.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_005),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"e2".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    let f1 = msg_op(5, 1, b"f1");
    let f2 = Op {
        id: OpId::new(NodeId(5), 0, 2),
        parents: ParentSet::One(f1.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_007),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"f2".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    vec![a, b, m, e1, e2, f1, f2]
}

fn write_frame(writer: &mut impl Write, value: &serde_json::Value) {
    let payload = serde_json::to_vec(value).expect("serialize request frame");
    let len = u32::try_from(payload.len()).expect("frame length fits u32");
    writer
        .write_all(&len.to_le_bytes())
        .expect("write frame length");
    writer.write_all(&payload).expect("write frame payload");
    let _: Option<()> = writer.flush().ok();
}

fn read_frame(reader: &mut impl Read) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).expect("read frame length");
    let len = usize::try_from(u32::from_le_bytes(len_buf)).expect("frame length fits usize");
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).expect("read frame payload");
    payload
}

/// Regression: full `GetWindow` JSON (rows + lanes + above/below/transitions +
/// `max_lane`) must be byte-identical across independent service processes on
/// the same chain. The lane-reuse algorithm previously iterated `HashMaps`
/// whose `RandomState` seeds differ per process, so two processes could assign
/// the same row different lanes — exactly the reported production bug.
#[test]
fn get_window_geometry_identical_across_independent_processes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join("chain");
    let mut page = editchain_codec::page::Page::new(0);
    for op in sensitive_chain_ops() {
        page.add_record(
            0,
            editchain_codec::frame::encode_op(&op).expect("encode op"),
        );
    }
    write_page(&chain_dir, &page);

    let exe = env!("CARGO_BIN_EXE_editchain-vscode-service");
    let workspace = tmp.path().to_str().expect("utf8 workspace");
    let chain = chain_dir.to_str().expect("utf8 chain");
    let filter = serde_json::json!({
        "summary_pattern": "",
        "kind_pattern": "",
        "include_kind_pattern": "",
        "hide_undated": true,
        "splice": true,
    });
    let mut windows: Vec<serde_json::Value> = Vec::new();
    for _ in 0..6 {
        let mut child = Command::new(exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn service process");
        let stdin = child.stdin.as_mut().expect("child stdin");
        let stdout = child.stdout.as_mut().expect("child stdout");
        write_frame(
            stdin,
            &serde_json::json!({"id": 1, "body": {"Open": {"workspace_path": workspace, "chain_dir": chain}}}),
        );
        let open = read_frame(stdout);
        let open_json: serde_json::Value =
            serde_json::from_slice(&open).expect("parse open response");
        assert!(
            open_json["body"]["Ok"]["nodes"].as_u64().unwrap_or(0) >= 7,
            "chain must project at least the seven test ops"
        );
        write_frame(
            stdin,
            &serde_json::json!({
                "id": 2,
                "body": {"GetWindow": {
                    "offset": 0,
                    "limit": 500,
                    "hide_submodules": false,
                    "filter": filter,
                }}
            }),
        );
        let window = read_frame(stdout);
        let window_json: serde_json::Value =
            serde_json::from_slice(&window).expect("parse window response");
        assert!(
            window_json["body"]["Ok"]["rows"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "window must contain rows"
        );
        // `serde_json::Value` equality is order-insensitive for object keys but
        // order-sensitive for arrays, so comparing the full Ok value compares
        // rows in order and their lane/above/below/transitions exactly.
        windows.push(window_json["body"]["Ok"].clone());
        let _: Option<()> = child.kill().ok();
        drop(child.wait());
    }
    let first = windows.first().expect("at least one process window");
    for (i, window) in windows.iter().enumerate().skip(1) {
        assert_eq!(
            first, window,
            "process {i} returned different GetWindow lane geometry"
        );
    }
}

/// Build a raw import op carrying one raw JSONL line (Codex-style envelope).
fn raw_import_op(node: u64, seq: u64, clock_ms: u64, parent: Option<OpId>, raw: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(raw.as_bytes().to_vec()),
            raw_hash: None,
        }),
    }
}

/// A normalized message child anchored at a raw import op.
fn raw_message_child(node: u64, seq: u64, parent: OpId, clock_ms: u64, text: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.as_bytes().to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// Build one linear turn-scoped chain: user message root, then the given rows
/// (each parented to the previous import), with turn-scoped normalized children
/// so the projection classifies tool rows as Execute and message rows as
/// Conversation inside `turn`.
fn turn_chain_ops(rows: &[(&str, Option<&str>)], turn: u64) -> Vec<Op> {
    let mut ops = Vec::new();
    let root = raw_import_op(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{}}"#,
    );
    ops.push(root.clone());
    ops.push(turn_message_child(101, 2, root.id, turn, "user request"));
    let mut previous = root;
    for (index, (kind, status)) in rows.iter().enumerate() {
        let node = u64::try_from(index + 2).unwrap_or(u64::MAX);
        let seq = u64::try_from(index + 2).unwrap_or(u64::MAX);
        let raw = match status {
            Some(status) => serde_json::json!({
                "type": "response_item",
                "payload": { "item": { "status": status } }
            })
            .to_string(),
            None => r#"{"type":"response_item","payload":{}}"#.to_string(),
        };
        let import = raw_import_op(node, seq, seq * 1_000, Some(previous.id), &raw);
        ops.push(import.clone());
        let child = match *kind {
            "tool" => turn_tool_child(node, seq + 100, import.id, turn),
            "message" => turn_message_child(node, seq + 100, import.id, turn, "agent text"),
            other => panic!("unknown kind {other}"),
        };
        ops.push(child);
        previous = import;
    }
    ops
}

/// A turn-scoped normalized message child of `parent`.
fn turn_message_child(node: u64, seq: u64, parent: OpId, turn: u64, text: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq * 1_000),
        scope: ScopeRef::Turn(editchain_core::TurnId(turn)),
        tags: Tags::HUMAN | Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.as_bytes().to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// A turn-scoped normalized tool child of `parent`.
fn turn_tool_child(node: u64, seq: u64, parent: OpId, turn: u64) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq * 1_000),
        scope: ScopeRef::Turn(editchain_core::TurnId(turn)),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Empty,
        }),
    }
}

/// A normalized Plan reflection child anchored at a raw import op.
fn turn_plan_child(node: u64, seq: u64, parent: OpId, turn: u64, summary: &str) -> Op {
    let scope = ScopeRef::Turn(editchain_core::TurnId(turn));
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq * 1_000),
        scope,
        tags: Tags::AGENT | Tags::REFLECTION,
        kind: OpKind::Reflection(ReflectionOp {
            scope,
            covers: editchain_core::FrontierSet::new(),
            window: editchain_core::WindowRef {
                start_seq: 0,
                end_seq: 0,
            },
            summary: Payload::Inline(summary.as_bytes().to_vec()),
            anchors: Payload::Empty,
        }),
    }
}

/// One linear session with three distinct source reasoning records whose
/// projected first heading is identical, followed by an agent continuation.
fn repeated_plan_chain_ops() -> Vec<Op> {
    let mut ops = Vec::new();
    let root = raw_import_op(
        30,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{}}"#,
    );
    ops.push(root.clone());
    ops.push(turn_message_child(130, 101, root.id, 7, "user request"));
    let mut previous = root;
    for (index, summary) in [
        "**Planning build and dry-run import steps**",
        "Planning   build and dry-run import steps",
        "__Planning build and dry-run import steps__",
    ]
    .iter()
    .enumerate()
    {
        let seq = u64::try_from(index + 2).unwrap_or(u64::MAX);
        let raw_json = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": summary }]
            }
        })
        .to_string();
        let raw = raw_import_op(30, seq, seq * 1_000, Some(previous.id), &raw_json);
        ops.push(raw.clone());
        ops.push(turn_plan_child(130, seq + 101, raw.id, 7, summary));
        previous = raw;
    }
    let continuation = raw_import_op(
        30,
        5,
        5_000,
        Some(previous.id),
        r#"{"type":"response_item","payload":{}}"#,
    );
    ops.push(continuation.clone());
    ops.push(turn_message_child(
        130,
        105,
        continuation.id,
        7,
        "agent continuation",
    ));
    ops
}

/// Store a blob in a chain's durable blob store, returning its reference.
fn store_blob(chain_dir: &Path, data: &[u8]) -> editchain_core::payload::BlobRef {
    let mut blobs = editchain_import::FsBlobSink::new(chain_dir.join("blobs")).expect("blob sink");
    blobs.put(data).expect("store blob")
}

#[test]
fn trace_rows_hidden_by_fixed_filter_kept_in_raw_mode_and_taxonomy_flows() {
    // import_a (message) -> trace envelope -> import_c (message). The fixed
    // viewer filter hides the trace envelope; raw mode keeps every row and
    // carries the provider-neutral taxonomy on each HistoryRow.
    let a = raw_import_op(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"alpha"}]}}"#,
    );
    let trace = raw_import_op(
        2,
        1,
        2_000,
        Some(a.id),
        r#"{"type":"response_item","payload":{}}"#,
    );
    let c = raw_import_op(
        3,
        1,
        3_000,
        Some(trace.id),
        r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"gamma"}]}}"#,
    );
    let ma = raw_message_child(4, 1, a.id, 1_000, "alpha");
    let mc = raw_message_child(5, 1, c.id, 3_000, "gamma");
    let projection = HistoryProjection::from_ops(vec![a, trace.clone(), c, ma, mc]);
    let mut ws = Workspace::from_projection(projection);

    let fixed = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    );
    let raw = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let fixed_window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &fixed,
        include_layout: true,
    });
    let raw_window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: true,
    });

    let trace_key = trace.id.to_string();
    assert!(
        !fixed_window.rows.iter().any(|r| r.node_key == trace_key),
        "fixed view hides the trace envelope"
    );
    assert_eq!(fixed_window.rows.len(), 2);
    assert!(
        raw_window.rows.iter().any(|r| r.node_key == trace_key),
        "raw view keeps the trace envelope"
    );
    assert_eq!(raw_window.rows.len(), 3);
    let trace_row = raw_window
        .rows
        .iter()
        .find(|r| r.node_key == trace_key)
        .expect("trace row");
    assert_eq!(trace_row.visibility, Visibility::Trace);
    assert_eq!(trace_row.record_role, RecordRole::Lifecycle);
    assert_eq!(trace_row.activity_kind, ActivityKind::System);
    let message_rows: Vec<&HistoryRow> = raw_window
        .rows
        .iter()
        .filter(|r| r.visibility == Visibility::Primary)
        .collect();
    assert_eq!(message_rows.len(), 2);
    assert!(
        message_rows
            .iter()
            .all(|r| r.record_role == RecordRole::Narrative
                && r.activity_kind == ActivityKind::Conversation),
        "content rows carry narrative/conversation taxonomy"
    );
}

#[test]
fn service_path_compaction_preserves_echo_and_outcome_metadata() {
    // The real service path compacts every raw import to bounded previews
    // (inline and blob-backed alike) before the projection derives taxonomy
    // and outcomes. This proves end-to-end that the compacted semantic subset
    // survives: external-tool echo markers are still classified
    // Trace/Echo/External even with normalized Message children, structured
    // tool outcomes keep their evidence, and generic agent prose stays
    // primary.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");

    // event_msg agent_message echo carrying the external tool-call marker.
    let echo_call = raw_import_op(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_call] {\"tool\":\"Bash\",\"command\":\"ls\"}"}}"#,
    );
    let echo_call_msg =
        raw_message_child(2, 1, echo_call.id, 1_000, "[external_agent_tool_call] run");

    // response_item assistant message echo carrying the tool-result marker.
    let echo_result = raw_import_op(
        3,
        1,
        2_000,
        Some(echo_call.id),
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"[external_agent_tool_result] completed"}]}}"#,
    );
    let echo_result_msg = raw_message_child(
        4,
        1,
        echo_result.id,
        2_000,
        "[external_agent_tool_result] completed",
    );

    // Generic agent prose (no marker) with a Message child stays primary.
    let prose = raw_import_op(
        5,
        1,
        3_000,
        Some(echo_result.id),
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"I will audit the tree"}}"#,
    );
    let prose_msg = raw_message_child(6, 1, prose.id, 3_000, "I will audit the tree");

    // A command tool row whose item_completed marker carries exitCode=0.
    let success_item = raw_import_op(
        7,
        1,
        4_000,
        Some(prose.id),
        r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"call_ok","exitCode":0,"status":"completed"}}}"#,
    );
    let success_tool = Op {
        id: OpId::new(NodeId(8), 0, 1),
        parents: ParentSet::One(success_item.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(4_000),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(b"call_ok".to_vec()),
            tool_name: Payload::Empty,
            stage: ToolStage::Finish,
            content: Payload::Inline(b"done".to_vec()),
        }),
    };
    let mut page = editchain_codec::page::Page::new(0);
    for op in [
        &echo_call,
        &echo_call_msg,
        &echo_result,
        &echo_result_msg,
        &prose,
        &prose_msg,
        &success_item,
        &success_tool,
    ] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut ws = Workspace::open(tmp.path().to_str().unwrap(), ".editchain")
        .expect("open workspace through the real service path");
    let fixed = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    );
    let raw = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let fixed_window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &fixed,
        include_layout: false,
    });
    let raw_window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: false,
    });

    let echo_keys = [echo_call.id.to_string(), echo_result.id.to_string()];
    for key in &echo_keys {
        let row = raw_window
            .rows
            .iter()
            .find(|r| &r.node_key == key)
            .unwrap_or_else(|| panic!("echo row {key} missing from raw window"));
        assert_eq!(row.visibility, Visibility::Trace);
        assert_eq!(row.record_role, RecordRole::Echo);
        assert_eq!(row.activity_kind, ActivityKind::External);
        assert!(
            !fixed_window.rows.iter().any(|r| &r.node_key == key),
            "fixed Activity view hides echoed external tool row {key}"
        );
    }

    let prose_row = raw_window
        .rows
        .iter()
        .find(|r| r.node_key == prose.id.to_string())
        .expect("prose row");
    assert_eq!(prose_row.visibility, Visibility::Primary);
    assert_eq!(prose_row.record_role, RecordRole::Narrative);
    assert_eq!(prose_row.activity_kind, ActivityKind::Conversation);
    assert!(
        fixed_window
            .rows
            .iter()
            .any(|r| r.node_key == prose.id.to_string()),
        "generic agent prose stays visible in both views"
    );

    let success_row = raw_window
        .rows
        .iter()
        .find(|r| r.node_key == success_item.id.to_string())
        .expect("tool row");
    assert_eq!(
        success_row.outcome,
        Outcome::Success,
        "structured outcome evidence survives compaction"
    );
    assert!(
        fixed_window
            .rows
            .iter()
            .any(|r| r.node_key == success_item.id.to_string()),
        "tool row stays primary"
    );
}

#[test]
fn service_path_compaction_preserves_childless_output_rows_and_blob_echoes() {
    // A childless function_call_output row: its bounded output preview must
    // survive compaction so the row is not misread as an empty envelope
    // (trace), and a huge blob-backed echo must keep its external marker
    // through the truncated-prefix fallback.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");

    // Childless response_item function_call_output: its output text keeps the
    // row Primary/Result through compaction.
    let output = raw_import_op(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"stdout line"}}"#,
    );

    // Large blob-backed event_msg echo: the durable payload is huge, so the
    // bounded preview truncates the JSON and the prefix fallback must recover
    // the external-tool marker. A normalized Message child is present too.
    let blob_raw = format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"[external_agent_tool_result] {}"}}}}"#,
        "x".repeat(200_000),
    );
    let blob_ref = store_blob(&chain_dir, blob_raw.as_bytes());
    let blob_echo = Op {
        id: OpId::new(NodeId(2), 0, 1),
        parents: ParentSet::One(output.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Blob(blob_ref),
            raw_hash: None,
        }),
    };
    let blob_echo_msg = raw_message_child(
        3,
        1,
        blob_echo.id,
        2_000,
        "[external_agent_tool_result] blob",
    );

    let mut page = editchain_codec::page::Page::new(0);
    for op in [&output, &blob_echo, &blob_echo_msg] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut ws = Workspace::open(tmp.path().to_str().unwrap(), ".editchain")
        .expect("open workspace through the real service path");
    let raw = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: false,
    });

    let output_row = window
        .rows
        .iter()
        .find(|r| r.node_key == output.id.to_string())
        .expect("output row");
    assert_eq!(output_row.visibility, Visibility::Primary);
    assert_eq!(output_row.record_role, RecordRole::Result);
    assert_eq!(output_row.activity_kind, ActivityKind::Execute);

    let echo_row = window
        .rows
        .iter()
        .find(|r| r.node_key == blob_echo.id.to_string())
        .expect("blob echo row");
    assert_eq!(echo_row.visibility, Visibility::Trace);
    assert_eq!(echo_row.record_role, RecordRole::Echo);
    assert_eq!(echo_row.activity_kind, ActivityKind::External);
}

#[test]
fn service_path_uses_command_stdout_as_the_output_subtitle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let command_raw = raw_import_op(
        10,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"cmd-1","status":"completed","stdout":"actual stdout\nsecond line","formatted_output":"formatted fallback"}}}"#,
    );
    let command = Op {
        id: OpId::new(NodeId(10), 0, 2),
        parents: ParentSet::One(command_raw.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(1_000),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::AGENT | Tags::COMMAND,
        kind: OpKind::Command(CommandOp {
            command_id: Payload::Inline(b"cmd-1".to_vec()),
            content: Payload::Inline(
                b"/bin/bash -lc 'printf actual'\nnormalized aggregate".to_vec(),
            ),
            stage: CommandStage::Finish,
        }),
    };
    let empty_output_import = raw_import_op(
        11,
        1,
        1_100,
        Some(command_raw.id),
        r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"cmd-2","status":"completed","stdout":"","formatted_output":""}}}"#,
    );
    let empty_output_command = Op {
        id: OpId::new(NodeId(11), 0, 2),
        parents: ParentSet::One(empty_output_import.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(1_100),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::AGENT | Tags::COMMAND,
        kind: OpKind::Command(CommandOp {
            command_id: Payload::Inline(b"cmd-2".to_vec()),
            content: Payload::Inline(b"/bin/bash -lc true".to_vec()),
            stage: CommandStage::Finish,
        }),
    };

    let mut page = editchain_codec::page::Page::new(0);
    for op in [
        &command_raw,
        &command,
        &empty_output_import,
        &empty_output_command,
    ] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut workspace = Workspace::open(tmp.path().to_str().unwrap(), ".editchain")
        .expect("open workspace through the real service path");
    let window = workspace.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &no_filter(),
        include_layout: false,
    });
    let row = window
        .rows
        .iter()
        .find(|row| row.node_key == command_raw.id.to_string())
        .expect("command output row");

    assert_eq!(row.kind, "command");
    assert_eq!(row.record_role, RecordRole::Result);
    assert_eq!(row.summary, "actual stdout\nsecond line");
    assert!(!row.summary.starts_with("$ "));
    assert!(!row.summary.contains("/bin/bash"));

    let empty_output_row = window
        .rows
        .iter()
        .find(|row| row.node_key == empty_output_import.id.to_string())
        .expect("silent command output row");
    assert_eq!(empty_output_row.summary, "No output");
    assert!(!empty_output_row.summary.contains("/bin/bash"));
}

#[test]
fn service_path_compaction_preserves_object_tool_payload_carriers() {
    // Childless tool-like response_item envelopes carrying non-empty object
    // arguments/parameters survive compaction with a bounded content signal:
    // they stay Primary/Action instead of collapsing to empty transport
    // (trace), while an id-only envelope stays trace.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");

    let call = raw_import_op(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"function_call","name":"WebSearch","arguments":{"query":"editchain docs"}}}"#,
    );
    let with_params = raw_import_op(
        2,
        1,
        2_000,
        Some(call.id),
        r#"{"type":"response_item","payload":{"type":"function_call","name":"Bash","parameters":{"command":"ls"}}}"#,
    );
    let id_only = raw_import_op(
        3,
        1,
        3_000,
        Some(with_params.id),
        r#"{"type":"response_item","payload":{"type":"function_call","id":"call_0"}}"#,
    );

    let mut page = editchain_codec::page::Page::new(0);
    for op in [&call, &with_params, &id_only] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut ws = Workspace::open(tmp.path().to_str().unwrap(), ".editchain")
        .expect("open workspace through the real service path");
    let raw = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: false,
    });

    for key in [call.id.to_string(), with_params.id.to_string()] {
        let row = window
            .rows
            .iter()
            .find(|r| r.node_key == key)
            .unwrap_or_else(|| panic!("tool-like row {key} missing from raw window"));
        assert_eq!(row.visibility, Visibility::Primary);
        assert_eq!(row.record_role, RecordRole::Action);
        assert_eq!(row.activity_kind, ActivityKind::Execute);
    }

    let id_only_row = window
        .rows
        .iter()
        .find(|r| r.node_key == id_only.id.to_string())
        .expect("id-only row");
    assert_eq!(id_only_row.visibility, Visibility::Trace);
}

#[test]
fn service_path_keeps_exec_command_separate_from_token_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let script = "const result = await tools.exec_command({\n  cmd: \"cargo test -p editchain-project\",\n  workdir: \"/workspace\"\n});\ntext(result.output);";
    let call_raw = serde_json::json!({
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call",
            "name": "exec",
            "input": script,
        },
    })
    .to_string();
    let call = raw_import_op(20, 1, 1_000, None, &call_raw);
    let call_child = Op {
        id: OpId::new(NodeId(20), 0, 2),
        parents: ParentSet::One(call.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(1_000),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(b"call-1".to_vec()),
            tool_name: Payload::Inline(b"exec".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Empty,
        }),
    };
    let accounting_json = serde_json::json!({
        "type": "token_usage_record",
        "payload": {
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "session_id": "thread-1",
            "root_turn_id": "turn-1",
            "response_id": "response-1",
            "usage": { "total_tokens": 114_757 },
        },
    })
    .to_string();
    let mut token = raw_import_op(20, 3, 1_100, Some(call.id), &accounting_json);
    token.tags |= Tags::META;
    let mut page = editchain_codec::page::Page::new(0);
    for op in [&call, &call_child, &token] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut workspace = Workspace::open(tmp.path().to_str().unwrap(), ".editchain")
        .expect("open workspace through the real service path");
    let window = workspace.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &no_filter(),
        include_layout: false,
    });
    let row = window
        .rows
        .iter()
        .find(|row| row.node_key == call.id.to_string())
        .expect("exec row");

    assert_eq!(row.summary, "tool: exec cargo test -p editchain-project");
    assert_eq!(row.sub_ops.len(), 1);
    let accounting_child = row.sub_ops.first().expect("token child");
    assert_eq!(accounting_child.kind, "token_usage_record");
    assert_eq!(accounting_child.summary, "114,757");
}

#[test]
fn service_path_compaction_preserves_scalar_and_truncated_tool_payload_carriers() {
    // Childless tool-like envelopes survive the real service path for scalar
    // carriers too (Primary/Action), and a huge blob-backed call whose
    // arguments object is cut mid-preview keeps the incomplete-carrier
    // sentinel instead of collapsing to empty transport (trace).
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");

    let scalar_call = raw_import_op(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"function_call","name":"Read","input":"/tmp/x","parameters":true}}"#,
    );

    let blob_raw = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call","name":"Bash","arguments":{{"command":"{}","cwd":"/tmp"}}}}}}"#,
        "x".repeat(200_000),
    );
    let blob_ref = store_blob(&chain_dir, blob_raw.as_bytes());
    let blob_call = Op {
        id: OpId::new(NodeId(2), 0, 1),
        parents: ParentSet::One(scalar_call.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Blob(blob_ref),
            raw_hash: None,
        }),
    };

    let id_only = raw_import_op(
        3,
        1,
        3_000,
        Some(blob_call.id),
        r#"{"type":"response_item","payload":{"type":"function_call","id":"call_0"}}"#,
    );

    let mut page = editchain_codec::page::Page::new(0);
    for op in [&scalar_call, &blob_call, &id_only] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut ws = Workspace::open(tmp.path().to_str().unwrap(), ".editchain")
        .expect("open workspace through the real service path");
    let raw = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: false,
    });

    for (key, label) in [
        (scalar_call.id.to_string(), "scalar carrier call"),
        (blob_call.id.to_string(), "truncated blob carrier call"),
    ] {
        let row = window
            .rows
            .iter()
            .find(|r| r.node_key == key)
            .unwrap_or_else(|| panic!("{label} missing from raw window"));
        assert_eq!(row.visibility, Visibility::Primary, "{label}");
        assert_eq!(row.record_role, RecordRole::Action, "{label}");
        assert_eq!(row.activity_kind, ActivityKind::Execute, "{label}");
    }

    let id_only_row = window
        .rows
        .iter()
        .find(|r| r.node_key == id_only.id.to_string())
        .expect("id-only row");
    assert_eq!(id_only_row.visibility, Visibility::Trace);
}

#[test]
fn service_path_marks_duplicate_response_item_event_msg_pairs_after_compaction() {
    // Exact raw pair shapes: a `response_item` message/assistant copy paired
    // with an `event_msg` agent_message carrying the same text at the same
    // timestamp in the same source chain. After the service path compacts the
    // records, the marker-with-colon pair classifies as trace on both sides,
    // the plain narrative pair demotes only the response_item copy (the
    // event_msg stays visible), and a unique response_item stays primary. Raw
    // row count semantics are unchanged: every row survives in raw mode.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");

    let marker_response = raw_import_op(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"[external_agent_tool_call: Bash]\ndescription: audit the tree\n[/external_agent_tool_call]"}]}}"#,
    );
    let marker_event = raw_import_op(
        1,
        2,
        1_000,
        Some(marker_response.id),
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_call: Bash]\ndescription: audit the tree\n[/external_agent_tool_call]"}}"#,
    );
    let narrative_response = raw_import_op(
        1,
        3,
        2_000,
        Some(marker_event.id),
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"working on it"}]}}"#,
    );
    let narrative_event = raw_import_op(
        1,
        4,
        2_000,
        Some(narrative_response.id),
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"working on it"}}"#,
    );
    let unique_response = raw_import_op(
        1,
        5,
        3_000,
        Some(narrative_event.id),
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"unique narrative"}]}}"#,
    );

    let mut page = editchain_codec::page::Page::new(0);
    for op in [
        &marker_response,
        &marker_event,
        &narrative_response,
        &narrative_event,
        &unique_response,
    ] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut ws = Workspace::open(tmp.path().to_str().unwrap(), ".editchain")
        .expect("open workspace through the real service path");
    let raw = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let fixed = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    );
    let raw_window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: false,
    });
    assert_eq!(
        raw_window.rows.len(),
        5,
        "raw row count semantics unchanged"
    );

    let row_meta = |key: &str| {
        raw_window
            .rows
            .iter()
            .find(|r| r.node_key == key)
            .unwrap_or_else(|| panic!("row {key} missing from raw window"))
    };

    let marker_response_row = row_meta(&marker_response.id.to_string());
    assert_eq!(marker_response_row.visibility, Visibility::Trace);
    assert_eq!(marker_response_row.record_role, RecordRole::Echo);
    assert_eq!(marker_response_row.activity_kind, ActivityKind::External);

    let marker_event_row = row_meta(&marker_event.id.to_string());
    assert_eq!(marker_event_row.visibility, Visibility::Trace);
    assert_eq!(marker_event_row.record_role, RecordRole::Echo);
    assert_eq!(marker_event_row.activity_kind, ActivityKind::External);

    let narrative_response_row = row_meta(&narrative_response.id.to_string());
    assert_eq!(narrative_response_row.visibility, Visibility::Trace);
    assert_eq!(narrative_response_row.record_role, RecordRole::Echo);
    assert_eq!(narrative_response_row.activity_kind, ActivityKind::External);

    let narrative_event_row = row_meta(&narrative_event.id.to_string());
    assert_eq!(narrative_event_row.visibility, Visibility::Primary);
    assert_eq!(narrative_event_row.record_role, RecordRole::Narrative);
    assert_eq!(
        narrative_event_row.activity_kind,
        ActivityKind::Conversation
    );

    let unique_response_row = row_meta(&unique_response.id.to_string());
    assert_eq!(unique_response_row.visibility, Visibility::Primary);
    assert_eq!(unique_response_row.record_role, RecordRole::Narrative);
    assert_eq!(
        unique_response_row.activity_kind,
        ActivityKind::Conversation
    );

    let fixed_window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &fixed,
        include_layout: false,
    });
    let fixed_keys: Vec<String> = fixed_window
        .rows
        .iter()
        .map(|r| r.node_key.clone())
        .collect();
    assert!(
        !fixed_keys.contains(&marker_response.id.to_string())
            && !fixed_keys.contains(&marker_event.id.to_string())
            && !fixed_keys.contains(&narrative_response.id.to_string()),
        "trace rows hidden by the fixed filter: {fixed_keys:?}"
    );
    assert!(
        fixed_keys.contains(&narrative_event.id.to_string())
            && fixed_keys.contains(&unique_response.id.to_string()),
        "primary rows survive the fixed filter: {fixed_keys:?}"
    );
}

#[test]
fn service_path_truncated_echo_texts_never_pair_but_untruncated_exact_pairs_do() {
    // Two distinct long texts sharing one display-preview prefix compact to
    // the SAME bounded text; the service flags both as truncated, so the
    // response_item must NOT be demoted as a duplicate. A shorter exact
    // untruncated pair still demotes its response_item after compaction.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");

    let shared_prefix = "same-prefix-line-".repeat(200);
    let long_response = raw_import_op(
        1,
        1,
        1_000,
        None,
        &format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{shared_prefix}TAIL-ONE"}}]}}}}"#,
        ),
    );
    let long_event = raw_import_op(
        1,
        2,
        1_000,
        Some(long_response.id),
        &format!(
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{shared_prefix}TAIL-TWO"}}}}"#,
        ),
    );
    // Untruncated exact pair: 900 chars is inside the display budget, so the
    // full text survives compaction and the pair still demotes the response.
    let exact_text = format!("{}{}", "exact untruncated narrative ".repeat(32), "narr");
    assert_eq!(
        exact_text.chars().count(),
        900,
        "kept under the display budget"
    );
    let exact_response = raw_import_op(
        1,
        3,
        2_000,
        Some(long_event.id),
        &format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{exact_text}"}}]}}}}"#,
        ),
    );
    let exact_event = raw_import_op(
        1,
        4,
        2_000,
        Some(exact_response.id),
        &format!(
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{exact_text}"}}}}"#,
        ),
    );

    let mut page = editchain_codec::page::Page::new(0);
    for op in [&long_response, &long_event, &exact_response, &exact_event] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut ws = Workspace::open(tmp.path().to_str().unwrap(), ".editchain")
        .expect("open workspace through the real service path");
    let raw = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: false,
    });
    assert_eq!(window.rows.len(), 4, "raw row count semantics unchanged");

    let row_meta = |key: &str| {
        window
            .rows
            .iter()
            .find(|r| r.node_key == key)
            .unwrap_or_else(|| panic!("row {key} missing from raw window"))
    };

    let long_response_row = row_meta(&long_response.id.to_string());
    assert_eq!(
        long_response_row.visibility,
        Visibility::Primary,
        "truncated same-prefix response is never demoted"
    );
    assert_eq!(
        long_response_row.record_role,
        RecordRole::Narrative,
        "truncated response keeps its narrative taxonomy"
    );

    let long_event_row = row_meta(&long_event.id.to_string());
    assert_eq!(
        long_event_row.visibility,
        Visibility::Primary,
        "truncated event row stays visible"
    );

    let exact_response_row = row_meta(&exact_response.id.to_string());
    assert_eq!(
        exact_response_row.visibility,
        Visibility::Trace,
        "untruncated exact pair still demotes the response copy after compaction"
    );
    assert_eq!(exact_response_row.record_role, RecordRole::Echo);
    assert_eq!(exact_response_row.activity_kind, ActivityKind::External);

    let exact_event_row = row_meta(&exact_event.id.to_string());
    assert_eq!(exact_event_row.visibility, Visibility::Primary);
    assert_eq!(exact_event_row.record_role, RecordRole::Narrative);
}

#[test]
fn cancelled_branch_rows_ship_muted_node_and_child_owned_edge_geometry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let root = raw_import_op(
        1,
        1,
        1_000,
        None,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"root"}]}}"#,
    );
    let active = raw_import_op(
        2,
        1,
        3_000,
        Some(root.id),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"continue"}]}}"#,
    );
    let cancelled = raw_import_op(
        3,
        1,
        2_000,
        Some(root.id),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},"interruptedMessageId":"msg_cancelled"}"#,
    );
    let mut page = editchain_codec::page::Page::new(0);
    for op in [&root, &active, &cancelled] {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode"));
    }
    write_page(&chain_dir, &page);

    let mut workspace =
        Workspace::open(tmp.path().to_str().unwrap(), ".editchain").expect("open workspace");
    let raw = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let window = workspace.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: true,
    });
    let row = |id: OpId| {
        window
            .rows
            .iter()
            .find(|row| row.node_key == id.to_string())
            .unwrap_or_else(|| panic!("missing row {id}"))
    };
    let active_row = row(active.id);
    let cancelled_row = row(cancelled.id);
    let root_row = row(root.id);

    assert_eq!(active_row.chain_state, ChainState::Active);
    assert_eq!(cancelled_row.chain_state, ChainState::Muted);
    assert!(cancelled_row.muted_below.contains(&cancelled_row.lane));
    assert!(root_row.muted_above.contains(&cancelled_row.lane));
    assert!(
        root_row
            .muted_transitions
            .contains(&(cancelled_row.lane, root_row.lane)),
        "the gray edge owns its bend in the active parent row"
    );
    assert!(
        !root_row.muted_above.contains(&active_row.lane),
        "the active sibling and shared trunk retain their palette color"
    );
}

#[test]
fn prepared_snapshot_manifest_records_projection_revision_forty_four() {
    // Stale snapshots from earlier projection revisions (pre-hide_trace,
    // pre cross-record response_item/event_msg duplicate pairing, pre
    // response_item label/compact summary changes, pre truncated-echo-text
    // duplicate-pair exclusion, pre prefix-string escape decoding, and pre
    // Activity work-unit/promotion/execute-run/Plan-repeat/work-group bundling
    // and inline-compaction semantics, exact provider relations, legacy Codex
    // token-usage contraction, correlation-only tool results, exact Claude
    // response contraction, authoritative view-parent rewrites, and causal
    // produced-commit branch edges, default timestamp-zero omission,
    // source-control file rows, and singleton nested-work flattening) must not
    // be served silently: the revision participates in the snapshot identity
    // hash.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let first = msg_op(41, 1, b"snapshot first");
    let mut page = editchain_codec::page::Page::new(0);
    page.add_record(0, editchain_codec::frame::encode_op(&first).unwrap());
    write_page(&chain_dir, &page);

    let report =
        prepare_render_snapshot(tmp.path(), Path::new(".editchain")).expect("prepare snapshot");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(report.path.join("manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    assert_eq!(manifest["format"], "editchain-render-snapshot");
    assert_eq!(manifest["identity"]["projection_revision"], 44u64);
}

#[test]
fn prepared_snapshot_omits_timestamp_zero_top_level_rows_and_keeps_bundled_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let dated = raw_import_op(42, 1, 1_000, None, r#"{"type":"user"}"#);
    let dated_message = raw_message_child(42, 2, dated.id, 1_000, "dated");
    let mut undated = raw_import_op(
        42,
        3,
        0,
        Some(dated.id),
        r#"{"type":"custom-title","customTitle":"undated"}"#,
    );
    undated.tags |= Tags::META;

    let mut page = editchain_codec::page::Page::new(0);
    page.add_record(
        0,
        editchain_codec::frame::encode_op(&dated).expect("encode dated op"),
    );
    page.add_record(
        0,
        editchain_codec::frame::encode_op(&dated_message).expect("encode dated message"),
    );
    page.add_record(
        0,
        editchain_codec::frame::encode_op(&undated).expect("encode undated op"),
    );
    write_page(&chain_dir, &page);

    let report =
        prepare_render_snapshot(tmp.path(), Path::new(".editchain")).expect("prepare snapshot");
    assert_eq!(report.top_level_rows, 1, "only the dated row is top-level");
    assert_eq!(report.rows, 2, "bundled metadata remains expandable");
    let rows = std::fs::read_to_string(report.path.join("rows.ndjson")).expect("read rows");
    let presented: Vec<serde_json::Value> = rows
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse row"))
        .collect();
    assert_eq!(presented.len(), 2);
    assert_eq!(presented[0]["node_key"], dated.id.to_string());
    assert_ne!(presented[0]["timestamp_ms"], 0);
    let sub_ops = presented[0]["sub_ops"].as_array().expect("sub_ops array");
    assert_eq!(sub_ops.len(), 1);
    assert_eq!(sub_ops[0]["op_id"], undated.id.to_string());
    assert_eq!(sub_ops[0]["timestamp_ms"], 0);
    assert_eq!(presented[1]["op_id"], undated.id.to_string());
    assert_eq!(presented[1]["timestamp_ms"], 0);
    assert_eq!(presented[1]["is_subop"], true);
}

#[test]
fn activity_keeps_context_compaction_visible_and_inline_while_raw_stays_exact() {
    let root = raw_import_op(
        9,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message"}}"#,
    );
    let root_message = turn_message_child(109, 101, root.id, 1, "request");
    let mut compacted = raw_import_op(
        9,
        2,
        2_000,
        Some(root.id),
        r#"{"type":"compacted","payload":{"message":"","replacement_history":[]}}"#,
    );
    compacted.tags |= Tags::STRUCTURAL;
    let continuation = raw_import_op(
        9,
        3,
        3_000,
        Some(root.id),
        r#"{"type":"response_item","payload":{"type":"message"}}"#,
    );
    let continuation_message = turn_message_child(209, 103, continuation.id, 1, "continued");
    let projection = HistoryProjection::from_ops(vec![
        root.clone(),
        root_message,
        compacted.clone(),
        continuation.clone(),
        continuation_message,
    ]);
    let mut ws = Workspace::from_projection(projection);
    let fixed = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    );

    let activity = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &fixed,
        include_layout: true,
    });
    let activity_rows: Vec<&HistoryRow> =
        activity.rows.iter().filter(|row| !row.is_subop).collect();
    assert_eq!(activity_rows.len(), 3);
    let activity_work = activity_rows
        .iter()
        .copied()
        .find(|row| row.node_key == compacted.id.to_string())
        .expect("Activity wraps the compaction checkpoint in one work group");
    assert_eq!(activity_work.visibility, Visibility::Primary);
    assert_eq!(activity_work.activity_kind, ActivityKind::Work);
    assert_eq!(
        activity_work
            .activity_bundle
            .as_ref()
            .map(|bundle| bundle.kind),
        Some(ActivityBundleKind::WorkGroup)
    );
    assert_eq!(activity_work.parents, vec![root.id.to_string()]);
    let work_index = activity
        .rows
        .iter()
        .position(|row| row.node_key == activity_work.node_key)
        .expect("work row index");
    let activity_checkpoint = activity
        .rows
        .iter()
        .find(|row| {
            row.is_subop
                && row.parent_row == Some(work_index)
                && row.op_id.as_deref() == Some(compacted.id.to_string().as_str())
        })
        .expect("expanded work group retains the compaction checkpoint");
    assert_eq!(activity_checkpoint.hierarchy_depth, 1);
    assert_eq!(activity_checkpoint.visibility, Visibility::Primary);
    assert_eq!(activity_checkpoint.activity_kind, ActivityKind::Plan);
    let activity_continuation = activity_rows
        .iter()
        .copied()
        .find(|row| row.node_key == continuation.id.to_string())
        .expect("Activity continuation");
    assert_eq!(
        activity_continuation.parents,
        vec![compacted.id.to_string()]
    );
    assert!(
        activity_rows
            .iter()
            .all(|row| row.lane == activity_work.lane),
        "checkpoint and continuation stay on the session lane"
    );

    let raw = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &no_filter(),
        include_layout: true,
    });
    let raw_checkpoint = raw
        .rows
        .iter()
        .find(|row| row.node_key == compacted.id.to_string())
        .expect("Raw keeps the compaction checkpoint");
    assert_eq!(raw_checkpoint.visibility, Visibility::Primary);
    let raw_continuation = raw
        .rows
        .iter()
        .find(|row| row.node_key == continuation.id.to_string())
        .expect("Raw continuation");
    assert_eq!(
        raw_continuation.parents,
        vec![root.id.to_string()],
        "Raw preserves the imported sibling topology"
    );
}

#[test]
fn activity_view_bundles_execute_runs_but_raw_profile_stays_exact_and_ordered() {
    // One turn: user request, three successful tools, agent answer. The fixed
    // Activity view folds the three tool rows into one expandable summary row;
    // the raw profile must keep every row, in the same order, un-bundled.
    let ops = turn_chain_ops(
        &[
            ("tool", Some("completed")),
            ("tool", Some("completed")),
            ("tool", Some("completed")),
            ("message", None),
        ],
        1,
    );
    let projection = HistoryProjection::from_ops(ops);
    let mut ws = Workspace::from_projection(projection);
    let fixed = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    );
    let raw = no_filter();
    let activity_window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &fixed,
        include_layout: false,
    });
    let raw_window = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &raw,
        include_layout: false,
    });

    // Raw profile: exact and ordered — five top-level rows, newest-first, no
    // synthetic bundle anywhere.
    assert_eq!(raw_window.rows.len(), 5);
    let raw_kinds: Vec<&str> = raw_window
        .rows
        .iter()
        .map(|row| row.kind.as_str())
        .collect();
    assert_eq!(
        raw_kinds,
        vec!["message", "tool", "tool", "tool", "message"]
    );
    assert!(
        raw_window
            .rows
            .iter()
            .all(|row| !row.is_subop && row.summary != "3 tool steps (success)"),
        "raw profile never bundles"
    );
    assert!(
        raw_window
            .rows
            .iter()
            .all(|row| row.activity_bundle.is_none()),
        "raw (unbundled) rows never carry activity-bundle metadata"
    );
    // Raw rows still carry the additive work-unit/promotion wire fields.
    let raw_value = serde_json::to_value(&raw_window.rows[0]).expect("serialize raw row");
    assert_eq!(raw_value["work_unit"]["id"], "session:1/turn:1");
    assert_eq!(raw_value["work_unit"]["count"], 5u64);
    assert_eq!(raw_value["work_unit"]["is_start"], true);
    assert_eq!(raw_value["session_summary"]["count"], 5u64);
    assert_eq!(raw_value["promoted"], true);
    assert_eq!(
        raw_window
            .rows
            .iter()
            .filter(|row| row.session_summary.is_some())
            .count(),
        1,
        "one true session endpoint carries the raw-view summary marker"
    );

    // Activity view: chat / outer work / chat. Because the execute bundle is
    // the work interval's only grouped member, opening work reveals its three
    // original tools directly without a redundant second disclosure.
    assert_eq!(activity_window.total, 6);
    assert_eq!(activity_window.rows.len(), 6);
    let session_summary = activity_window
        .rows
        .iter()
        .find_map(|row| row.session_summary.as_ref().map(|summary| (row, summary)))
        .expect("activity view has one session summary marker");
    assert!(!session_summary.0.is_subop);
    assert_eq!(session_summary.1.count, 3);
    assert_eq!(
        activity_window
            .rows
            .iter()
            .filter(|row| row.session_summary.is_some())
            .count(),
        1
    );
    let work_group = activity_window
        .rows
        .iter()
        .find(|row| {
            row.activity_bundle
                .as_ref()
                .is_some_and(|bundle| bundle.kind == ActivityBundleKind::WorkGroup)
        })
        .expect("activity view has the outer work group");
    assert_eq!(work_group.activity_kind, ActivityKind::Work);
    assert_eq!(
        work_group
            .activity_bundle
            .as_ref()
            .map(|meta| meta.member_count),
        Some(3)
    );
    assert_eq!(work_group.summary, "3 tool calls");
    assert!(activity_window.rows.iter().all(|row| {
        row.activity_bundle
            .as_ref()
            .is_none_or(|bundle| bundle.kind != ActivityBundleKind::ExecuteRun)
    }));
    assert_eq!(
        work_group.work_unit.as_ref().map(|unit| unit.id.as_str()),
        Some("session:1/turn:1")
    );
    assert_eq!(
        work_group.work_unit.as_ref().map(|unit| unit.count),
        Some(3u64)
    );
    let work_index = activity_window
        .rows
        .iter()
        .position(|row| row.node_key == work_group.node_key)
        .expect("work row index");
    let member_rows: Vec<&HistoryRow> = activity_window
        .rows
        .iter()
        .filter(|row| row.hierarchy_depth == 1 && row.parent_row == Some(work_index))
        .collect();
    assert_eq!(member_rows.len(), 3);
    assert!(member_rows[0].op_id.is_some());
    assert!(member_rows[1].op_id.is_some());
    assert!(member_rows[2].op_id.is_some());
    assert!(
        member_rows.iter().all(|row| row.activity_bundle.is_none()),
        "leaf member rows never carry activity-bundle metadata"
    );
    // The member rows are the original tool rows: ids 2..=4 as raw imports.
    let member_ids: Vec<String> = member_rows
        .iter()
        .map(|row| row.op_id.clone().unwrap_or_default())
        .collect();
    assert_eq!(member_ids, vec!["4:0:4", "3:0:3", "2:0:2"]);
    // Expansion index ships once for the snapshot window.
    assert_eq!(
        activity_window.sub_op_counts.as_deref(),
        Some(&[0usize, 3, 0][..])
    );
    assert_eq!(
        activity_window.expansion_spans.as_deref(),
        Some(
            &[editchain_protocol::ExpansionSpanDto {
                row: 1,
                descendant_count: 3,
            }][..]
        )
    );
    // Top-level order is preserved: agent answer, bundle, user request.
    let top_level: Vec<&HistoryRow> = activity_window
        .rows
        .iter()
        .filter(|row| !row.is_subop)
        .collect();
    assert_eq!(top_level.len(), 3);
    assert_eq!(top_level[0].activity_kind, ActivityKind::Conversation);
    assert_eq!(top_level[1].node_key, work_group.node_key);
    assert_eq!(top_level[2].activity_kind, ActivityKind::Conversation);
    assert!(
        top_level
            .iter()
            .filter(|row| row.node_key != work_group.node_key)
            .all(|row| row.activity_bundle.is_none()),
        "ordinary top-level rows never carry activity-bundle metadata"
    );
}

#[test]
fn activity_view_groups_repeated_plans_as_expandable_linear_updates() {
    let projection = HistoryProjection::from_ops(repeated_plan_chain_ops());
    let mut ws = Workspace::from_projection(projection);
    let fixed = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    );
    let activity = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &fixed,
        include_layout: true,
    });
    let raw = ws.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &no_filter(),
        include_layout: true,
    });

    let work_group = activity
        .rows
        .iter()
        .find(|row| {
            row.activity_bundle
                .as_ref()
                .is_some_and(|metadata| metadata.kind == ActivityBundleKind::WorkGroup)
        })
        .expect("Activity has one outer work group");
    assert_eq!(work_group.activity_kind, ActivityKind::Work);
    assert_eq!(
        work_group
            .activity_bundle
            .as_ref()
            .map(|metadata| metadata.member_count),
        Some(3)
    );
    let work_index = activity
        .rows
        .iter()
        .position(|row| row.node_key == work_group.node_key)
        .expect("work row index");

    assert!(activity.rows.iter().all(|row| {
        row.activity_bundle
            .as_ref()
            .is_none_or(|bundle| bundle.kind != ActivityBundleKind::PlanRepeat)
    }));
    let members: Vec<&HistoryRow> = activity
        .rows
        .iter()
        .filter(|row| row.hierarchy_depth == 1 && row.parent_row == Some(work_index))
        .collect();
    assert_eq!(members.len(), 3);
    assert_eq!(
        members
            .iter()
            .map(|row| row.summary.as_str())
            .collect::<Vec<_>>(),
        vec![
            "__Planning build and dry-run import steps__",
            "Planning   build and dry-run import steps",
            "**Planning build and dry-run import steps**",
        ],
        "expansion retains every original Plan presentation and order"
    );
    assert!(members.iter().all(|row| row.op_id.is_some()));

    let top_level: Vec<&HistoryRow> = activity.rows.iter().filter(|row| !row.is_subop).collect();
    assert_eq!(top_level.len(), 3);
    assert!(top_level.iter().all(|row| row.lane == 0));
    assert_eq!(top_level[0].parents, vec![work_group.node_key.clone()]);
    assert_eq!(work_group.parents, vec![top_level[2].node_key.clone()]);
    assert_eq!(
        activity.max_lane, 0,
        "grouping does not allocate a branch lane"
    );

    let raw_top_level: Vec<&HistoryRow> = raw.rows.iter().filter(|row| !row.is_subop).collect();
    assert_eq!(raw_top_level.len(), 5);
    assert_eq!(
        raw_top_level
            .iter()
            .filter(|row| row.activity_kind == ActivityKind::Plan)
            .count(),
        3
    );
    assert!(
        raw.rows.iter().all(|row| row.activity_bundle.is_none()),
        "Raw remains an exact ungrouped source view"
    );
}

#[test]
fn prepared_snapshot_serves_flattened_activity_view_and_records_current_revision() {
    // The pregenerated render snapshot must serve the SAME bundled Activity
    // rows as the live projection (work-unit/promotion/bundling parity) and
    // record the bumped projection revision in its identity.
    let tmp = tempfile::tempdir().expect("tempdir");
    let chain_dir = tmp.path().join(".editchain");
    let ops = turn_chain_ops(
        &[
            ("tool", Some("completed")),
            ("tool", Some("completed")),
            ("tool", Some("completed")),
            ("message", None),
        ],
        1,
    );
    let mut page = editchain_codec::page::Page::new(0);
    for op in &ops {
        page.add_record(0, editchain_codec::frame::encode_op(op).expect("encode op"));
    }
    write_page(&chain_dir, &page);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    );
    let mut live = Workspace::open(tmp.path().to_str().unwrap(), ".editchain").expect("live open");
    let expected = live.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &filter,
        include_layout: true,
    });
    assert_eq!(
        expected.total, 6,
        "activity view stores chat/work/chat + 3 direct work members"
    );
    assert!(
        expected
            .rows
            .iter()
            .all(|row| row.summary != "3 tool steps (success)"),
        "live activity view omits the redundant execute bundle"
    );

    let report =
        prepare_render_snapshot(tmp.path(), Path::new(".editchain")).expect("prepare snapshot");
    assert_eq!(report.rows, expected.total);
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(report.path.join("manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    assert_eq!(manifest["identity"]["projection_revision"], 44u64);

    let mut cached =
        Workspace::open(tmp.path().to_str().unwrap(), ".editchain").expect("cached open");
    let actual = cached.history_window(HistoryWindowOptions {
        offset: 0,
        limit: 100,
        hide_submodules: true,
        filter: &filter,
        include_layout: true,
    });
    assert_eq!(
        serde_json::to_value(&actual).expect("serialize actual"),
        serde_json::to_value(&expected).expect("serialize expected"),
        "snapshot rows must match the live bundled Activity projection byte-for-byte"
    );
}
