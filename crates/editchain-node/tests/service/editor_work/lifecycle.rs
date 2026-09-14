use super::{agent, batch, changed, document, event, git, live_request, records, start, window};
use editchain_core::{human::HumanWorkKind, NodeId, OpId, OpKind, ParentSet, Payload};
use editchain_node::Server;
use editchain_store::{
    format::{encode_op, Page},
    BlobStore, CanonicalChain, SegmentStore,
};
use serde_json::json;

fn copy_tree(source: &std::path::Path, destination: &std::path::Path) {
    std::fs::create_dir_all(destination).expect("copy directory");
    for entry in std::fs::read_dir(source).expect("source directory") {
        let entry = entry.expect("source entry");
        let output = destination.join(entry.file_name());
        if entry.file_type().expect("entry type").is_dir() {
            copy_tree(&entry.path(), &output);
        } else {
            let _bytes = std::fs::copy(entry.path(), output).expect("copy evidence");
        }
    }
}

#[test]
fn replay_after_relocation_uses_the_captured_workspace_context() {
    let original = tempfile::tempdir().expect("original workspace");
    let moved = tempfile::tempdir().expect("relocated workspace");
    let root = original.path();
    let input = vec![
        start(),
        event(
            2,
            json!({"type":"workspace_context","workspace_path":root,"observed_ms":2002,
        "repositories":[{"repository":"1","root":root,"head":"1111111111111111111111111111111111111111"}]}),
        ),
        event(
            3,
            json!({"type":"document_snapshot","document":document(1),"text":"before"}),
        ),
        changed(4, 2, "before", "after"),
        event(
            5,
            json!({"type":"human_edit","change":4,"signal":"keyboard_selection"}),
        ),
    ];
    assert_eq!(
        live_request(&mut Server::new(), batch(root, input.clone()))["accepted"],
        5
    );
    copy_tree(&root.join(".editchain"), &moved.path().join(".editchain"));
    assert_eq!(
        live_request(&mut Server::new(), batch(moved.path(), input))["replayed"],
        5
    );
    let original = records(root);
    let relocated = records(moved.path());
    assert_eq!(original.len(), 1);
    assert_eq!(relocated.len(), 1);
    assert!(relocated[0].1.git.is_some());
    assert_eq!(
        encode_op(&original[0].0).expect("source bytes"),
        encode_op(&relocated[0].0).expect("replayed bytes")
    );
}

#[test]
fn a_delayed_keyboard_indicator_uses_context_from_the_observed_edit() {
    let tmp = tempfile::tempdir().expect("workspace");
    let root = tmp.path();
    let context = |seq, head| {
        event(
            seq,
            json!({"type":"workspace_context","workspace_path":root,"observed_ms":seq,
        "repositories":[{"repository":"1","root":root,"head":head}]}),
        )
    };
    let older = "1111111111111111111111111111111111111111";
    let newer = "2222222222222222222222222222222222222222";
    let input = vec![
        start(),
        context(2, older),
        event(
            3,
            json!({"type":"document_snapshot","document":document(1),"text":"A"}),
        ),
        changed(4, 2, "A", "B"),
        context(5, newer),
        event(
            6,
            json!({"type":"human_edit","change":4,"signal":"keyboard_selection"}),
        ),
        changed(7, 3, "B", "C"),
        event(
            8,
            json!({"type":"human_edit","change":7,"signal":"keyboard_selection"}),
        ),
    ];
    assert_eq!(
        live_request(&mut Server::new(), batch(root, input))["accepted"],
        8
    );
    let human = records(root);
    assert_eq!(human.len(), 2);
    assert_eq!(
        human[0]
            .1
            .git
            .as_ref()
            .expect("captured context")
            .head
            .as_deref(),
        Some(older)
    );
    assert_eq!(human[0].1.context_observed_ms, Some(2));
    assert_eq!(
        human[1]
            .1
            .git
            .as_ref()
            .expect("new context")
            .head
            .as_deref(),
        Some(newer)
    );
    assert_ne!(human[0].1.turn, human[1].1.turn);
    assert_eq!(human[1].0.parents, ParentSet::One(human[0].0.id));
}

#[test]
fn live_observation_rows_retire_when_derived_evidence_arrives_in_a_later_tail() {
    let source = tempfile::tempdir().expect("capture workspace");
    let target = tempfile::tempdir().expect("live workspace");
    let input = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":"before"}),
        ),
        changed(3, 2, "before", "after"),
        event(
            4,
            json!({"type":"human_edit","change":3,"signal":"keyboard_selection"}),
        ),
    ];
    assert_eq!(
        live_request(&mut Server::new(), batch(source.path(), input))["accepted"],
        4
    );
    let chain = CanonicalChain::read(&source.path().join(".editchain")).expect("source chain");
    let target_chain = target.path().join(".editchain");
    copy_tree(
        &source.path().join(".editchain/blobs"),
        &target_chain.join("blobs"),
    );
    let mut raw = Page::new(0);
    let mut derived = Page::new(0);
    for (op, _) in chain.located_ops() {
        let page = if matches!(&op.kind,OpKind::Import(import) if matches!(import.raw_ref,Payload::Blob(_)))
        {
            &mut raw
        } else {
            &mut derived
        };
        page.add_record(0, encode_op(op).expect("fixture record"));
    }
    SegmentStore::open(&target_chain)
        .expect("writer")
        .append_page(&raw)
        .expect("raw tail");
    let mut history = Server::new();
    let opened = live_request(
        &mut history,
        json!({"OpenLive":{"workspace_path":target.path(),"chain_dir":".editchain"}}),
    );
    assert!(!window(&mut history, &opened["snapshot_id"]).is_empty());
    SegmentStore::open(&target_chain)
        .expect("writer")
        .append_page(&derived)
        .expect("derived tail");
    let update = live_request(
        &mut history,
        json!({"SyncLive":{"epoch":opened["live"]["epoch"],"after_revision":0,"codex":null}}),
    );
    let snapshot = update["deltas"]
        .as_array()
        .expect("deltas")
        .last()
        .expect("delta")["snapshot_id"]
        .clone();
    let rows = window(&mut history, &snapshot);
    assert!(
        rows.iter()
            .filter(|row| row["is_subop"] != true)
            .all(|row| row["author"] == "human"),
        "late annotations must retire temporary raw rows: {rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["file_change"]["source"] == "human")
            .count(),
        1
    );

    // Replay every complete-record prefix of the actual production append
    // order, including a reader observing a marker before its source arrives.
    let prefixes = tempfile::tempdir().expect("prefix workspace");
    let prefix_chain = prefixes.path().join(".editchain");
    copy_tree(
        &source.path().join(".editchain/blobs"),
        &prefix_chain.join("blobs"),
    );
    let mut prefix_history = Server::new();
    let baseline = live_request(
        &mut prefix_history,
        json!({"OpenLive":{"workspace_path":prefixes.path(),"chain_dir":".editchain"}}),
    );
    let mut revision = json!(0);
    let mut physical: Vec<_> = chain.located_ops().collect();
    physical.sort_by_key(|(_, location)| {
        let location = location.as_ref().expect("physical location");
        (location.segment_seq, location.data_offset)
    });
    for (op, _) in physical {
        let mut page = Page::new(0);
        page.add_record(0, encode_op(op).expect("prefix operation"));
        SegmentStore::open(&prefix_chain)
            .expect("prefix writer")
            .append_page(&page)
            .expect("prefix append");
        let update = live_request(
            &mut prefix_history,
            json!({"SyncLive":{"epoch":baseline["live"]["epoch"],"after_revision":revision,"codex":null}}),
        );
        revision = update["revision"].clone();
        if let Some(delta) = update["deltas"].as_array().expect("prefix deltas").last() {
            let rows = window(&mut prefix_history, &delta["snapshot_id"]);
            assert!(
                rows.iter()
                    .all(|row| row["kind"] != "import" && row["is_system"] != true),
                "a complete-record prefix must not expose raw editor transport: {rows:?}"
            );
        }
    }
}

#[test]
fn independent_recorders_and_agents_keep_their_series_when_git_changes_outside_capture() {
    let tmp = tempfile::tempdir().expect("workspace");
    let root = tmp.path();
    drop(git(root, &["init", "-q"]));
    std::fs::write(root.join("ai.txt"), "base").expect("base file");
    drop(git(root, &["add", "ai.txt"]));
    drop(git(root, &["commit", "-qm", "base"]));
    let open = json!({"workspace_path":root,"chain_dir":".editchain"});
    let mut recorder = Server::new();
    let initial = live_request(&mut recorder, json!({"GetEditorContext":open}));
    let first_agent = agent(
        root,
        OpId::new(NodeId(81), 0, 1),
        "base",
        "agent one",
        &initial["repositories"][0],
    );
    let second_agent = agent(
        root,
        OpId::new(NodeId(82), 0, 1),
        "agent one",
        "agent two",
        &initial["repositories"][0],
    );
    let capture = vec![
        start(),
        event(
            2,
            json!({"type":"workspace_context","workspace_path":root,"observed_ms":1000,"repositories":initial["repositories"]}),
        ),
        event(
            3,
            json!({"type":"document_snapshot","document":document(1),"text":"agent two"}),
        ),
        changed(4, 2, "agent two", "human one"),
        event(
            5,
            json!({"type":"human_edit","change":4,"signal":"keyboard_selection"}),
        ),
    ];
    assert_eq!(
        live_request(&mut recorder, batch(root, capture.clone()))["accepted"],
        5
    );
    let mut other = capture;
    for event in &mut other {
        event["session"] = json!("22222222-2222-4222-8222-222222222222");
    }
    assert_eq!(
        live_request(&mut Server::new(), batch(root, other))["accepted"],
        5
    );

    // A terminal commit happens without an editor-context observation.
    drop(git(root, &["add", "ai.txt"]));
    drop(git(root, &["commit", "-qm", "external commit"]));
    let current = live_request(&mut recorder, json!({"GetEditorContext":open}));
    assert_ne!(
        current["repositories"][0]["head"],
        initial["repositories"][0]["head"]
    );
    assert_eq!(
        live_request(
            &mut recorder,
            batch(
                root,
                vec![
                    changed(6, 3, "human one", "before poll"),
                    event(
                        7,
                        json!({"type":"human_edit","change":6,"signal":"keyboard_selection"})
                    ),
                    event(
                        8,
                        json!({"type":"workspace_context","workspace_path":root,"observed_ms":2008,"repositories":current["repositories"]})
                    ),
                    changed(9, 4, "before poll", "after poll"),
                    event(
                        10,
                        json!({"type":"human_edit","change":9,"signal":"keyboard_selection"})
                    )
                ]
            )
        )["accepted"],
        5
    );
    let human = records(root);
    assert_eq!(human.len(), 4);
    assert_eq!(
        human[0].1.git, human[1].1.git,
        "query-time HEAD cannot rewrite earlier context"
    );
    assert_eq!(human[0].1.turn, human[1].1.turn);
    assert_ne!(human[1].1.git, human[2].1.git);
    assert_ne!(
        human[1].1.turn, human[2].1.turn,
        "observed HEAD change starts an episode"
    );
    assert_eq!(human[2].0.parents, ParentSet::One(human[1].0.id));
    assert_eq!(
        human[3].0.parents,
        ParentSet::None,
        "a second VS Code window has its own series"
    );
    assert_eq!(human[3].1.git, human[0].1.git);
    let _prepared =
        editchain_node::history::prepare_live_checkpoint(root, &root.join(".editchain"))
            .expect("prepare native paged view");
    let mut history = Server::new();
    let opened = live_request(&mut history, json!({"OpenLivePaged":open}));
    let rows = window(&mut history, &opened["snapshot_id"]);
    let original_git = format!(
        "git:{}:{}",
        initial["repositories"][0]["repository"]
            .as_str()
            .expect("repository"),
        initial["repositories"][0]["head"].as_str().expect("head")
    );
    for id in [human[0].0.id, human[3].0.id, first_agent, second_agent] {
        let row = rows
            .iter()
            .find(|row| row["node_key"] == id.to_string())
            .expect("independent work row");
        assert_eq!(
            row["parents"],
            json!([original_git]),
            "all four series branch from the observed Git state"
        );
    }
    let latest = rows
        .iter()
        .find(|row| row["node_key"] == human[2].0.id.to_string())
        .expect("latest human work");
    assert_eq!(
        latest["parents"].as_array().expect("parents").len(),
        2,
        "new baseline retains the previous human fragment as well"
    );
    assert!(
        rows.iter()
            .all(|row| row["kind"] != "import" || row["author"] != "human"),
        "raw observation envelopes stay in Trace"
    );
}

#[test]
fn legacy_raw_capture_backfills_after_payload_repair_without_changing_source_bytes() {
    let tmp = tempfile::tempdir().expect("workspace");
    let root = tmp.path();
    let input = vec![
        start(),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":"before"}),
        ),
        changed(3, 2, "before", "after"),
        event(
            4,
            json!({"type":"human_edit","change":3,"signal":"keyboard_selection"}),
        ),
    ];
    assert_eq!(
        live_request(&mut Server::new(), batch(root, input.clone()))["accepted"],
        4
    );
    let chain_path = root.join(".editchain");
    let chain = CanonicalChain::read(&chain_path).expect("capture chain");
    let mut original: Vec<_> = chain
        .located_ops()
        .map(|(op, _)| (op.id, encode_op(op).expect("encoding")))
        .collect();
    original.sort_by_key(|(id, _)| *id);
    // A disposable pre-integration chain contains only the unchanged raw Imports.
    let mut legacy = Page::new(0);
    for (op, _) in chain.located_ops() {
        if let OpKind::Import(import) = &op.kind {
            if let Payload::Blob(blob) = &import.raw_ref {
                legacy.add_record(0, encode_op(op).expect("raw encoding"));
                if op.id.seq == 3 {
                    let editchain_core::ContentId::Hash256(hash) = blob.id else {
                        panic!("hashed payload")
                    };
                    let blobs = BlobStore::new(chain_path.join("blobs")).expect("blobs");
                    std::fs::remove_file(blobs.path_for(&hash)).expect("missing old payload");
                }
            }
        }
    }
    for entry in std::fs::read_dir(&chain_path).expect("segments") {
        let path = entry.expect("entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "eclog")
        {
            std::fs::remove_file(path).expect("replace fixture segments");
        }
    }
    SegmentStore::open(&chain_path)
        .expect("fixture writer")
        .append_page(&legacy)
        .expect("legacy page");
    assert_eq!(
        live_request(&mut Server::new(), batch(root, input))["replayed"],
        4
    );
    let restored = CanonicalChain::read(&chain_path).expect("backfilled chain");
    let mut encoded: Vec<_> = restored
        .located_ops()
        .map(|(op, _)| (op.id, encode_op(op).expect("encoding")))
        .collect();
    encoded.sort_by_key(|(id, _)| *id);
    assert_eq!(
        encoded, original,
        "backfill and a fresh capture derive the same canonical bytes"
    );
    assert_eq!(records(root)[0].1.kind, HumanWorkKind::Edit);
}
