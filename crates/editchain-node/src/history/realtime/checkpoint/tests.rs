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
    assert_eq!(
        window(&mut workspace).unwrap().total,
        2,
        "offscreen tasks start folded"
    );
    workspace.toggle_disclosure("item:4", true).unwrap();
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
    report_viewport(&mut workspace, &["item:4", "item:0"], true, 20);
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
    assert!(
        anchor
            .task_group
            .as_ref()
            .is_some_and(|task| task.expanded == Some(false) && task.summarized),
        "an explicit close applies to subsequent arrivals too"
    );
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
    let resumed = window(&mut reopened).unwrap();
    assert_eq!(resumed.total, 2, "an explicit close survives restart");
    assert!(
        resumed
            .rows
            .first()
            .unwrap()
            .task_group
            .as_ref()
            .unwrap()
            .summarized
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

fn report_viewport(workspace: &mut LiveWorkspace, keys: &[&str], at_head: bool, capacity: u16) {
    workspace
        .observe_viewport(&editchain_protocol::ViewportLiveRequest {
            snapshot_id: workspace.snapshot_id.clone(),
            keys: keys.iter().map(|key| (*key).into()).collect(),
            at_head,
            capacity,
        })
        .unwrap();
}

#[test]
fn offscreen_appends_stay_folded_and_explicit_opens_survive_scrolling_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    report_viewport(&mut workspace, &["item:0"], false, 8);
    for sequence in 5..25 {
        let parent = format!("item:{}", sequence - 1);
        let block = stage(&mut workspace, sequence, Some(&parent), false).unwrap();
        let (_, changed) = workspace.connect(&[], vec![block]).unwrap();
        assert!(changed.len() <= 3);
        let page = window(&mut workspace).unwrap();
        assert_eq!(page.total, 2);
        let task = page.rows.first().unwrap().task_group.as_ref().unwrap();
        assert_eq!(task.expanded, Some(false));
        assert!(task.summarized);
    }
    workspace.toggle_disclosure("item:24", true).unwrap();
    report_viewport(&mut workspace, &["item:0"], false, 8);
    let block = stage(&mut workspace, 25, Some("item:24"), false).unwrap();
    drop(workspace.connect(&[], vec![block]).unwrap());
    workspace.checkpoint().unwrap();
    let page = window(&mut workspace).unwrap();
    assert_eq!(page.total, 26);
    assert_eq!(
        page.rows
            .first()
            .unwrap()
            .task_group
            .as_ref()
            .unwrap()
            .expanded,
        Some(true)
    );
    drop(workspace);
    let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(window(&mut reopened).unwrap().total, 26);
}

#[test]
fn latest_task_opens_completely_and_only_folds_when_all_members_leave_the_viewport() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    report_viewport(&mut workspace, &["item:4", "item:0"], true, 8);
    let initial = window(&mut workspace).unwrap();
    assert_eq!(
        initial.total, 5,
        "the latest task opens on its first display"
    );
    assert!(initial
        .rows
        .first()
        .unwrap()
        .task_group
        .as_ref()
        .is_some_and(|task| { task.expanded == Some(true) && !task.summarized }));
    let mut blocks = Vec::new();
    for sequence in 5..1005 {
        let parent = format!("item:{}", sequence - 1);
        blocks.push(stage(&mut workspace, sequence, Some(&parent), false).unwrap());
    }
    drop(workspace.connect(&[], blocks).unwrap());
    let page = window(&mut workspace).unwrap();
    assert_eq!(
        page.total, 1005,
        "an open path exposes all its physical members"
    );
    assert!(
        !page
            .rows
            .first()
            .unwrap()
            .task_group
            .as_ref()
            .unwrap()
            .summarized
    );
    assert_eq!(
        page.rows
            .first()
            .unwrap()
            .task_group
            .as_ref()
            .unwrap()
            .expanded,
        Some(true)
    );
    let lane = page.rows.first().unwrap().lane;
    report_viewport(&mut workspace, &["item:500"], false, 8);
    assert_eq!(
        window(&mut workspace).unwrap().total,
        1005,
        "the ribbon can leave the viewport while another member remains visible"
    );
    let block = stage(&mut workspace, 1005, Some("item:1004"), false).unwrap();
    let (_, changed) = workspace.connect(&[], vec![block]).unwrap();
    assert!(
        changed.len() <= 3,
        "an open path still accepts bounded +1 edits"
    );
    assert_eq!(window(&mut workspace).unwrap().total, 1006);
    report_viewport(&mut workspace, &["item:0"], false, 8);
    let folded = window(&mut workspace).unwrap();
    assert_eq!(folded.total, 2);
    assert!(
        folded
            .rows
            .first()
            .unwrap()
            .task_group
            .as_ref()
            .unwrap()
            .summarized
    );
    assert_eq!(folded.rows.first().unwrap().lane, lane);
    report_viewport(&mut workspace, &["item:1005", "item:0"], true, 8);
    assert_eq!(
        window(&mut workspace).unwrap().total,
        2,
        "scrolling back does not open a task"
    );
    let block = stage(&mut workspace, 1006, Some("item:1005"), false).unwrap();
    drop(workspace.connect(&[], vec![block]).unwrap());
    let fresh = window(&mut workspace).unwrap();
    assert_eq!(
        fresh.total, 1007,
        "fresh visible activity opens the whole path again"
    );
    assert_eq!(
        fresh
            .rows
            .first()
            .unwrap()
            .task_group
            .as_ref()
            .unwrap()
            .expanded,
        Some(true)
    );
}

#[test]
fn automatic_opens_resume_at_the_head_and_explicit_closes_override_live_updates() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    report_viewport(&mut workspace, &["item:4", "item:0"], true, 8);
    assert_eq!(window(&mut workspace).unwrap().total, 5);
    drop(workspace);
    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(
        window(&mut workspace).unwrap().total,
        2,
        "the persisted automatic open expires before the next viewport is known"
    );
    report_viewport(&mut workspace, &["item:4", "item:0"], true, 8);
    assert_eq!(window(&mut workspace).unwrap().total, 5);
    workspace.toggle_disclosure("item:4", true).unwrap();
    let block = stage(&mut workspace, 5, Some("item:4"), false).unwrap();
    drop(workspace.connect(&[], vec![block]).unwrap());
    workspace.checkpoint().unwrap();
    let closed = window(&mut workspace).unwrap();
    assert_eq!(closed.total, 2);
    assert!(closed
        .rows
        .first()
        .unwrap()
        .task_group
        .as_ref()
        .is_some_and(|task| { task.expanded == Some(false) && task.summarized }));
    drop(workspace);
    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    report_viewport(&mut workspace, &["item:5", "item:0"], true, 8);
    assert_eq!(
        window(&mut workspace).unwrap().total,
        2,
        "initial head disclosure respects a persisted explicit close"
    );
}

#[test]
fn starting_a_new_task_does_not_reopen_its_folded_predecessor() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    report_viewport(&mut workspace, &["item:4", "item:0"], true, 8);
    report_viewport(&mut workspace, &["item:0"], false, 8);
    report_viewport(&mut workspace, &["item:4", "item:0"], true, 8);
    assert_eq!(window(&mut workspace).unwrap().total, 2);
    let mut blocks = Vec::new();
    for sequence in 5..7 {
        let parent = format!("item:{}", sequence - 1);
        let block = stage(&mut workspace, sequence, Some(&parent), false).unwrap();
        let task = workspace
            .inputs
            .get_mut(&block.meta.key)
            .unwrap()
            .task
            .as_mut()
            .unwrap();
        task.key = "next-task".into();
        task.turn = "next-turn".into();
        blocks.push(block);
    }
    drop(workspace.connect(&[], blocks).unwrap());
    let page = window(&mut workspace).unwrap();
    assert_eq!(page.total, 4, "only the two actual arrivals open");
    assert_eq!(
        page.rows
            .first()
            .unwrap()
            .task_group
            .as_ref()
            .unwrap()
            .expanded,
        Some(true)
    );
    assert_eq!(
        page.rows
            .iter()
            .find(|row| row.continuity_key == "item:4")
            .unwrap()
            .task_group
            .as_ref()
            .unwrap()
            .expanded,
        Some(false)
    );
}

#[test]
fn schema_four_disclosure_without_automatic_state_preserves_explicit_opens() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    workspace.toggle_disclosure("item:4", true).unwrap();
    // Round-trip only disclosure through the actual checkpoint codec. Its
    // values are page addresses; the rest of Saved contains non-JSON map keys.
    let mut legacy: serde_json::Value = workspace
        .checkpoint_store
        .unload(&workspace.disclosure)
        .unwrap();
    let disclosure = legacy.as_object_mut().unwrap();
    drop(disclosure.remove("automatic"));
    drop(disclosure.remove("explicitly_closed"));
    workspace.disclosure = workspace.checkpoint_store.unload(&legacy).unwrap();
    workspace.checkpoint().unwrap();
    drop(workspace);
    let mut resumed = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert!(resumed.reused_checkpoint);
    report_viewport(&mut resumed, &["item:0"], false, 8);
    assert_eq!(
        window(&mut resumed).unwrap().total,
        5,
        "preexisting explicit opens survive without resetting or reindexing history"
    );
}

#[test]
fn schema_three_preparation_only_resets_disclosure_and_retains_the_physical_graph() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    workspace.toggle_disclosure("item:4", true).unwrap();
    let before = window(&mut workspace).unwrap();
    let mut saved = workspace.saved();
    saved.version = 3;
    drop(
        workspace
            .checkpoint_store
            .commit::<_, Saved>(&saved)
            .unwrap(),
    );
    drop(workspace);
    assert!(LiveWorkspace::open_paged(&request(root.path())).is_err());
    let mut prepared = LiveWorkspace::prepare(&request(root.path())).unwrap();
    assert_eq!(window(&mut prepared).unwrap().total, 2);
    prepared.toggle_disclosure("item:4", true).unwrap();
    let after = window(&mut prepared).unwrap();
    assert_eq!(
        serde_json::to_value(before.rows).unwrap(),
        serde_json::to_value(after.rows).unwrap()
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
