use super::*;
use serde_json::json;

fn capture(root: &std::path::Path, recorder: u8, person: u8) {
    let events: Vec<_> = [
        json!({"type":"tracking_started","activity_schema":3,"dwell_ms":2000,"vscode_version":"1.85.0"}),
        json!({"type":"editor_opened","editor":"tab","uri":"file:///a.rs","path":"a.rs"}),
    ].into_iter().enumerate().map(|(index, event)| json!({
        "schema":1,"session":format!("{recorder:08}-1111-4111-8111-111111111111"),
        "identity":{"kind":"unsigned","guid":format!("{person:08}-9999-4999-8999-999999999999"),"stream":"a".repeat(24)},
        "user_name":"shared-display-name","sequence":index.saturating_add(1),
        "time_ms":u64::from(recorder).saturating_mul(100),"event":event
    })).collect();
    let response = crate::Server::new().handle(&serde_json::from_value(json!({"id":1,"body":{
        "RecordEditorEvents":{"workspace_path":root,"chain_dir":".editchain","events":events}
    }})).unwrap()).unwrap();
    assert!(matches!(response.body, ResponseBody::Ok(_)));
}

fn old_human_graph(workspace: &mut LiveWorkspace) -> Vec<String> {
    let mut blocks: Vec<_> = workspace
        .orders
        .values()
        .filter_map(|order| {
            let mut block = workspace.blocks.get(order)?.clone();
            let _stream = block.meta.human_stream.take()?;
            Some(block)
        })
        .collect();
    blocks.sort_by_key(|block| Reverse(block.meta.order()));
    let keys: Vec<_> = blocks.iter().map(|block| block.meta.key.clone()).collect();
    let metas: Vec<_> = blocks.iter().map(|block| block.meta.clone()).collect();
    workspace.graph.edit(&keys, &metas);
    drop(workspace.connect(&[], blocks).unwrap());
    keys
}

#[test]
fn version_seven_repairs_human_lanes_using_cached_inputs_and_keeps_other_lanes() {
    let root = tempfile::tempdir().unwrap();
    capture(root.path(), 1, 1);
    capture(root.path(), 2, 2);
    capture(root.path(), 3, 1);
    let canonical = human_edits::canonical(root.path());
    let mut workspace = fixture(root.path()).unwrap();
    let keys = old_human_graph(&mut workspace);
    assert_eq!(keys.len(), 3);
    let before = window(&mut workspace).unwrap();
    let human: Vec<_> = before
        .rows
        .iter()
        .filter(|row| row.author == "human")
        .collect();
    let [latest, _peer, earliest] = <[_; 3]>::try_from(human).unwrap();
    assert_eq!(latest.group, earliest.group);
    assert_ne!(latest.lane, earliest.lane, "legacy reload bend reproduced");
    workspace.rows.flush().unwrap();
    let pages = workspace
        .chain
        .join("live-v1/rows")
        .metadata()
        .unwrap()
        .len();
    let mut saved = workspace.saved();
    saved.version = 7;
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
    let human: Vec<_> = after
        .rows
        .iter()
        .filter(|row| row.author == "human")
        .collect();
    let [latest, peer, earliest] = <[_; 3]>::try_from(human).unwrap();
    assert_eq!(
        latest.lane, earliest.lane,
        "both recorder runs share one lane"
    );
    assert_ne!(
        latest.lane, peer.lane,
        "a peer with the same display name stays separate"
    );
    for old in &before.rows {
        let current = after
            .rows
            .iter()
            .find(|row| row.node_key == old.node_key)
            .unwrap();
        assert_eq!(current.parents, old.parents);
        assert_eq!(current.group, old.group);
        if old.author != "human" {
            assert_eq!(current.lane, old.lane);
        }
    }
    for key in keys {
        let block = resumed
            .blocks
            .get(resumed.orders.get(&key).unwrap())
            .unwrap();
        assert!(block.meta.human_stream.is_some());
    }
    assert_eq!(
        resumed.chain.join("live-v1/rows").metadata().unwrap().len(),
        pages
    );
    assert_eq!(human_edits::canonical(root.path()), canonical);
    assert_eq!(
        load(&resumed.checkpoint_store).unwrap().unwrap().version,
        VERSION
    );
    drop(resumed);
    let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(
        serde_json::to_value(window(&mut reopened).unwrap().rows).unwrap(),
        serde_json::to_value(after.rows).unwrap()
    );
}
