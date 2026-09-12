use super::{batch, document, event, live_request, start};
use editchain_core::{
    human::{HumanWorkKind, HumanWorkRecord},
    ActorId, Clock, ContentId, FileEdit, FileOp, FileStage, GitLink, GitLinkKind, GitOid, ImportOp,
    NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind, ParentSet, Payload, RepositoryId, ScopeRef,
    SessionId, Tags,
};
use editchain_node::Server;
use editchain_project::human::work_record;
use editchain_store::{
    format::{encode_op, Page},
    BlobStore, CanonicalChain, SegmentStore,
};
use serde_json::{json, Value};
use std::{path::Path, process::Command};

#[path = "lifecycle.rs"]
mod lifecycle;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
        .env("GIT_AUTHOR_DATE", "1970-01-01T00:00:01+0000")
        .env("GIT_COMMITTER_DATE", "1970-01-01T00:00:01+0000")
        .output()
        .expect("valid graph fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("valid graph fixture")
        .trim()
        .to_owned()
}

fn changed(seq: u64, version: u64, before: &str, after: &str) -> Value {
    event(
        seq,
        json!({"type":"document_changed", "document":document(version),
        "before_version":version-1,"before":before,"after":after,
        "changes":[{"offset":0,"length":before.encode_utf16().count(),"text":after}],"reason":null}),
    )
}

fn records(root: &Path) -> Vec<(Op, HumanWorkRecord)> {
    let chain = CanonicalChain::read(&root.join(".editchain")).expect("valid graph fixture");
    let mut records: Vec<_> = chain
        .located_ops()
        .filter_map(|(op, _)| work_record(op).map(|record| (op.clone(), record)))
        .collect();
    records.sort_by_key(|(_, record)| (record.session.clone(), record.source_event.seq));
    records
}

fn window(server: &mut Server, snapshot: &Value) -> Vec<Value> {
    live_request(
        server,
        json!({"GetWindow":{"snapshot_id":snapshot,"offset":0,"limit":1000,"include_layout":true}}),
    )["rows"]
        .as_array()
        .expect("valid graph fixture")
        .clone()
}

fn agent(root: &Path, raw_id: OpId, before: &str, after: &str, context: &Value) -> OpId {
    let actor = raw_id.node.0;
    let seq = raw_id.seq;
    let file_id = OpId {
        seq: seq + 1,
        ..raw_id
    };
    let mut blobs = BlobStore::new(root.join(".editchain/blobs")).expect("valid graph fixture");
    blobs.write(before.as_bytes()).expect("valid graph fixture");
    blobs.write(after.as_bytes()).expect("valid graph fixture");
    let base = ContentId::Hash256(*blake3::hash(before.as_bytes()).as_bytes());
    let after_id = ContentId::Hash256(*blake3::hash(after.as_bytes()).as_bytes());
    let raw = Op {
        id: raw_id,
        parents: if seq == 1 {
            ParentSet::None
        } else {
            ParentSet::One(OpId {
                seq: seq - 4,
                ..raw_id
            })
        },
        actor: ActorId(actor),
        clock: Clock::UnixMs(if seq == 1 { 1001 } else { 2001 + seq }),
        scope: ScopeRef::Session(SessionId(actor)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(b"{\"type\":\"agent_revision\"}".to_vec()),
            raw_hash: None,
        }),
    };
    let file = Op {
        id: file_id,
        parents: ParentSet::One(raw_id),
        tags: Tags::FILE | Tags::AGENT,
        kind: OpKind::File(FileOp {
            path: editchain_import::derive_path_id("ai.txt"),
            stage: FileStage::Applied,
            base: Some(base),
            after: Some(after_id),
            edit: FileEdit::None,
        }),
        ..raw.clone()
    };
    let note = Op {
        id: OpId {
            seq: seq + 2,
            ..raw_id
        },
        parents: ParentSet::One(raw_id),
        tags: Tags::NOTE,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![file_id],
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(b"ai.txt".to_vec()),
        }),
        ..raw.clone()
    };
    let mut ops = vec![raw.clone(), file, note];
    if seq == 1 {
        ops.push(Op {
            id: OpId {
                seq: seq + 3,
                ..raw_id
            },
            parents: ParentSet::One(raw_id),
            tags: Tags::META,
            kind: OpKind::GitLink(GitLink {
                source: raw_id,
                target_repo: RepositoryId(
                    context["repository"]
                        .as_str()
                        .expect("valid graph fixture")
                        .parse()
                        .expect("valid graph fixture"),
                ),
                target_oid: GitOid::from_hex(
                    context["head"].as_str().expect("valid graph fixture"),
                )
                .expect("valid graph fixture"),
                kind: GitLinkKind::BasedOn,
            }),
            ..raw
        });
    }
    let mut page = Page::new(0);
    for op in &ops {
        page.add_record(0, encode_op(op).expect("valid graph fixture"));
    }
    SegmentStore::open(root.join(".editchain"))
        .expect("valid graph fixture")
        .append_page(&page)
        .expect("valid graph fixture");
    std::fs::write(root.join("ai.txt"), after).expect("valid graph fixture");
    raw_id
}

#[test]
fn human_and_agent_series_share_git_but_keep_exact_intermediate_edits() {
    let tmp = tempfile::tempdir().expect("valid graph fixture");
    let root = tmp.path();
    drop(git(root, &["init", "-q"]));
    std::fs::write(root.join("ai.txt"), "base\n").expect("valid graph fixture");
    drop(git(root, &["add", "ai.txt"]));
    drop(git(root, &["commit", "-qm", "base"]));
    let mut capture = Server::new();
    let context = live_request(
        &mut capture,
        json!({"GetEditorContext":{"workspace_path":root,"chain_dir":".editchain"}}),
    );
    let repo = &context["repositories"][0];
    let first_agent = agent(
        root,
        OpId::new(NodeId(73), 0, 1),
        "base\n",
        "base\nai\n",
        repo,
    );
    let first = vec![
        start(),
        event(
            2,
            json!({"type":"workspace_context","workspace_path":root,"observed_ms":2000,"repositories":context["repositories"]}),
        ),
        event(
            3,
            json!({"type":"document_snapshot","document":document(1),"text":"base\nai\n"}),
        ),
        changed(4, 2, "base\nai\n", "base\nhuman\n"),
        event(
            5,
            json!({"type":"human_edit","change":4,"signal":"keyboard_selection"}),
        ),
    ];
    assert_eq!(
        live_request(&mut capture, batch(root, first))["accepted"],
        5
    );
    let open = json!({"workspace_path":root,"chain_dir":".editchain"});
    let mut history = Server::new();
    let opened = live_request(&mut history, json!({"OpenLive":open}));
    let second_agent = agent(
        root,
        OpId::new(NodeId(73), 0, 5),
        "base\nhuman\n",
        "base\nhuman\nai two\n",
        repo,
    );
    let mut second = vec![
        changed(6, 3, "base\nhuman\n", "base\nhuman\nai two\n"),
        event(
            7,
            json!({"type":"code_exposure","document":document(3),"editor":"one","ranges":[{"start":[0,0],"end":[3,0]}],"started_ms":2006,"duration_ms":3000}),
        ),
        changed(8, 4, "base\nhuman\nai two\n", "base\nhuman\nhuman two\n"),
        event(
            9,
            json!({"type":"human_edit","change":8,"signal":"keyboard_selection"}),
        ),
    ];
    second[3]["time_ms"] = json!(40000);
    assert_eq!(
        live_request(&mut capture, batch(root, second.clone()))["accepted"],
        4
    );
    std::fs::write(root.join("ai.txt"), "base\nhuman\nhuman two\n").expect("valid graph fixture");
    let update = live_request(
        &mut history,
        json!({"SyncLive":{"epoch":opened["live"]["epoch"],"after_revision":0,"codex":null}}),
    );
    let snapshot = update["deltas"]
        .as_array()
        .expect("valid graph fixture")
        .last()
        .expect("valid graph fixture")["snapshot_id"]
        .clone();
    let rows = window(&mut history, &snapshot);
    let human = records(root);
    assert_eq!(human.len(), 3);
    assert_eq!(
        human.iter().map(|(_, r)| r.kind).collect::<Vec<_>>(),
        [
            HumanWorkKind::Edit,
            HumanWorkKind::Read,
            HumanWorkKind::Edit
        ]
    );
    assert_eq!(human[0].1.turn, human[1].1.turn);
    assert_ne!(human[1].1.turn, human[2].1.turn);
    assert_eq!(human[1].0.parents, ParentSet::One(human[0].0.id));
    assert_eq!(human[2].0.parents, ParentSet::One(human[1].0.id));
    let git_key = format!(
        "git:{}:{}",
        repo["repository"].as_str().expect("valid graph fixture"),
        repo["head"].as_str().expect("valid graph fixture")
    );
    let first_row = rows
        .iter()
        .find(|row| row["node_key"] == human[0].0.id.to_string())
        .expect("valid graph fixture");
    assert_eq!(first_row["author"], "human");
    assert!(
        first_row["parents"]
            .as_array()
            .expect("valid graph fixture")
            .contains(&json!(git_key)),
        "expected {git_key}; row: {first_row}"
    );
    let agent_row = rows
        .iter()
        .find(|row| row["node_key"] == second_agent.to_string())
        .expect("valid graph fixture");
    assert_eq!(agent_row["parents"], json!([first_agent.to_string()]));
    let files: Vec<_> = rows
        .iter()
        .filter(|row| row["file_change"]["source"] == "human")
        .collect();
    assert_eq!(files.len(), 2);
    // A click can retain the displayed revision while another writer advances
    // the live view. Reject that revision without poisoning subsequent reads.
    let stale = history
        .handle(
            &serde_json::from_value(json!({"id":99,"body":{"GetFileDiff":{
                "snapshot_id":opened["snapshot_id"],"change":files[0]["file_change"]
            }}}))
            .expect("valid stale diff request"),
        )
        .expect_err("old live snapshot must be rejected");
    assert_eq!(
        editchain_protocol::ServiceError::from_error(stale.as_ref()).code,
        editchain_protocol::ErrorCode::StaleSnapshot
    );
    let mut sides = Vec::new();
    for row in &files {
        let result = live_request(
            &mut history,
            json!({"GetFileDiff":{"snapshot_id":snapshot,"change":row["file_change"]}}),
        );
        sides.push((
            result["before"]
                .as_str()
                .expect("valid graph fixture")
                .to_owned(),
            result["after"]
                .as_str()
                .expect("valid graph fixture")
                .to_owned(),
        ));
    }
    assert!(sides.contains(&("base\nai\n".into(), "base\nhuman\n".into())));
    assert!(sides.contains(&(
        "base\nhuman\nai two\n".into(),
        "base\nhuman\nhuman two\n".into()
    )));
    let report = live_request(&mut capture, json!({"GetHumanWork":open}));
    assert_eq!(
        report["historical_ai_lines"], 2,
        "human FileOps cannot become AI evidence"
    );
    assert_eq!(report["historical_ai_lines_edited"], 2);
    let before = CanonicalChain::read(&root.join(".editchain"))
        .expect("valid graph fixture")
        .stats()
        .accepted;
    assert_eq!(
        live_request(&mut Server::new(), batch(root, second))["replayed"],
        4
    );
    assert_eq!(
        CanonicalChain::read(&root.join(".editchain"))
            .expect("valid graph fixture")
            .stats()
            .accepted,
        before,
        "restart and retry do not duplicate work"
    );
    let idle = live_request(
        &mut history,
        json!({"SyncLive":{"epoch":opened["live"]["epoch"],"after_revision":update["revision"],"codex":null}}),
    );
    assert_eq!(idle["deltas"], json!([]));
    assert_eq!(idle["work"]["chain_records"], 0);
    let mut offline = Server::new();
    let reopened = live_request(&mut offline, json!({"Open":open}));
    let offline_rows = window(&mut offline, &reopened["snapshot_id"]);
    assert!(offline_rows.iter().any(|row| row["author"] == "human"));
    for (index, (op, _)) in human.iter().enumerate() {
        let row = offline_rows
            .iter()
            .find(|row| row["node_key"] == op.id.to_string())
            .expect("physical human row in static history");
        let expected = if index == 0 {
            git_key.clone()
        } else {
            human[index - 1].0.id.to_string()
        };
        assert_eq!(
            row["parents"],
            json!([expected]),
            "static history keeps the same human series"
        );
    }
    assert_eq!(
        offline_rows
            .iter()
            .filter(|row| row["file_change"]["source"] == "human")
            .count(),
        2
    );
}

#[test]
fn equal_contents_keep_distinct_occurrences_and_batching_does_not_change_derivation() {
    let all = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":"X"}),
        ),
        changed(3, 2, "X", "Y"),
        event(
            4,
            json!({"type":"human_edit","change":3,"signal":"keyboard_selection"}),
        ),
        changed(5, 3, "Y", "X"),
        event(6, json!({"type":"human_edit","change":5,"signal":"undo"})),
    ];
    let one = tempfile::tempdir().expect("valid graph fixture");
    let split = tempfile::tempdir().expect("valid graph fixture");
    assert_eq!(
        live_request(&mut Server::new(), batch(one.path(), all.clone()))["accepted"],
        6
    );
    for event in all {
        assert_eq!(
            live_request(&mut Server::new(), batch(split.path(), vec![event]))["accepted"],
            1
        );
    }
    let records = records(one.path());
    assert_eq!(records.len(), 2);
    let initial = records[0].1.before.as_ref().expect("valid graph fixture");
    let final_revision = records[1].1.after.as_ref().expect("valid graph fixture");
    assert_eq!(initial.content, final_revision.content);
    assert_ne!(initial.occurrence, final_revision.occurrence);
    assert_eq!(
        records[0]
            .1
            .after
            .as_ref()
            .expect("valid graph fixture")
            .occurrence,
        records[1]
            .1
            .before
            .as_ref()
            .expect("valid graph fixture")
            .occurrence
    );
    assert!(
        records.iter().all(|(_, record)| record.git.is_none()),
        "missing recorded context stays unknown"
    );
    let bytes = |root: &Path| {
        let chain = CanonicalChain::read(&root.join(".editchain")).expect("valid graph fixture");
        let mut result: Vec<_> = chain
            .located_ops()
            .map(|(op, _)| (op.id, encode_op(op).expect("valid graph fixture")))
            .collect();
        result.sort_by_key(|(id, _)| *id);
        result
    };
    assert_eq!(bytes(one.path()), bytes(split.path()));
}
