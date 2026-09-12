use super::*;
use editchain_core::OpId;
use std::cmp::Reverse;

fn task(turn: &str) -> TaskIdentity {
    TaskIdentity {
        key: format!("thread:{turn}"),
        thread: "thread".into(),
        turn: turn.into(),
        boundary: OpId::new(editchain_core::NodeId(1), 0, 0),
    }
}

fn put(runs: &mut runs::Runs, time: u64, turn: Option<&str>) {
    let key = format!("item:{time}");
    runs.put(key.clone(), (Reverse(time), key, 1), turn.map(task));
}

#[test]
fn append_and_revision_have_bounded_membership_work_at_any_task_size() {
    for size in [10, 1_000, 100_000] {
        let mut runs = runs::Runs::default();
        for time in 0..size {
            put(&mut runs, time, Some("a"));
        }
        let original = runs.sections.keys().next().unwrap().clone();
        runs.dirty.clear();
        runs.membership.clear();
        put(&mut runs, size, Some("a"));
        assert_eq!(runs.sections.len(), 1);
        assert_eq!(
            runs.sections.get(&original).unwrap().members.len(),
            usize::try_from(size).unwrap().saturating_add(1)
        );
        assert_eq!(runs.dirty.len(), 1);
        assert_eq!(runs.membership.len(), 1);
        assert_eq!(
            runs.membership
                .get(&format!("item:{size}"))
                .unwrap()
                .clone(),
            Some(original.clone())
        );
        runs.dirty.clear();
        runs.membership.clear();
        put(&mut runs, size, Some("a"));
        assert!(
            runs.dirty.is_empty(),
            "a content revision does not rebuild its header"
        );
        assert_eq!(runs.membership.len(), 1);
        assert_eq!(
            runs.membership
                .get(&format!("item:{size}"))
                .unwrap()
                .clone(),
            Some(original)
        );
    }
}

#[test]
fn late_interleaving_splits_only_its_contiguous_section_without_reordering() {
    let mut runs = runs::Runs::default();
    for time in [10, 20, 30, 40] {
        put(&mut runs, time, Some("a"));
    }
    let original = runs.sections.keys().next().unwrap().clone();
    runs.dirty.clear();
    runs.membership.clear();
    put(&mut runs, 25, Some("b"));
    assert_eq!(runs.sections.len(), 3);
    assert_eq!(
        runs.sections
            .get(&original)
            .unwrap()
            .members
            .iter()
            .map(|at| at.0 .0)
            .collect::<Vec<_>>(),
        [20, 10]
    );
    assert_eq!(
        runs.membership.len(),
        3,
        "only the new item and split prefix move"
    );
    let newer = runs
        .membership
        .get("item:40")
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(
        runs.sections
            .get(&newer)
            .unwrap()
            .members
            .iter()
            .map(|at| at.0 .0)
            .collect::<Vec<_>>(),
        [40, 30]
    );
    runs.remove("item:25");
    put(&mut runs, 50, Some("a"));
    assert_eq!(runs.membership.get("item:50").unwrap().clone(), Some(newer));
    assert_eq!(
        runs.sections.get(&original).unwrap().members.len(),
        2,
        "removing a barrier does not rename established sections"
    );
}

#[test]
fn ungrouped_records_and_rollback_incarnations_are_section_boundaries() {
    let mut runs = runs::Runs::default();
    put(&mut runs, 10, Some("a"));
    put(&mut runs, 20, None);
    put(&mut runs, 30, Some("a"));
    assert_eq!(runs.sections.len(), 2);
    let mut restored = task("a");
    restored.key.push_str(":restored");
    restored.boundary = OpId::new(editchain_core::NodeId(1), 0, 35);
    runs.put(
        "item:40".into(),
        (Reverse(40), "item:40".into(), 1),
        Some(restored),
    );
    assert_eq!(runs.sections.len(), 3);
}

#[test]
fn splitting_after_an_older_backfill_cannot_reuse_the_original_section_key() {
    let mut runs = runs::Runs::default();
    put(&mut runs, 40, Some("a"));
    let original = runs.sections.keys().next().unwrap().clone();
    put(&mut runs, 10, Some("a"));
    put(&mut runs, 25, Some("b"));
    assert_eq!(runs.sections.len(), 3);
    assert_eq!(
        runs.sections
            .values()
            .map(|run| run.members.len())
            .sum::<usize>(),
        3
    );
    assert_eq!(
        runs.sections
            .get(&original)
            .unwrap()
            .members
            .first()
            .unwrap()
            .1,
        "item:10"
    );
    let newer = runs.membership.get("item:40").unwrap().as_ref().unwrap();
    assert_ne!(newer, &original);
    assert_eq!(runs.sections.get(newer).unwrap().members.len(), 1);
}
