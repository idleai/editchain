use super::*;
use editchain_protocol::{ExpansionSpanDto, FileChangeSource, SubOpSummary};
use serde_json::json;

fn capture(root: &std::path::Path) {
    let document =
        |version| json!({"id":"buffer", "uri":"file:///a.rs", "path":"a.rs", "version":version});
    let mut events = vec![
        json!({"type":"tracking_started", "dwell_ms":2000, "vscode_version":"1.85.0"}),
        json!({"type":"document_snapshot", "document":document(1), "text":"0\n"}),
    ];
    for (version, before, after) in [(2u64, "0\n", "1\n"), (3, "1\n", "2\n"), (4, "2\n", "3\n")] {
        events.push(
            json!({"type":"document_changed", "document":document(version),
            "before_version":version.saturating_sub(1), "before":before, "after":after,
            "changes":[{"offset":0, "length":1, "text":after.trim_end()}], "reason":null}),
        );
        events.push(
            json!({"type":"human_edit", "change":events.len(), "signal":"keyboard_selection"}),
        );
    }
    let events: Vec<_> = events
        .into_iter()
        .enumerate()
        .map(|(i, event)| {
            json!({
                "schema":1, "session":"11111111-1111-4111-8111-111111111111",
                "sequence":i.saturating_add(1), "time_ms":i.saturating_add(2000), "event":event
            })
        })
        .collect();
    let result = crate::Server::new().handle(&serde_json::from_value(json!({"id":1,"body":{
        "RecordEditorEvents":{"workspace_path":root, "chain_dir":".editchain", "events":events}
    }})).unwrap()).unwrap();
    assert!(matches!(result.body, ResponseBody::Ok(_)));
}

fn block(workspace: &LiveWorkspace, key: &str) -> StoredBlock {
    workspace
        .blocks
        .get(workspace.orders.get(key).unwrap())
        .unwrap()
        .clone()
}

fn legacy_edits(workspace: &mut LiveWorkspace) -> Vec<String> {
    let keys: Vec<_> = workspace
        .inputs
        .iter()
        .filter(|(_, input)| {
            input
                .operations
                .iter()
                .any(|op| matches!(op.kind, editchain_core::OpKind::File(_)))
        })
        .map(|(key, _)| key.clone())
        .collect();
    let mut replacements = Vec::new();
    for key in &keys {
        let mut old = workspace.load_block(&block(workspace, key)).unwrap();
        let row = old.rows.first_mut().unwrap();
        let change = row.file_change.take().unwrap();
        if change.source == FileChangeSource::Agent {
            row.kind = "import".into();
        }
        let mut child = row.clone();
        child.op_id.clone_from(&change.op_id);
        child.node_key = format!("{}::child:0", row.node_key);
        child.continuity_key = format!("{key}:file:{}", change.path);
        child.is_subop = true;
        child.parent_row = Some(0);
        child.hierarchy_depth = 1;
        child.parents.clear();
        child.author.clear();
        child.task_group = None;
        child.summary.clone_from(&change.path);
        child.file_change = Some(change);
        row.sub_ops = vec![SubOpSummary {
            op_id: child.op_id.clone().unwrap(),
            summary: child.summary.clone(),
            kind: "file".into(),
            timestamp_ms: row.timestamp_ms,
        }];
        old.rows.push(child);
        old.meta.row_count = 2;
        old.meta.spans = vec![ExpansionSpanDto {
            row: 0,
            descendant_count: 1,
        }];
        replacements.push(workspace.rows.put(old).unwrap());
    }
    drop(workspace.connect(&[], replacements).unwrap());
    keys
}

fn canonical(root: &std::path::Path) -> Vec<Vec<u8>> {
    editchain_store::CanonicalChain::read(&root.join(".editchain"))
        .unwrap()
        .located_ops()
        .map(|(op, _)| editchain_store::format::encode_op(op).unwrap())
        .collect()
}

#[test]
fn version_five_compacts_retained_edits_preserving_graph_diffs_and_explicit_episode_state() {
    for expanded in [false, true] {
        let root = tempfile::tempdir().unwrap();
        capture(root.path());
        let canonical_before = canonical(root.path());
        let mut workspace = fixture(root.path()).unwrap();
        let keys = legacy_edits(&mut workspace);
        assert_eq!(keys.len(), 3);
        let anchor = window(&mut workspace)
            .unwrap()
            .rows
            .into_iter()
            .find(|row| row.author == "human" && row.task_group.is_some())
            .unwrap()
            .continuity_key;
        workspace.toggle_disclosure(&anchor, true).unwrap();
        if !expanded {
            workspace.toggle_disclosure(&anchor, true).unwrap();
        }
        let before = window(&mut workspace).unwrap();
        let mut saved = workspace.saved();
        saved.version = 5;
        workspace.rows.flush().unwrap();
        drop(
            workspace
                .checkpoint_store
                .commit::<_, Saved>(&saved)
                .unwrap(),
        );
        drop(workspace);

        let mut resumed = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        assert!(resumed.reused_checkpoint);
        let after = window(&mut resumed).unwrap();
        assert_eq!(before.total, after.total, "no extra visible wrapper row");
        for (before, after) in before.rows.iter().zip(&after.rows) {
            assert_eq!(
                (
                    &before.node_key,
                    &before.parents,
                    before.lane,
                    &before.group
                ),
                (&after.node_key, &after.parents, after.lane, &after.group)
            );
            assert_eq!(
                serde_json::to_value(&before.task_group).unwrap(),
                serde_json::to_value(&after.task_group).unwrap()
            );
        }
        for key in &keys {
            let compact = resumed.load_block(&block(&resumed, key)).unwrap();
            assert_eq!(compact.meta.row_count, 1);
            assert!(compact.meta.spans.is_empty());
            let row = compact.rows.first().unwrap();
            assert!(!row.is_subop);
            assert!(row.sub_ops.is_empty());
            let change = row.file_change.as_ref().unwrap();
            assert_eq!(change.source, FileChangeSource::Human);
            let result = resumed
                .handle(
                    &serde_json::from_value(json!({"GetFileDiff":{
                        "snapshot_id":resumed.snapshot_id, "change":change
                    }}))
                    .unwrap(),
                )
                .unwrap();
            let diff = match result {
                ResponseBody::Ok(diff) => Some(diff),
                ResponseBody::Error(_) => None,
            }
            .expect("retained diff");
            assert!(matches!(
                (
                    diff.get("before").and_then(serde_json::Value::as_str),
                    diff.get("after").and_then(serde_json::Value::as_str)
                ),
                (Some("0\n"), Some("1\n"))
                    | (Some("1\n"), Some("2\n"))
                    | (Some("2\n"), Some("3\n"))
            ));
        }
        assert_eq!(
            block(&resumed, "item:4").meta.row_count,
            2,
            "agent details retained"
        );
        assert_eq!(
            load(&resumed.checkpoint_store).unwrap().unwrap().version,
            VERSION
        );
        drop(resumed);
        let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        let twice = window(&mut reopened).unwrap();
        assert_eq!(
            serde_json::to_value(after.rows).unwrap(),
            serde_json::to_value(twice.rows).unwrap()
        );
        assert_eq!(
            canonical(root.path()),
            canonical_before,
            "canonical records remain byte-for-byte identical"
        );
    }
}

fn capture_agent(root: &std::path::Path) {
    use editchain_core::{
        ActorId, Clock, FileEdit, FileOp, FileStage, ImportOp, OpKind, ParentSet, Payload,
        ScopeRef, SessionId, Tags,
    };
    let raw = Op {
        id: OpId::new(NodeId(73), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(73),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::Session(SessionId(73)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                    "type":"FileChange","id":"edit","status":"completed",
                    "changes":{"a.rs":{"type":"add","content":"AI\n"}}
                }}})
                .to_string()
                .into_bytes(),
            ),
            raw_hash: None,
        }),
    };
    let file = Op {
        id: OpId::new(NodeId(73), 0, 2),
        parents: ParentSet::One(raw.id),
        tags: Tags::AGENT | Tags::FILE,
        kind: OpKind::File(FileOp {
            path: editchain_import::derive_path_id("a.rs"),
            stage: FileStage::Applied,
            base: None,
            after: None,
            edit: FileEdit::None,
        }),
        ..raw.clone()
    };
    let mut page = editchain_store::format::Page::new(0);
    for op in [raw, file] {
        page.add_record(0, editchain_store::format::encode_op(&op).unwrap());
    }
    editchain_store::SegmentStore::open(root.join(".editchain"))
        .unwrap()
        .append_page(&page)
        .unwrap();
}

#[test]
fn version_six_replaces_import_wrappers_with_single_changes_without_rewriting_history() {
    let root = tempfile::tempdir().unwrap();
    capture_agent(root.path());
    let original = canonical(root.path());
    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    let keys = legacy_edits(&mut workspace);
    let key = keys.first().unwrap();
    let old = window(&mut workspace)
        .unwrap()
        .rows
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(old.kind, "import");
    workspace.rows.flush().unwrap();
    let mut saved = workspace.saved();
    saved.version = 6;
    drop(
        workspace
            .checkpoint_store
            .commit::<_, Saved>(&saved)
            .unwrap(),
    );
    drop(workspace);

    let mut resumed = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert!(resumed.reused_checkpoint);
    let current = window(&mut resumed).unwrap();
    let row = current.rows.first().unwrap();
    assert_eq!(current.rows.len(), 1);
    assert_eq!(row.kind, "file");
    assert_eq!(row.node_key, old.node_key);
    assert_eq!(row.continuity_key, old.continuity_key);
    assert_eq!(row.parents, old.parents);
    assert_eq!(row.lane, old.lane);
    assert!(row.sub_ops.is_empty());
    assert_eq!(block(&resumed, key).meta.row_count, 1);
    assert_eq!(row.file_change.as_ref().unwrap().path, "a.rs");
    let diff = resumed
        .handle(
            &serde_json::from_value(json!({"GetFileDiff":{
                "snapshot_id":resumed.snapshot_id,"change":row.file_change
            }}))
            .unwrap(),
        )
        .unwrap();
    assert!(matches!(diff, ResponseBody::Ok(_)));
    drop(resumed);
    let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(
        serde_json::to_value(window(&mut reopened).unwrap().rows).unwrap(),
        serde_json::to_value(current.rows).unwrap()
    );
    assert_eq!(canonical(root.path()), original);
}
