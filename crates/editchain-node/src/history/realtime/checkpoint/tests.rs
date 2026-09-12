use super::*;
use editchain_core::NodeId;
use editchain_project::live::TaskIdentity;
use editchain_protocol::{
    GetWindowRequest, HistoryWindow, LiveBlock, OpenRequest, RequestBody, ResponseBody,
};
use std::cmp::Reverse;

fn request(root: &std::path::Path) -> OpenRequest {
    OpenRequest {
        workspace_path: root.to_string_lossy().into_owned(),
        chain_dir: ".editchain".into(),
    }
}

fn stage(
    workspace: &mut LiveWorkspace,
    sequence: u64,
    parent: Option<&str>,
    details: bool,
) -> Result<StoredBlock> {
    let key = format!("item:{sequence}");
    let node = format!("1:0:{sequence}");
    let input = LiveRow {
        key: key.clone(),
        anchor: OpId::new(NodeId(1), 0, sequence),
        incarnation: OpId::new(NodeId(1), 0, sequence),
        operations: Vec::new(),
        task: Some(TaskIdentity {
            key: "native-task".into(),
            thread: "thread".into(),
            turn: "turn".into(),
            boundary: OpId::new(NodeId(1), 0, 0),
        }),
    };
    drop(workspace.inputs.insert(key.clone(), input));
    let mut block: LiveBlock = serde_json::from_value(serde_json::json!({
        "meta":{"key":key, "node_key":node, "sort_time":sequence, "parents":parent.into_iter().collect::<Vec<_>>(), "row_count":1, "spans":[]},
        "rows":[{"node_key":node,"op_id":node,"continuity_key":key,"summary":format!("Physical activity {sequence}"),
            "kind":"message","timestamp_ms":sequence,"group":"session","parents":[],"is_submodule":false}]}))?;
    if details {
        block.rows.push(serde_json::from_value(serde_json::json!({"node_key":"detail", "continuity_key":"detail",
            "summary":"Original output details", "timestamp_ms":sequence,"group":"session","parents":[],
            "is_submodule":false,"is_subop":true,"parent_row":0}))?);
        block.meta.row_count = 2;
        block.meta.spans.push(editchain_protocol::ExpansionSpanDto {
            row: 0,
            descendant_count: 1,
        });
    }
    let block = workspace.rows.put(block)?;
    drop(workspace.orders.insert(key, block.meta.order()));
    drop(workspace.blocks.insert(
        block.meta.order(),
        block.clone(),
        editchain_protocol::rank::Measure {
            expanded: block.meta.row_count,
            visible: 1,
        },
    ));
    Ok(block)
}

fn fixture(root: &std::path::Path) -> Result<LiveWorkspace> {
    let mut workspace = LiveWorkspace::open_paged(&request(root))?;
    let mut blocks = Vec::new();
    for index in 0u64..5 {
        let parent = index.checked_sub(1).map(|index| format!("item:{index}"));
        blocks.push(stage(&mut workspace, index, parent.as_deref(), index == 4)?);
    }
    drop(workspace.connect(&[], blocks)?);
    workspace.checkpoint()?;
    Ok(workspace)
}

fn window(workspace: &mut LiveWorkspace) -> Result<HistoryWindow> {
    match workspace.handle(&RequestBody::GetWindow(GetWindowRequest {
        snapshot_id: workspace.snapshot_id.clone(),
        offset: 0,
        limit: 100,
        include_layout: true,
    }))? {
        ResponseBody::Ok(value) => Ok(serde_json::from_value(value)?),
        ResponseBody::Error(error) => Err(format!("window failed: {error:?}").into()),
    }
}

#[test]
fn native_folding_preserves_original_rows_details_lanes_and_new_content_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    let before = window(&mut workspace).unwrap();
    assert_eq!(before.total, 5, "group metadata adds no rows");
    assert_eq!(
        before
            .rows
            .first()
            .and_then(|row| row.task_group.as_ref())
            .map(|task| task.anchor.as_str()),
        Some("item:4")
    );
    workspace.toggle_disclosure("item:4", false).unwrap();
    assert_eq!(
        window(&mut workspace).unwrap().total,
        6,
        "item details have an independent control"
    );
    workspace.toggle_disclosure("item:4", true).unwrap();
    let folded = window(&mut workspace).unwrap();
    assert_eq!(
        folded.total, 2,
        "the physical anchor contracts its path, including its detail span"
    );
    assert!(folded
        .rows
        .first()
        .and_then(|row| row.task_group.as_ref())
        .is_some_and(|task| task.summarized));
    assert!(workspace.reveal_matches(&["item:4".into()]));
    let found = window(&mut workspace).unwrap();
    assert!(
        found
            .rows
            .first()
            .and_then(|row| row.task_group.as_ref())
            .is_some_and(|task| !task.summarized && task.expanded == Some(false)),
        "searching the folded anchor reveals its own content, without opening the path"
    );
    workspace.toggle_disclosure("item:4", true).unwrap();
    assert_eq!(
        window(&mut workspace).unwrap().total,
        6,
        "original detail expansion is restored"
    );
    workspace.toggle_disclosure("item:4", true).unwrap();
    let new = stage(&mut workspace, 5, Some("item:4"), false).unwrap();
    let (_, changed) = workspace.connect(&[], vec![new]).unwrap();
    assert!(
        changed.len() <= 3,
        "append publishes physical frontier and membership changes only"
    );
    workspace.checkpoint().unwrap();
    let appended = window(&mut workspace).unwrap();
    let anchor = appended.rows.first().ok_or("missing anchor").unwrap();
    assert_eq!(anchor.summary, "Physical activity 5");
    assert_eq!(anchor.node_key, "1:0:5");
    assert!(anchor
        .task_group
        .as_ref()
        .is_some_and(|task| task.expanded == Some(false) && !task.summarized));
    for row in &appended.rows {
        if let Some(old) = before
            .rows
            .iter()
            .find(|old| old.continuity_key == row.continuity_key)
        {
            assert_eq!((row.lane, &row.parents), (old.lane, &old.parents));
        }
    }
    drop(workspace);
    let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(
        serde_json::to_value(window(&mut reopened).unwrap().rows).unwrap(),
        serde_json::to_value(appended.rows).unwrap()
    );
    reopened.toggle_disclosure("item:4", true).unwrap();
    assert!(
        window(&mut reopened)
            .unwrap()
            .rows
            .first()
            .and_then(|row| row.task_group.as_ref())
            .is_some_and(|task| task.expanded == Some(true)),
        "an already queued click on the previous anchor still addresses the stable task path"
    );
}

#[test]
fn explicit_preparation_migrates_old_headers_without_replaying_or_replacing_physical_rows() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    let before = window(&mut workspace).unwrap();
    let order = (Reverse(4), "item:4".into(), 1);
    let mut anchor = workspace
        .blocks
        .get(&order)
        .cloned()
        .ok_or("missing anchor")
        .unwrap();
    let summary = anchor
        .meta
        .task_summary
        .take()
        .ok_or("missing task")
        .unwrap();
    drop(workspace.blocks.insert(
        order,
        anchor.clone(),
        editchain_protocol::rank::Measure {
            expanded: 2,
            visible: 1,
        },
    ));
    let header: LiveBlock = serde_json::from_value(serde_json::json!({
        "meta":{"key":"legacy-header", "node_key":"legacy-header", "sort_time":4, "row_count":1, "spans":[], "task_header":summary},
        "rows":[{"node_key":"legacy-header","continuity_key":"legacy-header","summary":"Legacy header", "kind":"task",
            "timestamp_ms":4,"group":"session","parents":[],"is_submodule":false}]})).unwrap();
    let header = workspace.rows.put(header).unwrap();
    let order = (Reverse(4), "item:4".into(), 0);
    drop(
        workspace
            .orders
            .insert("legacy-header".into(), order.clone()),
    );
    drop(workspace.blocks.insert(
        order,
        header,
        editchain_protocol::rank::Measure {
            expanded: 1,
            visible: 1,
        },
    ));
    workspace.rows.flush().unwrap();
    let mut saved = workspace.saved();
    saved.version = 1;
    drop(
        workspace
            .checkpoint_store
            .commit::<_, Saved>(&saved)
            .unwrap(),
    );
    let storage = Rc::downgrade(&workspace.checkpoint_store);
    drop(workspace);
    assert!(
        storage.upgrade().is_none(),
        "migration fixture must release the prior checkpoint owner"
    );
    let error = LiveWorkspace::open_paged(&request(root.path())).unwrap_err();
    assert!(
        error.to_string().contains("checkpoint needs preparation"),
        "opening rejects the old schema before constructing runtime services: {error}"
    );
    let mut migrated = LiveWorkspace::prepare(&request(root.path())).unwrap();
    assert!(migrated.reused_checkpoint);
    let after = window(&mut migrated).unwrap();
    assert_eq!(after.total, before.total);
    assert!(after.rows.iter().all(|row| row.kind != "task"));
    for (before, after) in before.rows.iter().zip(&after.rows) {
        assert_eq!(
            (
                &before.continuity_key,
                &before.node_key,
                before.lane,
                &before.parents
            ),
            (
                &after.continuity_key,
                &after.node_key,
                after.lane,
                &after.parents
            )
        );
    }
}
