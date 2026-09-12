use super::*;
use editchain_protocol::{TaskGroupDto, TaskStatus};

fn meta(key: &str, time: u64, parents: &[&str]) -> LiveBlockMeta {
    serde_json::from_value(serde_json::json!({
        "key": key, "sort_time": time, "row_count": 1, "spans": [],
        "node_key": key, "parents": parents, "task_group": "section",
    }))
    .unwrap()
}

fn summary(anchor: &str, time: u64, status: TaskStatus) -> LiveBlockMeta {
    let parent = match anchor {
        "d" => "c",
        "e" => "d",
        _ => "",
    };
    let mut meta = meta(anchor, time, &[parent]);
    meta.task_summary = Some(TaskGroupDto {
        title: None,
        expanded: None,
        summarized: false,
        task_id: "task".into(),
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        status,
        member_count: 4,
        anchor: anchor.into(),
    });
    meta
}

fn baseline(status: TaskStatus) -> LiveBaseline {
    LiveBaseline {
        paged: false,
        epoch: SnapshotId::new("epoch"),
        revision: 0,
        total: 4,
        blocks: vec![
            summary("d", 4, status),
            meta("c", 3, &["b"]),
            meta("b", 2, &["a"]),
            meta("a", 1, &[]),
        ],
    }
}

fn visible(index: &LiveIndex) -> Vec<String> {
    index
        .visible_between(
            ExpandedRow::new(0).unwrap(),
            ExpandedRow::new(i64::try_from(index.total()).unwrap()).unwrap(),
        )
        .map(|row| index.position(row.get()).unwrap().0)
        .collect()
}

fn delta(index: &mut LiveIndex, metas: &[LiveBlockMeta]) {
    let old: u64 = metas
        .iter()
        .filter_map(|meta| index.orders.get(&meta.key))
        .filter_map(|order| index.tree.get(order))
        .map(|block| block.meta.row_count)
        .sum();
    let upserts = metas
        .iter()
        .map(|meta| LiveBlock {
            meta: meta.clone(),
            rows: vec![serde_json::from_value(serde_json::json!({
                "summary": meta.key, "timestamp_ms": meta.sort_time, "node_key": meta.key,
                "continuity_key": meta.key, "group": "thread", "parents": [], "is_submodule": false,
            }))
            .unwrap()],
        })
        .collect();
    index
        .apply(
            &LiveDelta {
                visible_total: None,
                base_revision: index.revision,
                revision: index.revision.saturating_add(1),
                snapshot_id: index.epoch.clone(),
                removed: Vec::new(),
                total: index
                    .total()
                    .saturating_sub(old)
                    .saturating_add(u64::try_from(metas.len()).unwrap()),
                chain_generation: 0,
                max_lane: 1,
                work: editchain_protocol::LiveWork::default(),
                upserts,
            },
            &BTreeSet::new(),
        )
        .unwrap();
}

#[test]
fn completed_history_folds_only_straight_interiors_and_rank_select_skips_them() {
    let mut index = LiveIndex::new(&baseline(TaskStatus::Completed)).unwrap();
    assert_eq!(visible(&index), ["d", "a"]);
    assert_eq!(index.visible_total(), 2);
    for start in 0..4 {
        for end in start..4 {
            let rows = index
                .visible_between(
                    ExpandedRow::new(start).unwrap(),
                    ExpandedRow::new(end).unwrap(),
                )
                .map(ExpandedRow::get)
                .collect::<Vec<_>>();
            assert_eq!(
                rows,
                [0, 3]
                    .into_iter()
                    .filter(|row| *row >= start && *row <= end)
                    .collect::<Vec<_>>()
            );
        }
    }
    assert!(index.toggle_task(ExpandedRow::new(0).unwrap()));
    assert_eq!(visible(&index), ["d", "c", "b", "a"]);
    assert!(index.toggle_task(ExpandedRow::new(0).unwrap()));
    index.reveal(ExpandedRow::new(2).unwrap());
    assert_eq!(visible(&index), ["d", "b", "a"]);
}

#[test]
fn new_and_revised_members_stay_visible_even_in_a_completed_folded_task() {
    let mut index = LiveIndex::new(&baseline(TaskStatus::Completed)).unwrap();
    delta(
        &mut index,
        &[summary("e", 5, TaskStatus::Completed), meta("d", 4, &["c"])],
    );
    assert_eq!(visible(&index), ["e", "d", "a"]);
    delta(&mut index, &[meta("b", 2, &["a"])]);
    assert_eq!(visible(&index), ["e", "d", "b", "a"]);
    assert!(!index.task_expanded(ExpandedRow::new(0).unwrap()));
}

#[test]
fn completion_never_collapses_a_task_already_visible_in_this_view() {
    let mut index = LiveIndex::new(&baseline(TaskStatus::InProgress)).unwrap();
    delta(&mut index, &[summary("d", 4, TaskStatus::Completed)]);
    assert_eq!(visible(&index), ["d", "c", "b", "a"]);
    let mut empty = LiveIndex::new(&LiveBaseline {
        paged: false,
        epoch: SnapshotId::new("e"),
        revision: 0,
        total: 0,
        blocks: Vec::new(),
    })
    .unwrap();
    delta(&mut empty, &baseline(TaskStatus::Completed).blocks);
    assert_eq!(visible(&empty), ["d", "c", "b", "a"]);
}

#[test]
fn late_forks_reveal_hidden_attachments_without_moving_existing_lanes() {
    let mut index = LiveIndex::new(&baseline(TaskStatus::Completed)).unwrap();
    let before = geometry(&index, "b");
    let mut branch = meta("fork", 6, &["b"]);
    branch.task_group = None;
    delta(&mut index, &[branch]);
    assert!(visible(&index).iter().any(|key| key == "b"));
    assert_eq!(geometry(&index, "b").lane, before.lane);
    let folded = geometry(&index, "b");
    let anchor = index.absolute(&("d".into(), 0)).unwrap();
    assert!(index.toggle_task(ExpandedRow::new(anchor).unwrap()));
    assert_eq!(
        serde_json::to_value(geometry(&index, "b")).unwrap(),
        serde_json::to_value(folded).unwrap()
    );
}

fn geometry(index: &LiveIndex, key: &str) -> editchain_protocol::HistoryRow {
    let mut row = serde_json::from_value(serde_json::json!({"summary":"", "timestamp_ms":0, "node_key":key, "group":"", "parents":[], "is_submodule":false})).unwrap();
    index.graph.decorate(key, 0, &mut row);
    row
}
