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
    ActorId, Clock, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
    ToolOp, ToolStage,
};
use editchain_project::filter::ChainFilter;
use editchain_protocol::{Request, RequestBody, ResponseBody, SearchFiltersDto};
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

    let filter = ChainFilter::new(String::new(), String::new(), String::new(), false, true);
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
    ChainFilter::new(String::new(), String::new(), String::new(), false, false)
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

fn run(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn op_identifiers_above_2_53_round_trip_exactly_through_window_details_and_search() {
    let big_op = msg_op(OVER_2_53, 42, b"needle-exact-id");
    let projection = editchain_project::HistoryProjection::from_ops(vec![big_op.clone()]);
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
    let projection = editchain_project::HistoryProjection::from_ops(ops);
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
    let projection = editchain_project::HistoryProjection::from_ops(ops);
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
    let projection = editchain_project::HistoryProjection::from_ops(vec![tool, msg]);
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
