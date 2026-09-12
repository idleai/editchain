use super::*;

fn grouped_meta(index: usize) -> Value {
    json!({"key": format!("item:{index}"), "node_key": format!("node:{index}"),
        "sort_time": index, "row_count": 1, "spans": [], "task_group": "section",
        "parents": index.checked_sub(1).map(|parent| format!("item:{parent}")).into_iter().collect::<Vec<_>>(),
    })
}

fn grouped_header(index: usize) -> Value {
    json!({"key": "section", "node_key": "section", "sort_time": index, "row_count": 1, "spans": [],
        "task_header": {"task_id":"task", "thread_id":"thread", "turn_id":"turn", "status":"completed",
            "member_count": index.saturating_add(1), "anchor": format!("item:{index}")},
    })
}

fn grouped_state() -> HistoryAppState {
    let baseline = serde_json::from_value(json!({"epoch":"live", "revision":0, "total":4,
        "blocks":[grouped_header(2), grouped_meta(2), grouped_meta(1), grouped_meta(0)]}))
    .unwrap();
    HistoryAppState {
        snapshot_id: SnapshotId::new("live"),
        total: Some(4),
        phase: SnapshotPhase::LayoutReady,
        expansion: Some(ExpansionIndex::from_live(&baseline).unwrap()),
        ..HistoryAppState::default()
    }
}

#[test]
fn appending_keeps_a_previously_exposed_endpoint_and_the_readers_pixel_anchor() {
    let mut state = grouped_state();
    for (absolute, key) in [(1, 2), (3, 0)] {
        drop(
            state
                .cache
                .insert_legacy(ExpandedRow::new(absolute).unwrap(), &row(key)),
        );
    }
    let viewport = Viewport::new(34, 34);
    let step = message(
        &mut state,
        json!("delta"),
        json!({"Ok": {"epoch":"live", "revision":1,
        "work": editchain_protocol::LiveWork::default(), "deltas":[{
            "base_revision":0, "revision":1, "snapshot_id":"live:1", "removed":[], "total":5,
            "chain_generation":1, "max_lane":1, "work":editchain_protocol::LiveWork::default(),
            "upserts":[{"meta":grouped_header(3), "rows":[row(99)]}, {"meta":grouped_meta(3), "rows":[row(3)]}],
        }]}}),
        &viewport,
    );
    assert_eq!(
        state.visible_index_for_abs(2),
        Some(2),
        "the old endpoint stays exposed after gaining a child"
    );
    assert!(
        !state.is_row_expanded(0),
        "preserving the viewport does not open its task"
    );
    assert!(step
        .ops
        .iter()
        .any(|op| matches!(op, DomOp::ReanchorLive { scroll_top: 68, .. })));
}

#[test]
fn search_reveals_an_uncached_folded_member_before_scheduling_its_page() {
    let mut state = grouped_state();
    assert_eq!(state.visible_index_for_abs(2), None);
    let mut step = Step::new();
    state.submit_find("needle", &mut step);
    let request = state.requests.log().last().unwrap().id;
    let _step = message(
        &mut state,
        json!(request),
        json!({"Ok":{
            "snapshot_id":"live", "matches":[{"row":2, "node_key":"node:1", "summary":"needle"}], "returned":1, "more":false,
        }}),
        &Viewport::new(0, 100),
    );
    assert!(state.visible_index_for_abs(2).is_some());
    assert!(!state.is_row_expanded(0));
    let page = state
        .requests
        .log()
        .last()
        .unwrap()
        .body
        .get("GetWindow")
        .unwrap();
    let start = page.get("offset").unwrap().as_u64().unwrap();
    let limit = page.get("limit").unwrap().as_u64().unwrap();
    assert!(start <= 2 && start.saturating_add(limit) > 2);
}

fn row(key: usize) -> Value {
    json!({
        "node_key": format!("node:{key}"), "continuity_key": format!("item:{key}"),
        "summary": format!("message {key}"), "timestamp_ms": 1,
        "group": "session:live", "parents": [], "is_submodule": false,
        "is_system": false, "author": "", "commit_id": "", "kind": "message",
        "lane": 0, "above": [], "below": [], "transitions": [], "sub_ops": [], "is_subop": false,
    })
}

fn message(state: &mut HistoryAppState, id: Value, body: Value, viewport: &Viewport) -> Step {
    let mut step = Step::new();
    let envelope: Value = [("id", id), ("body", body)].into_iter().collect();
    state.handle_host_message(HostMessage::parse(&envelope).unwrap(), viewport, &mut step);
    step
}

fn opened(snapshot: &str, nodes: usize) -> Value {
    json!({"Ok": {"protocol_version": 2, "snapshot_id": snapshot, "nodes": nodes}})
}

fn window(
    state: &mut HistoryAppState,
    rows: &[Value],
    counts: &[usize],
    viewport: &Viewport,
) -> Step {
    let request = state.requests.log().last().unwrap().clone();
    let offset = request
        .body
        .get("GetWindow")
        .unwrap()
        .get("offset")
        .unwrap()
        .as_u64()
        .unwrap();
    let limit = request
        .body
        .get("GetWindow")
        .unwrap()
        .get("limit")
        .unwrap()
        .as_u64()
        .unwrap();
    let page: Vec<_> = rows
        .iter()
        .skip(usize::try_from(offset).unwrap())
        .take(usize::try_from(limit).unwrap())
        .cloned()
        .collect();
    message(
        state,
        json!(request.id),
        json!({"Ok": {
            "snapshot_id": state.snapshot_id, "total": rows.len(), "rows": page,
            "chain_generation": 0, "max_lane": 0, "layout_ready": true,
            "sub_op_counts": (offset == 0).then_some(counts),
        }}),
        viewport,
    )
}

fn update(state: &mut HistoryAppState, rows: &[Value], viewport: &Viewport) {
    let _step = message(state, json!("updating"), Value::Null, viewport);
    let step = message(state, json!("update"), opened("next", rows.len()), viewport);
    assert!(!step
        .ops
        .iter()
        .any(|op| matches!(op, DomOp::Reanchor { .. } | DomOp::ReanchorLive { .. })));
    let request = state.requests.log().last().unwrap().clone();
    let keys = request
        .body
        .get("LocateRows")
        .unwrap()
        .get("keys")
        .unwrap()
        .as_array()
        .unwrap();
    let locations: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| keys.contains(row.get("continuity_key").unwrap()))
        .map(|(index, row)| {
            json!({
                "row": index, "key": row.get("continuity_key"), "node_key": row.get("node_key"),
            })
        })
        .collect();
    let _step = message(
        state,
        json!(request.id),
        json!({"Ok": {"snapshot_id": "next", "rows": locations}}),
        viewport,
    );
}

#[test]
fn live_growth_and_item_revision_preserve_selection_expansion_and_pixel_anchor() {
    let viewport = Viewport::new(347, 102);
    let rows: Vec<_> = (0..200).map(row).collect();
    let mut counts = vec![0; 198];
    *counts.get_mut(1).unwrap() = 2;
    let mut state = HistoryAppState::default();
    let _step = message(
        &mut state,
        json!("open"),
        opened("old", rows.len()),
        &viewport,
    );
    let _step = window(&mut state, &rows, &counts, &viewport);
    assert!(state.toggle_expanded(1));
    state.select_row(10);
    state.selection.set_roving(ExpandedRow::new(10));
    let mut next: Vec<_> = (200..203).map(row).chain(rows).collect();
    *next.get_mut(13).unwrap().get_mut("node_key").unwrap() = json!("revised:10");
    let mut next_counts = vec![0; 201];
    *next_counts.get_mut(4).unwrap() = 2;
    update(&mut state, &next, &viewport);
    let step = window(&mut state, &next, &next_counts, &viewport);
    assert_eq!(state.selected_key(), Some("revised:10"));
    assert_eq!(state.roving_abs(), 13);
    assert!(state.is_row_expanded(4));
    assert!(step.ops.iter().any(|op| matches!(
        op,
        DomOp::ReanchorLive {
            scroll_top: 449,
            ..
        }
    )));
    assert!(step
        .sends
        .iter()
        .any(|send| matches!(send, Send::LiveSettled { error: None, .. })));
}

#[test]
fn live_handover_waits_for_a_distant_viewport_and_rejects_retired_responses() {
    let viewport = Viewport::new(34_007, 102);
    let mut state = HistoryAppState {
        snapshot_id: SnapshotId::new("old"),
        total: Some(2000),
        phase: SnapshotPhase::LayoutReady,
        expansion: Some(ExpansionIndex::from_metadata(2000, Some(&vec![0; 2000]), None).unwrap()),
        ..HistoryAppState::default()
    };
    for index in 1000..1010 {
        drop(state.cache.insert_legacy(
            ExpandedRow::new(index).unwrap(),
            &row(usize::try_from(index).unwrap()),
        ));
    }
    let next: Vec<_> = (2000..2700).map(row).chain((0..2000).map(row)).collect();
    update(&mut state, &next, &viewport);
    let counts = vec![0; next.len()];
    let first = window(&mut state, &next, &counts, &viewport);
    assert!(!first
        .ops
        .iter()
        .any(|op| matches!(op, DomOp::ReanchorLive { .. } | DomOp::Reanchor { .. })));
    let mut finished = false;
    for _ in 0..8 {
        let step = window(&mut state, &next, &counts, &viewport);
        if step.ops.iter().any(|op| {
            matches!(
                op,
                DomOp::ReanchorLive {
                    scroll_top: 57_807,
                    ..
                }
            )
        }) {
            finished = true;
            break;
        }
    }
    assert!(finished);
    let generation = state.view_gen;
    let ignored = message(
        &mut state,
        json!(999_999),
        json!({"Error": "stale snapshot"}),
        &viewport,
    );
    assert!(ignored.ops.is_empty());
    assert_eq!(state.view_gen, generation);
    assert_eq!(state.snapshot_id.as_str(), "next");
}

#[test]
fn deleted_selection_clears_and_live_top_remains_pinned() {
    let viewport = Viewport::new(0, 68);
    let mut state = HistoryAppState::default();
    let rows: Vec<_> = (0..30).map(row).collect();
    let _step = message(&mut state, json!("open"), opened("old", 30), &viewport);
    let _step = window(&mut state, &rows, &[0; 30], &viewport);
    state.select_row(1);
    let next: Vec<_> = (30..35).chain(2..30).map(row).collect();
    update(&mut state, &next, &viewport);
    let step = window(&mut state, &next, &vec![0; next.len()], &viewport);
    assert_eq!(state.selected_key(), None);
    assert!(step
        .ops
        .iter()
        .any(|op| matches!(op, DomOp::ReanchorLive { scroll_top: 0, .. })));
}

#[test]
fn live_graph_width_uses_window_geometry_including_crossing_edges() {
    let baseline = serde_json::from_value(json!({
        "epoch": "live", "revision": 0, "total": 4,
        "blocks": (0..4).map(|index| json!({
            "key": format!("item:{index}"), "node_key": format!("node:{index}"),
            "sort_time": 100 - index, "row_count": 1, "spans": [],
        })).collect::<Vec<_>>(),
    }))
    .unwrap();
    let mut state = HistoryAppState {
        total: Some(4),
        max_lane: 190,
        render_top: 0,
        render_bottom: 2,
        expansion: Some(ExpansionIndex::from_live(&baseline).unwrap()),
        ..HistoryAppState::default()
    };
    for (index, lane) in [2, 3, 2, 190].into_iter().enumerate() {
        let mut source = row(index);
        *source.get_mut("lane").unwrap() = json!(lane);
        if index == 1 {
            *source.get_mut("transitions").unwrap() = json!([[3, 9]]);
            *source.get_mut("below").unwrap() = json!([9]);
        }
        drop(state.cache.insert_legacy(
            ExpandedRow::new(i64::try_from(index).unwrap()).unwrap(),
            &source,
        ));
    }
    assert_eq!(
        state.graph_max_lane(),
        9,
        "include passing edges, exclude distant peaks"
    );
    let layout = crate::app::dom::graph_layout(state.graph_max_lane(), 1440.0, 1440.0);
    assert!((layout.lane_width - 14.76).abs() < 0.01);
    state.render_bottom = 3;
    assert_eq!(
        state.graph_max_lane(),
        190,
        "scrolling includes the newly visible lane"
    );
    state.expansion = None;
    state.render_bottom = 2;
    assert_eq!(
        state.graph_max_lane(),
        190,
        "fixed snapshots retain their layout contract"
    );
}

#[test]
fn operation_deltas_retain_cached_rows_pixel_anchor_selection_and_replay_cursor() {
    let viewport = Viewport::new(347, 102);
    let blocks: Vec<_> = (0..30).map(|index| json!({
        "key": format!("item:{index}"), "sort_time": 1000 - index, "row_count": 1, "spans": [],
        "node_key": format!("node:{index}"), "parents": [],
    })).collect();
    let baseline = serde_json::from_value(
        json!({"epoch": "live", "revision": 0, "total": 30, "blocks": blocks}),
    )
    .unwrap();
    let mut state = HistoryAppState {
        snapshot_id: SnapshotId::new("live"),
        total: Some(30),
        phase: SnapshotPhase::LayoutReady,
        expansion: Some(ExpansionIndex::from_live(&baseline).unwrap()),
        ..HistoryAppState::default()
    };
    for index in 0..30 {
        drop(state.cache.insert_legacy(
            ExpandedRow::new(index).unwrap(),
            &row(usize::try_from(index).unwrap()),
        ));
    }
    state.select_row(10);
    state.selection.set_roving(ExpandedRow::new(10));
    let mut revised = row(10);
    *revised.get_mut("node_key").unwrap() = json!("revised:10");
    let delta = json!({"Ok": {"epoch": "live", "revision": 1, "work": editchain_protocol::LiveWork::default(), "deltas": [{
        "base_revision": 0, "revision": 1, "snapshot_id": "live:1", "removed": [], "total": 31,
        "chain_generation": 31, "max_lane": 0, "work": editchain_protocol::LiveWork::default(),
        "upserts": [
            {"meta": {"key": "item:30", "sort_time": 2000, "row_count": 1, "spans": []}, "rows": [row(30)]},
            {"meta": blocks.get(10).unwrap(), "rows": [revised]},
        ],
    }]}});
    let step = message(&mut state, json!("delta"), delta.clone(), &viewport);
    assert_eq!(state.selected_key(), Some("revised:10"));
    assert_eq!(state.roving_abs(), 11);
    assert_eq!(
        state.cache.get_by_index(21).unwrap().source.node_key,
        "node:20"
    );
    assert!(step.ops.iter().any(|op| matches!(
        op,
        DomOp::ReanchorLive {
            scroll_top: 381,
            ..
        }
    )));
    assert!(
        state.requests.log().is_empty(),
        "cached viewport needs no replacement Open or page"
    );
    let replay = message(&mut state, json!("delta"), delta, &viewport);
    assert!(
        replay.ops.is_empty(),
        "duplicate delivery does not reanimate"
    );
    assert!(replay
        .sends
        .iter()
        .any(|send| matches!(send, Send::LiveSettled { error: None, .. })));
    let gap = json!({"Ok": {"epoch": "live", "revision": 3, "work": editchain_protocol::LiveWork::default(), "deltas": [{
        "base_revision": 2, "revision": 3, "snapshot_id": "live:3", "removed": [], "upserts": [],
        "total": 31, "chain_generation": 31, "max_lane": 0, "work": editchain_protocol::LiveWork::default(),
    }]}});
    let rejected = message(&mut state, json!("delta"), gap, &viewport);
    assert!(rejected.sends.iter().any(|send| matches!(send, Send::LiveSettled { snapshot_id, error: Some(_), .. } if snapshot_id == "live:3")));
}
