//! Process-level contracts for the engine CLI and its shell-facing behavior.

use clap as _;
use ctrlc as _;
use editchain_git as _;
use editchain_import as _;
use editchain_index as _;
use serde as _;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use editchain_engine::{
    ActorId, BlobRef, Clock, ContentId, Engine, MessageOp, NodeId, Op, OpId, OpKind, ParentSet,
    Payload, ScopeRef, Tags,
};
use serde_json::{json, Value};

#[path = "streams.rs"]
mod cli_streams;

fn message(sequence: u64, content: Payload) -> Op {
    Op {
        id: OpId::new(NodeId(7), 0, sequence),
        parents: ParentSet::None,
        actor: ActorId(9),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::NONE,
        kind: OpKind::Message(MessageOp {
            content,
            content_type: Payload::Empty,
        }),
    }
}

fn command(chain: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_editchain"));
    let _command = command.arg("--chain").arg(chain).args(["--output", "json"]);
    command
}

fn run(chain: &Path, args: &[&str], input: &[u8], code: i32) -> Output {
    let mut child = command(chain)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn result(chain: &Path, args: &[&str], input: &[u8], code: i32) -> Value {
    let output = run(chain, args, input, code);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn append(chain: &Path, operation: &Op) {
    let output = run(
        chain,
        &["append"],
        &serde_json::to_vec(operation).unwrap(),
        0,
    );
    assert!(
        output.stderr.is_empty(),
        "successful append has no diagnostics"
    );
}

#[test]
fn package_help_and_exit_contracts() {
    assert_eq!(env!("CARGO_PKG_NAME"), "editchain");
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let help = run(&chain, &["--help"], b"", 0);
    assert!(help.stderr.is_empty(), "help belongs on stdout");
    let text = String::from_utf8(help.stdout).unwrap();
    for name in [
        "init",
        "append",
        "import",
        "export",
        "meta",
        "annotations",
        "reflections",
        "follow",
        "integrity",
        "rebuild",
        "replicate",
    ] {
        assert!(text.contains(name), "missing help for {name}");
    }
    for args in [vec!["prepare-view"], vec!["import"]] {
        let rejected = run(&chain, &args, b"", 2);
        assert!(
            rejected.stdout.is_empty(),
            "usage diagnostics belong on stderr"
        );
        assert!(
            !chain.exists(),
            "invalid commands cannot initialize storage"
        );
    }
    let missing = run(&chain, &["history"], b"", 3);
    assert!(
        missing.stdout.is_empty(),
        "diagnostics cannot contaminate machine results"
    );
    assert!(!chain.exists(), "queries cannot initialize an absent chain");
    let invalid = run(
        &chain,
        &["content", "bad-id", "--field", "MessageContent"],
        b"",
        2,
    );
    assert!(invalid.stdout.is_empty(), "usage errors belong on stderr");
    let _init = result(&chain, &["init"], b"", 0);
    let _bad = result(&chain, &["append"], b"{bad-json}", 2);
    assert!(
        Engine::open(&chain)
            .unwrap()
            .snapshot()
            .unwrap()
            .operations()
            .next()
            .is_none(),
        "bad JSON did not append"
    );
    let human = run(&chain, &["--output", "human", "integrity"], b"", 0);
    assert!(
        String::from_utf8(human.stdout)
            .unwrap()
            .contains("clean: true"),
        "human output is labeled"
    );
}

#[test]
fn stdin_replay_conflicts_and_evidence_archive_preserve_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let target = temp.path().join("target");
    let _init = run(&chain, &["init"], b"", 0);
    let blob: BlobRef =
        serde_json::from_value(result(&chain, &["store-blob"], b"\0\xff\r\n", 0)).unwrap();
    let operation = message(1, Payload::Blob(blob));
    append(&chain, &operation);
    let duplicate = result(
        &chain,
        &["append", "--encoded"],
        &editchain_engine::encode_op(&operation).unwrap(),
        0,
    );
    assert_eq!(duplicate.get("admission"), Some(&json!("duplicate")));
    let changed = message(1, Payload::Inline(b"conflicting evidence".to_vec()));
    let _conflict = result(
        &chain,
        &["append"],
        &serde_json::to_vec(&changed).unwrap(),
        4,
    );
    let variants = result(&chain, &["variants", "7:0:1"], b"", 0);
    assert_eq!(variants.as_array().unwrap().len(), 2);
    let _conflicted = result(&chain, &["operation", "7:0:1"], b"", 4);
    let _missing = result(&chain, &["operation", "7:0:999"], b"", 3);
    let _integrity = result(&chain, &["integrity"], b"", 4);
    let archive = run(&chain, &["--output", "jsonl", "export"], b"", 0);
    let _target = run(&target, &["init"], b"", 0);
    let _restore = result(&target, &["append", "--archive"], &archive.stdout, 4);
    let _replay = result(&target, &["append", "--archive"], &archive.stdout, 0);
    let engine = Engine::open(&chain).unwrap();
    let replica = Engine::open(&target).unwrap();
    assert_eq!(
        engine.snapshot().unwrap().evidence(),
        replica.snapshot().unwrap().evidence()
    );
    let content = run(
        &target,
        &["blob", &serde_json::to_string(&blob.id).unwrap(), "--raw"],
        b"",
        0,
    );
    assert_eq!(content.stdout, b"\0\xff\r\n");
}

#[test]
fn annotations_reflections_and_queries_use_recorded_facts() {
    use editchain_engine::{
        FileEdit, FileOp, FileStage, FrontierSet, NoteOp, NoteRelationship, PathId, ReflectionOp,
        WindowRef,
    };
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let _init = run(&chain, &["init"], b"", 0);
    append(
        &chain,
        &message(1, Payload::Inline(b"hello evidence".to_vec())),
    );
    let note = Op {
        kind: OpKind::Note(NoteOp {
            target_ids: vec![OpId::new(NodeId(7), 0, 1)],
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(b"note".to_vec()),
        }),
        ..message(2, Payload::Empty)
    };
    let _note = result(
        &chain,
        &["annotate"],
        &serde_json::to_vec(&note).unwrap(),
        0,
    );
    let reflection = Op {
        kind: OpKind::Reflection(ReflectionOp {
            scope: ScopeRef::None,
            covers: FrontierSet(vec![]),
            window: WindowRef {
                start_seq: 1,
                end_seq: 2,
            },
            summary: Payload::Inline(b"summary".to_vec()),
            anchors: Payload::Empty,
        }),
        ..message(3, Payload::Empty)
    };
    let _reflection = result(
        &chain,
        &["reflect"],
        &serde_json::to_vec(&reflection).unwrap(),
        0,
    );
    let empty = result(&chain, &["annotations", "--limit", "1"], b"", 0);
    assert!(
        empty.get("items").unwrap().as_array().unwrap().is_empty(),
        "filtered pages can be empty"
    );
    assert!(
        !empty.get("next_after").unwrap().is_null(),
        "filtered pagination preserves the scan cursor"
    );
    for args in [
        vec!["annotations", "--after", "7:0:1"],
        vec!["reflections"],
        vec!["history", "--key", "{\"Actor\":9}"],
    ] {
        let page = result(&chain, &args, b"", 0);
        assert!(
            !page.get("items").unwrap().as_array().unwrap().is_empty(),
            "query returns recorded facts"
        );
    }
    let search = result(&chain, &["search", "evidence"], b"", 0);
    assert_eq!(search.get("hits").unwrap().as_array().unwrap().len(), 1);
    let raw = run(
        &chain,
        &["content", "7:0:1", "--field", "MessageContent", "--raw"],
        b"",
        0,
    );
    assert_eq!(raw.stdout, b"hello evidence");
    let _meta = result(&chain, &["meta", "7:0:1"], b"", 0);
    let _ancestors = result(&chain, &["ancestors", "7:0:1"], b"", 0);
    let links = result(&chain, &["relationships"], b"", 0);
    assert_eq!(links.get("items").unwrap().as_array().unwrap().len(), 1);
    let before =
        serde_json::to_string(&json!({"operation":note.id,"field":"NoteContent"})).unwrap();
    let after =
        serde_json::to_string(&json!({"operation":reflection.id,"field":"ReflectionSummary"}))
            .unwrap();
    let comparison = result(
        &chain,
        &["compare", "--before", &before, "--after", &after],
        b"",
        0,
    );
    assert!(
        comparison
            .get("comparison")
            .unwrap()
            .get("Changed")
            .is_some(),
        "comparison uses exact fields"
    );
    let engine = Engine::open(&chain).unwrap();
    let base = engine.store_blob(b"before").unwrap();
    let after = engine.store_blob(b"after").unwrap();
    let file = Op {
        kind: OpKind::File(FileOp {
            path: PathId(1),
            stage: FileStage::Applied,
            base: Some(base.id),
            after: Some(after.id),
            edit: FileEdit::Blob(after),
        }),
        ..message(4, Payload::Empty)
    };
    append(&chain, &file);
    let diff = result(&chain, &["diff", "7:0:4"], b"", 0);
    assert!(
        diff.get("Found")
            .unwrap()
            .get("comparison")
            .unwrap()
            .get("Changed")
            .is_some(),
        "diff uses recorded snapshots"
    );
    let history = result(&chain, &["history"], b"", 0);
    let _rebuild = result(&chain, &["rebuild"], b"", 0);
    assert_eq!(result(&chain, &["history"], b"", 0), history);
    let _integrity = result(&chain, &["integrity"], b"", 0);
}

#[test]
fn missing_and_corrupt_blobs_are_explicit_and_recoverable() {
    let temp = tempfile::tempdir().unwrap();
    let engine = Engine::open(temp.path()).unwrap();
    let bytes = b"late blob";
    let hash = *blake3::hash(bytes).as_bytes();
    let reference = BlobRef {
        id: ContentId::Hash256(hash),
        len: u32::try_from(bytes.len()).unwrap(),
    };
    append(temp.path(), &message(1, Payload::Blob(reference)));
    let missing = result(
        temp.path(),
        &["content", "7:0:1", "--field", "MessageContent"],
        b"",
        3,
    );
    assert_eq!(
        missing.get("Found").unwrap().get("value"),
        Some(&json!("Missing"))
    );
    let _search = result(temp.path(), &["search", "late"], b"", 3);
    let _export = result(temp.path(), &["export"], b"", 3);
    let _stored = result(temp.path(), &["store-blob"], bytes, 0);
    let _found = result(
        temp.path(),
        &["content", "7:0:1", "--field", "MessageContent"],
        b"",
        0,
    );
    let _clean = result(temp.path(), &["integrity"], b"", 0);
    let blobs = editchain_store::BlobStore::new(engine.chain_dir().join("blobs")).unwrap();
    std::fs::write(blobs.path_for(&hash), b"damaged").unwrap();
    let _corrupt = result(
        temp.path(),
        &["blob", &serde_json::to_string(&reference.id).unwrap()],
        b"",
        4,
    );
    let _integrity = result(temp.path(), &["integrity"], b"", 4);
}

#[test]
fn source_files_and_directories_use_shared_capture_without_viewer_checkpoints() {
    let temp = tempfile::tempdir().unwrap();
    let sources = temp.path().join("sources");
    std::fs::create_dir(&sources).unwrap();
    let source = sources.join("session.jsonl");
    std::fs::write(
        &source,
        include_bytes!("../../../editchain-import/tests/fixtures/human/session.jsonl"),
    )
    .unwrap();
    let mut recorded = None;
    for (index, input) in [&source, &sources].into_iter().enumerate() {
        let chain = temp.path().join(format!("chain-{index}"));
        let args = [
            "import",
            "--provider",
            "human",
            "--input",
            input.to_str().unwrap(),
        ];
        let report = result(&chain, &args, b"", 0);
        assert!(
            report.get("raw_ops").unwrap().as_u64().unwrap() > 0,
            "raw evidence was captured"
        );
        assert_eq!(
            result(&chain, &args, b"", 0).get("written"),
            Some(&json!(0))
        );
        let history = result(&chain, &["history", "--limit", "1000"], b"", 0);
        if let Some(expected) = &recorded {
            assert_eq!(
                &history, expected,
                "source selection preserves recorded identities"
            );
        } else {
            recorded = Some(history);
        }
        for host_state in ["live-v1", "editor-v1", "search"] {
            assert!(
                !chain.join(host_state).exists(),
                "engine capture cannot create {host_state}"
            );
        }
        let _clean = result(&chain, &["integrity"], b"", 0);
    }
}

#[test]
fn shared_provider_imports_stdin_are_resumable_and_dry_run_is_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let fixture = include_bytes!("../../../editchain-import/tests/fixtures/human/session.jsonl");
    let args = [
        "import",
        "--provider",
        "human",
        "--input",
        "-",
        "--source-name",
        "session.jsonl",
    ];
    let mut preview = args.to_vec();
    preview.push("--dry-run");
    let _preview = result(&chain, &preview, fixture, 0);
    assert!(!chain.exists(), "dry-run has no destination side effects");
    let first = result(&chain, &args, fixture, 0);
    assert!(
        first.get("written").unwrap().as_u64().unwrap() > 0,
        "raw human evidence was admitted"
    );
    let second = result(&chain, &args, fixture, 0);
    assert_eq!(second.get("written"), Some(&json!(0)));
    let claude = include_bytes!("../../../editchain-import/tests/fixtures/claude/session.jsonl");
    let _claude = result(
        &chain,
        &[
            "import",
            "--provider",
            "claude",
            "--input",
            "-",
            "--source-name",
            "session.jsonl",
        ],
        claude,
        0,
    );
}

#[test]
fn local_replication_transfers_conflicts_and_late_blobs_with_selected_scope() {
    let temp = tempfile::tempdir().unwrap();
    let left = temp.path().join("left");
    let right = temp.path().join("right");
    let local = Engine::open(&left).unwrap();
    let remote = Engine::open(&right).unwrap();
    let bytes = b"late";
    let reference = BlobRef {
        id: ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
        len: 4,
    };
    let operation = message(1, Payload::Blob(reference));
    append(&left, &operation);
    let args = [
        "replicate",
        "--peer",
        right.to_str().unwrap(),
        "--namespace",
        "fixture",
        "--share-all",
    ];
    let _partial = result(&left, &args, b"", 3);
    assert_eq!(
        local.snapshot().unwrap().evidence(),
        remote.snapshot().unwrap().evidence()
    );
    let _blob = local.store_blob(bytes).unwrap();
    let _completed = result(&left, &args, b"", 0);
    assert_eq!(
        remote.resolve_blob(&reference).unwrap(),
        editchain_engine::BlobResolution::Found(bytes.to_vec())
    );
    let _repeat = result(&left, &args, b"", 0);
    let encoded = editchain_engine::encode_op(&operation).unwrap();
    let key = editchain_sync::RecordKey::from_encoded(&encoded).unwrap();
    let third = temp.path().join("third");
    let _third = Engine::open(&third).unwrap();
    append(&left, &message(2, Payload::Inline(b"private".to_vec())));
    let scope = temp.path().join("scope.json");
    std::fs::write(
        &scope,
        serde_json::to_vec(&json!({"records":[key],"blobs":[]})).unwrap(),
    )
    .unwrap();
    let _selected = result(
        &left,
        &[
            "replicate",
            "--peer",
            third.to_str().unwrap(),
            "--namespace",
            "selected",
            "--scope",
            scope.to_str().unwrap(),
        ],
        b"",
        3,
    );
    assert_eq!(
        Engine::open(third)
            .unwrap()
            .snapshot()
            .unwrap()
            .stats()
            .accepted,
        1
    );

    let _conflict = result(
        &right,
        &["append"],
        &serde_json::to_vec(&message(1, Payload::Inline(b"peer conflict".to_vec()))).unwrap(),
        4,
    );
    let _replicate = result(&left, &args, b"", 0);
    assert_eq!(
        local.snapshot().unwrap().evidence(),
        remote.snapshot().unwrap().evidence()
    );
    assert_eq!(local.snapshot().unwrap().stats().quarantined, 2);
}

#[test]
fn initialization_git_evidence_and_corrupt_index_rebuild() {
    use editchain_engine::{ChainId, ChainStart, GitLink, GitLinkKind, GitOid, RepositoryId};
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let start = Op {
        scope: ScopeRef::Chain(ChainId(42)),
        kind: OpKind::ChainStart(ChainStart {
            name: b"chain".to_vec(),
            version: 1,
        }),
        ..message(0, Payload::Empty)
    };
    let _init = result(
        &chain,
        &["init", "--input", "-"],
        &serde_json::to_vec(&start).unwrap(),
        0,
    );
    let oid = GitOid::from_hex("0123456789abcdef0123456789abcdef01234567").unwrap();
    let link = Op {
        kind: OpKind::GitLink(GitLink {
            source: start.id,
            target_repo: RepositoryId(5),
            target_oid: oid,
            kind: GitLinkKind::BasedOn,
        }),
        ..message(1, Payload::Empty)
    };
    append(&chain, &link);
    let query = serde_json::to_string(&json!({"repository":5,"oid":oid})).unwrap();
    let git = result(&chain, &["git", "--query", &query], b"", 0);
    assert_eq!(git.get("items").unwrap().as_array().unwrap().len(), 1);
    let entity = serde_json::to_string(&json!({"Git":{"repository":5,"oid":oid}})).unwrap();
    let relations = result(&chain, &["relationships", "--entity", &entity], b"", 0);
    assert_eq!(relations.get("items").unwrap().as_array().unwrap().len(), 1);
    let before = result(&chain, &["history"], b"", 0);
    std::fs::write(chain.join("index-v1/root"), b"damaged checkpoint").unwrap();
    let failed = run(&chain, &["integrity"], b"", 4);
    assert!(
        !failed.status.success(),
        "a corrupt checkpoint must not pass integrity"
    );
    let _rebuild = result(&chain, &["rebuild"], b"", 0);
    assert_eq!(result(&chain, &["history"], b"", 0), before);
}

#[test]
fn archive_rejects_blob_tampering_and_reports_truncated_input() {
    let temp = tempfile::tempdir().unwrap();
    let engine = Engine::open(temp.path()).unwrap();
    let bytes = b"original";
    let reference = BlobRef {
        id: ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
        len: 8,
    };
    let archive = json!([{"type":"header","version":1},{"type":"blob","reference":reference,"bytes":b"tampered"}]);
    let _rejected = result(
        temp.path(),
        &["append", "--archive"],
        &serde_json::to_vec(&archive).unwrap(),
        2,
    );
    assert_eq!(
        engine.resolve_blob(&reference).unwrap(),
        editchain_engine::BlobResolution::Missing
    );
    let _truncated = result(
        temp.path(),
        &["append", "--archive"],
        b"{\"type\":\"header\",\"version\":1}\n",
        2,
    );
    let writer = editchain_store::SegmentStore::open(temp.path()).unwrap();
    let _busy = run(
        temp.path(),
        &["append"],
        &serde_json::to_vec(&message(1, Payload::Empty)).unwrap(),
        5,
    );
    drop(writer);
    append(temp.path(), &message(1, Payload::Empty));
}

#[cfg(unix)]
#[test]
fn shared_codex_import_uses_the_recorded_exporter_contract() {
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let projection = temp.path().join("projection.ndjson");
    std::fs::write(
        &projection,
        include_bytes!("../../../editchain-import/tests/fixtures/codex/projection.ndjson"),
    )
    .unwrap();
    let helper = temp.path().join("helper.sh");
    std::fs::write(&helper, "cat \"$1\"\n").unwrap();
    let args = [
        "import",
        "--provider",
        "codex",
        "--input",
        "-",
        "--source-name",
        "rollout-contract.jsonl",
        "--workspace",
        "/workspace",
        "--codex-helper",
        "sh",
        "--codex-helper-arg",
        helper.to_str().unwrap(),
        "--codex-helper-arg",
        projection.to_str().unwrap(),
    ];
    let first = result(
        &chain,
        &args,
        include_bytes!("../../../editchain-import/tests/fixtures/codex/rollout-contract.jsonl"),
        3,
    );
    assert_eq!(first.get("raw_ops"), Some(&json!(7)));
    assert_eq!(first.get("malformed"), Some(&json!(1)));
    let repeated = result(
        &chain,
        &args,
        include_bytes!("../../../editchain-import/tests/fixtures/codex/rollout-contract.jsonl"),
        0,
    );
    assert_eq!(repeated.get("written"), Some(&json!(0)));
}

#[test]
fn export_retains_decodable_evidence_and_reports_unsupported_records() {
    use editchain_store::AppendLog as _;
    let temp = tempfile::tempdir().unwrap();
    let _engine = Engine::open(temp.path()).unwrap();
    append(
        temp.path(),
        &message(1, Payload::Inline(b"valid record".to_vec())),
    );
    let mut writer = editchain_store::SegmentStore::open(temp.path()).unwrap();
    writer.append_record(0, &[255]).unwrap();
    drop(writer);
    let archive = result(temp.path(), &["export"], b"", 4);
    let entries = archive.as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.get("type") == Some(&json!("operation"))),
        "supported evidence is still exported"
    );
    let summary = entries.last().unwrap().get("stats").unwrap();
    assert_eq!(summary.get("undecodable"), Some(&json!(1)));
}
