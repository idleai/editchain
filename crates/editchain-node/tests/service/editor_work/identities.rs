use super::{agent, batch, event, git, live_request, records, window};
use editchain_core::{human::HumanWorkKind, ContentId, NodeId, OpId, OpKind, ParentSet, Payload};
use editchain_node::Server;
use editchain_protocol::Request;
use editchain_store::{
    format::{encode_op, Page},
    BlobStore, CanonicalChain, SegmentStore,
};
use serde_json::{json, Value};
use std::path::Path;

const GUID: &str = "99999999-9999-4999-8999-999999999999";
const FIRST: &str = "11111111-1111-4111-8111-111111111111";
const SECOND: &str = "22222222-2222-4222-8222-222222222222";

fn identity(guid: &str, stream: &str) -> Value {
    json!({"kind":"unsigned","guid":guid,"stream":stream.repeat(24)})
}

fn observed(session: &str, sequence: u64, payload: Value) -> Value {
    let mut value = event(sequence, payload);
    value["session"] = json!(session);
    value["identity"] = identity(GUID, "a");
    // Restarted clocks deliberately disagree with admission order.
    value["time_ms"] = json!(if session == FIRST {
        60000 + sequence
    } else {
        sequence
    });
    value
}

fn start(session: &str) -> Value {
    observed(
        session,
        1,
        json!({"type":"tracking_started","activity_schema":3,"dwell_ms":2000,"vscode_version":"1.85.0"}),
    )
}

fn tab(session: &str, sequence: u64, kind: &str) -> Value {
    observed(
        session,
        sequence,
        json!({"type":kind,"editor":"tab","uri":"file:///ai.txt","path":"ai.txt"}),
    )
}

fn encoded(root: &Path) -> Vec<Vec<u8>> {
    CanonicalChain::read(&root.join(".editchain"))
        .expect("chain")
        .located_ops()
        .map(|(op, _)| encode_op(op).expect("encoding"))
        .collect()
}

#[test]
fn human_reload_keeps_its_lane_in_live_updates_and_retained_checkpoints() {
    for mode in ["OpenLive", "OpenLivePaged"] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let capture = |session| {
            live_request(
                &mut Server::new(),
                batch(root, vec![start(session), tab(session, 2, "editor_opened")]),
            )
        };
        let _recorded = capture(FIRST);
        let query = json!({mode:{"workspace_path":root,"chain_dir":".editchain"}});
        let mut history = Server::new();
        let opened = live_request(&mut history, query.clone());
        let before = window(&mut history, &opened["snapshot_id"]);
        let previous = before.iter().find(|row| row["author"] == "human").unwrap();
        let _recorded = capture(SECOND);
        let update = live_request(
            &mut history,
            json!({"SyncLive":{"epoch":opened["live"]["epoch"],"after_revision":0,"codex":null}}),
        );
        let snapshot = &update["deltas"].as_array().unwrap().last().unwrap()["snapshot_id"];
        let after = window(&mut history, snapshot);
        let next = after
            .iter()
            .find(|row| row["node_key"] != previous["node_key"])
            .unwrap();
        assert_eq!(next["group"], previous["group"]);
        assert_eq!(next["parents"], json!([previous["node_key"]]));
        assert_eq!(
            next["lane"], previous["lane"],
            "reload must continue its lane: {mode}"
        );
        assert_eq!(next["transitions"], json!([]));
        drop(history);

        let mut reopened = Server::new();
        let opened = live_request(&mut reopened, query);
        assert_eq!(opened["diagnostics"]["open_chain_records"], 0);
        let rows = window(&mut reopened, &opened["snapshot_id"]);
        assert!(rows.iter().all(|row| row["lane"] == previous["lane"]));
        let _recorded = capture(GUID);
        let update = live_request(
            &mut reopened,
            json!({"SyncLive":{"epoch":opened["live"]["epoch"],"after_revision":0,"codex":null}}),
        );
        let snapshot = &update["deltas"].as_array().unwrap().last().unwrap()["snapshot_id"];
        let rows = window(&mut reopened, snapshot);
        let newest = records(root)
            .into_iter()
            .find(|(_, work)| work.session == GUID)
            .unwrap()
            .0
            .id
            .to_string();
        assert!(rows.iter().any(|row| row["node_key"] == newest));
        assert!(rows.iter().all(|row| row["lane"] == previous["lane"]));
    }
}

#[test]
fn unsigned_identity_connects_reloads_and_interleaved_recorders_beside_agents() {
    let temporary = tempfile::tempdir().expect("workspace");
    let root = temporary.path();
    drop(git(root, &["init", "-q"]));
    drop(git(root, &["commit", "--allow-empty", "-qm", "baseline"]));
    let query = json!({"workspace_path":root,"chain_dir":".editchain"});
    let mut first = Server::new();
    let context = live_request(&mut first, json!({"GetEditorContext":query}));
    let agent_id = agent(
        root,
        OpId::new(NodeId(83), 0, 1),
        "base",
        "AI",
        &context["repositories"][0],
    );
    let begin = |session| {
        batch(
            root,
            vec![
                start(session),
                observed(
                    session,
                    2,
                    json!({"type":"workspace_context", "workspace_path":root,"observed_ms":1,"repositories":context["repositories"]}),
                ),
                tab(session, 3, "editor_opened"),
            ],
        )
    };
    let mut second = Server::new();
    let requests = [
        begin(FIRST),
        begin(SECOND),
        batch(root, vec![tab(FIRST, 4, "editor_closed")]),
        batch(root, vec![tab(SECOND, 4, "editor_closed")]),
    ];
    for (index, request) in requests.iter().enumerate() {
        let server = if index % 2 == 0 {
            &mut first
        } else {
            &mut second
        };
        assert!(
            live_request(server, request.clone())["accepted"]
                .as_u64()
                .expect("accepted")
                > 0
        );
    }
    let human = records(root);
    let order: Vec<_> = [(FIRST, 3), (SECOND, 3), (FIRST, 4), (SECOND, 4)]
        .iter()
        .map(|(session, sequence)| {
            human
                .iter()
                .find(|(_, work)| work.session == *session && work.source_event.seq == *sequence)
                .expect("work")
        })
        .collect();
    for (index, (op, work)) in order.iter().enumerate() {
        assert_eq!(op.actor, order[0].0.actor);
        assert_eq!(op.scope, order[0].0.scope);
        assert_eq!(
            work.identity.as_ref().expect("unsigned identity").guid,
            GUID
        );
        let previous = index.checked_sub(1).map_or(ParentSet::None, |previous| {
            ParentSet::One(order[previous].0.id)
        });
        assert_eq!(op.parents, previous);
    }
    let before = encoded(root);
    drop(first);
    drop(second);
    // Fresh reducers must use retained source parents, including reverse retries.
    for request in requests.iter().rev() {
        assert_eq!(
            live_request(&mut Server::new(), request.clone())["accepted"],
            0
        );
    }
    assert_eq!(encoded(root), before);
    for mode in ["Open", "OpenLive"] {
        let mut history = Server::new();
        let opened = live_request(&mut history, json!({mode:query}));
        let rows = window(&mut history, &opened["snapshot_id"]);
        for (index, (op, _)) in order.iter().enumerate() {
            let row = rows
                .iter()
                .find(|row| row["node_key"] == op.id.to_string())
                .expect("human row");
            if index > 0 {
                assert_eq!(row["parents"], json!([order[index - 1].0.id.to_string()]));
            } else {
                assert!(row["parents"][0]
                    .as_str()
                    .expect("Git anchor")
                    .starts_with("git:"));
            }
        }
        assert!(rows
            .iter()
            .any(|row| row["node_key"] == agent_id.to_string()));
    }
}

#[test]
fn different_unsigned_people_and_workspace_bindings_keep_separate_branches() {
    let temporary = tempfile::tempdir().expect("workspace");
    let mut server = Server::new();
    for (session, attribution) in [
        (FIRST, identity(GUID, "a")),
        (SECOND, identity(SECOND, "a")),
        (GUID, identity(GUID, "b")),
    ] {
        let mut events = vec![start(session), tab(session, 2, "editor_opened")];
        for event in &mut events {
            event["identity"] = attribution.clone();
        }
        assert_eq!(
            live_request(&mut server, batch(temporary.path(), events))["accepted"],
            2
        );
    }
    let work = records(temporary.path());
    assert_eq!(work.len(), 3);
    assert!(work.iter().all(
        |(op, work)| op.parents == ParentSet::None && work.kind == HumanWorkKind::EditorOpened
    ));
    for pair in work.windows(2) {
        assert_ne!(pair[0].0.scope, pair[1].0.scope);
    }
}

#[test]
fn identity_retries_keep_admitted_parents_and_reject_mid_session_changes() {
    let temporary = tempfile::tempdir().expect("workspace");
    let root = temporary.path();
    let mut server = Server::new();
    let opened = tab(FIRST, 2, "editor_opened");
    let response = live_request(
        &mut server,
        batch(root, vec![start(FIRST), opened.clone(), opened.clone()]),
    );
    assert_eq!(response["accepted"], 2);
    assert_eq!(response["replayed"], 1);
    let before = encoded(root);
    for attribution in [identity(SECOND, "a"), identity(GUID, "b"), Value::Null] {
        let mut invalid = tab(FIRST, 3, "editor_closed");
        invalid["identity"] = attribution;
        let request = Request {
            id: 1,
            body: serde_json::from_value(batch(root, vec![invalid])).expect("request"),
        };
        assert!(server.handle(&request).is_err());
        assert_eq!(encoded(root), before);
    }
    let request = batch(root, vec![start(FIRST), opened]);
    assert_eq!(live_request(&mut Server::new(), request)["replayed"], 2);
    assert_eq!(encoded(root), before);
}

#[test]
fn unsigned_source_only_recovery_and_missing_payload_repair_preserve_exact_work() {
    let temporary = tempfile::tempdir().expect("workspace");
    let root = temporary.path();
    let events = vec![
        start(FIRST),
        tab(FIRST, 2, "editor_opened"),
        start(SECOND),
        tab(SECOND, 2, "editor_closed"),
    ];
    let request = batch(root, events);
    assert_eq!(
        live_request(&mut Server::new(), request.clone())["accepted"],
        4
    );
    let original = encoded(root);
    let directory = root.join(".editchain");
    let chain = CanonicalChain::read(&directory).expect("chain");
    let mut page = Page::new(0);
    let mut missing = None;
    for (op, _) in chain.located_ops().collect::<Vec<_>>().into_iter().rev() {
        if let OpKind::Import(import) = &op.kind {
            if let Payload::Blob(blob) = &import.raw_ref {
                let ContentId::Hash256(hash) = blob.id else {
                    panic!("capture uses content hashes")
                };
                missing = Some(hash);
                page.add_record(0, encode_op(op).expect("source encoding"));
            }
        } else if editchain_core::human::is_observation_marker(op) {
            page.add_record(0, encode_op(op).expect("marker encoding"));
        }
    }
    // Simulate a crash after durable source capture, before derived work.
    // Only this disposable fixture is modified.
    for entry in std::fs::read_dir(&directory).expect("segments") {
        let path = entry.expect("entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "eclog")
        {
            std::fs::remove_file(path).expect("replace fixture segments");
        }
    }
    SegmentStore::open(&directory)
        .expect("writer")
        .append_page(&page)
        .expect("raw page");
    let blobs = BlobStore::new(directory.join("blobs")).expect("blobs");
    std::fs::remove_file(blobs.path_for(&missing.expect("source payload")))
        .expect("missing fixture payload");
    assert_eq!(live_request(&mut Server::new(), request)["replayed"], 4);
    assert_eq!(encoded(root), original);
}

#[test]
fn recorded_names_label_cold_and_live_human_sessions_and_survive_replay() {
    let temporary = tempfile::tempdir().expect("workspace");
    let root = temporary.path();
    drop(git(root, &["init", "-q"]));
    drop(git(root, &["commit", "--allow-empty", "-qm", "baseline"]));
    let mut server = Server::new();
    let mut requests = Vec::new();
    for (session, name) in [
        (FIRST, Some("alice")),
        (SECOND, Some("Zoë <bob>")),
        (GUID, None),
    ] {
        let mut events = vec![start(session), tab(session, 2, "editor_opened")];
        for event in &mut events {
            event["identity"] = identity(session, "a");
            if let Some(name) = name {
                event["user_name"] = json!(name);
            }
        }
        let request = batch(root, events);
        assert_eq!(live_request(&mut server, request.clone())["accepted"], 2);
        requests.push(request);
    }
    let work = records(root);
    assert_eq!(work.len(), 3);
    for mode in ["Open", "OpenLive"] {
        let mut history = Server::new();
        let opened = live_request(
            &mut history,
            json!({mode:{"workspace_path":root,"chain_dir":".editchain"}}),
        );
        let rows = window(&mut history, &opened["snapshot_id"]);
        for (session, name) in [
            (FIRST, Some("alice")),
            (SECOND, Some("Zoë <bob>")),
            (GUID, None),
        ] {
            let (op, record) = work
                .iter()
                .find(|(_, record)| record.session == session)
                .expect("human activity");
            assert_eq!(record.user_name.as_deref(), name);
            let row = rows
                .iter()
                .find(|row| row["node_key"] == op.id.to_string())
                .expect("human row");
            assert_eq!(
                row["session_meta"]["session_title"],
                name.unwrap_or("VS Code")
            );
        }
    }
    let before = encoded(root);
    for request in requests.into_iter().rev() {
        assert_eq!(live_request(&mut Server::new(), request)["replayed"], 2);
    }
    assert_eq!(
        encoded(root),
        before,
        "retry retains original names and operation IDs"
    );
}

#[test]
fn invalid_display_names_are_rejected_before_capture() {
    let temporary = tempfile::tempdir().expect("workspace");
    for name in [
        String::new(),
        " padded ".into(),
        "line\nbreak".into(),
        "a".repeat(81),
    ] {
        let mut first = start(FIRST);
        first["user_name"] = json!(name);
        let request = Request {
            id: 1,
            body: serde_json::from_value(batch(temporary.path(), vec![first])).expect("request"),
        };
        let response = Server::new().handle(&request).expect("validation response");
        assert!(matches!(
            response.body,
            editchain_protocol::ResponseBody::Error(_)
        ));
        assert!(!temporary.path().join(".editchain").exists());
    }
}
