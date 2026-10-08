use super::{append, message, put, reference};
use crate::{ChainIndex, IndexChangeKind};
use editchain_core::{ActorId, Payload};

#[test]
fn durable_changes_include_late_ids_conflicts_and_content_after_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let bytes = b"content arrives after the operation";
    let first = message(80, Payload::Blob(reference(bytes)));
    append(root, std::slice::from_ref(&first));
    let index = ChainIndex::open(root).unwrap();
    let initial = index.revision();
    drop(index);

    let earlier = message(2, Payload::Empty);
    append(root, std::slice::from_ref(&earlier));
    let _blob = put(root, bytes);
    let index = ChainIndex::open(root).unwrap();
    let first_page = index.changes_since(&initial, 1).unwrap().unwrap();
    assert!(
        !first_page.complete,
        "bounded changes retain a continuation"
    );
    let last_page = index.changes_since(&first_page.next, 10).unwrap().unwrap();
    assert!(last_page.complete, "the final page reaches this revision");
    let kinds: Vec<_> = first_page
        .changes
        .into_iter()
        .chain(last_page.changes)
        .map(|change| (change.operation, change.kind))
        .collect();
    assert!(
        kinds.contains(&(earlier.id, IndexChangeKind::Added)),
        "late lower IDs are included"
    );
    assert!(
        kinds.contains(&(first.id, IndexChangeKind::Content)),
        "late blobs advance the revision"
    );
    let before_conflict = index.revision();
    drop(index);

    append(
        root,
        &[editchain_core::Op {
            actor: ActorId(999),
            ..earlier.clone()
        }],
    );
    let mut index = ChainIndex::open(root).unwrap();
    let changes = index.changes_since(&before_conflict, 10).unwrap().unwrap();
    assert!(changes.changes.iter().any(|change| change.operation == earlier.id && change.kind == IndexChangeKind::Removed), "quarantine retracts dependent rows");
    let stable = index.revision();
    let _idle = index.refresh().unwrap();
    assert_eq!(
        index.revision(),
        stable,
        "idle reads do not invalidate windows"
    );
    let _rebuilt = index.rebuild().unwrap();
    assert!(
        index.changes_since(&stable, 10).unwrap().is_none(),
        "a rebuilt index requires a new derived snapshot"
    );
}

#[test]
fn expired_or_future_cursors_never_look_like_complete_empty_changes() {
    let mut journal = crate::changes::ChangeLog::default();
    let _initialized = journal.initialize().unwrap();
    let initial = journal.revision.clone();
    for sequence in 0..=100_000 {
        journal
            .append(message(sequence, Payload::Empty).id, IndexChangeKind::Added)
            .unwrap();
    }
    assert!(
        journal.since(&initial, 10).unwrap().is_none(),
        "expired journals require a full rebuild"
    );
    let mut future = journal.revision.clone();
    future.position = future.position.saturating_add(1);
    assert!(
        journal.since(&future, 10).unwrap().is_none(),
        "future revisions are invalid"
    );
    assert!(
        journal.since(&journal.revision, 0).is_err(),
        "zero-sized reads are rejected"
    );
}
