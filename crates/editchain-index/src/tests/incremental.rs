use std::io::Write as _;

use editchain_core::{ActorId, Payload};
use editchain_store::{
    format::{encode_op, encode_page, Page},
    BlobStore, CanonicalChain,
};

use crate::{ChainIndex, ContentState, IndexKey};

use super::{append, message, put, reference};

#[test]
fn new_operations_read_only_the_frontier_at_different_history_sizes() {
    let mut work = Vec::new();
    for size in [1, 10_000] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let history: Vec<_> = (100..100_u64.saturating_add(size))
            .map(|seq| message(seq, Payload::Empty))
            .collect();
        append(root, &history);
        let mut index = ChainIndex::open(root).unwrap();
        let before = std::fs::metadata(root.join("index-v1/pages"))
            .unwrap()
            .len();
        // Imports can arrive with both an older identity and older event time.
        let late = message(1, Payload::Inline(b"late".to_vec()));
        append(root, std::slice::from_ref(&late));
        let delta = index.refresh().unwrap();
        assert_eq!(delta.added.into_iter().collect::<Vec<_>>(), vec![late.id]);
        assert_eq!(delta.work.records.records_decoded, 1);
        assert_eq!(delta.work.content_reads, 0);
        assert_eq!(index.operations(None, 1).unwrap(), vec![late.id]);
        assert_eq!(index.get(late.id).unwrap(), Some(late));
        let growth = std::fs::metadata(root.join("index-v1/pages"))
            .unwrap()
            .len()
            .saturating_sub(before);
        assert!(growth < 256 * 1024, "one append rewrote {growth} bytes");
        work.push(delta.work.records.bytes_read);
        let idle = index.refresh().unwrap();
        assert_eq!(idle.work.records.bytes_read, 0);
        assert_eq!(idle.work.content_reads, 0);
        assert!(idle.added.is_empty(), "idle refresh is inert");
    }
    assert_eq!(
        work.first(),
        work.last(),
        "append reads must not grow with history"
    );
}

#[test]
fn late_blobs_notify_all_dependents_without_new_records_and_survive_restart() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let blob = reference(b"late shared content");
    let later = reference(b"content after restart");
    let first = message(1, Payload::Blob(blob));
    let second = message(2, Payload::Blob(blob));
    let third = message(3, Payload::Blob(later));
    append(root, &[first.clone(), second.clone(), third.clone()]);
    let mut index = ChainIndex::open(root).unwrap();
    assert!(
        !root.join("blobs").exists(),
        "opening indexes does not create blobs"
    );
    assert_eq!(
        index
            .content(first.id)
            .unwrap()
            .unwrap()
            .first()
            .unwrap()
            .state,
        ContentState::Missing
    );
    assert_eq!(
        index.refresh().unwrap().work.content_reads,
        2,
        "shared references are checked once"
    );
    assert_eq!(put(root, b"late shared content"), blob);
    let delta = index.refresh().unwrap();
    assert_eq!(
        delta.content_changed.into_iter().collect::<Vec<_>>(),
        vec![first.id, second.id]
    );
    assert!(delta.added.is_empty(), "blob-only update");
    assert_eq!(delta.work.records.records_decoded, 0);
    assert_eq!(delta.work.records.bytes_read, 0);
    assert_eq!(delta.work.content_reads, 2);
    assert_eq!(
        index
            .content(first.id)
            .unwrap()
            .unwrap()
            .first()
            .unwrap()
            .state,
        ContentState::Available
    );
    assert_eq!(
        index.refresh().unwrap().work.content_reads,
        1,
        "available dependencies leave the retry set"
    );
    drop(index);
    let mut index = ChainIndex::open(root).unwrap();
    assert_eq!(put(root, b"content after restart"), later);
    let delta = index.refresh().unwrap();
    assert_eq!(
        delta.content_changed.into_iter().collect::<Vec<_>>(),
        vec![third.id]
    );
    assert_eq!(delta.work.records.bytes_read, 0);
    assert_eq!(index.refresh().unwrap().work.content_reads, 0);
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "late content agrees with replay"
    );
    let before = index.content(third.id).unwrap();
    let _stats = index.rebuild().unwrap();
    assert_eq!(index.content(third.id).unwrap(), before);
}

#[test]
fn conflicts_retract_postings_and_pending_content_and_replays_stay_inert() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let blob = reference(b"never used after quarantine");
    let original = message(1, Payload::Blob(blob));
    append(root, std::slice::from_ref(&original));
    let mut index = ChainIndex::open(root).unwrap();
    let conflict = message(1, Payload::Inline(b"different".to_vec()));
    append(
        root,
        &[original.clone(), conflict.clone(), conflict.clone()],
    );
    let delta = index.refresh().unwrap();
    assert_eq!(
        delta.removed.into_iter().collect::<Vec<_>>(),
        vec![original.id]
    );
    assert!(delta.added.is_empty(), "conflicting ID stays absent");
    assert!(
        index
            .lookup(IndexKey::Actor(ActorId(17)), None, 10)
            .unwrap()
            .is_empty(),
        "author posting retracted"
    );
    assert!(
        index
            .lookup(IndexKey::Content(blob.id), None, 10)
            .unwrap()
            .is_empty(),
        "content posting retracted"
    );
    assert_eq!(index.content(original.id).unwrap(), None);
    assert_eq!(index.refresh().unwrap().work.content_reads, 0);
    assert_eq!(put(root, b"never used after quarantine"), blob);
    append(root, &[original.clone(), conflict]);
    let delta = index.refresh().unwrap();
    assert!(
        delta.added.is_empty() && delta.removed.is_empty() && delta.content_changed.is_empty(),
        "replay cannot revive quarantine"
    );
    assert_eq!(index.record_variants(original.id).unwrap().len(), 2);
    assert_eq!(index.stats(), CanonicalChain::read(root).unwrap().stats());
    assert!(
        index.verify_integrity().unwrap().index_matches,
        "conflict replay matches rebuild"
    );
}

#[test]
fn content_io_failure_does_not_consume_the_operation_frontier() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let old = message(1, Payload::Empty);
    append(root, std::slice::from_ref(&old));
    let mut index = ChainIndex::open(root).unwrap();
    let bytes = b"retry content";
    let blob = reference(bytes);
    let new = message(2, Payload::Blob(blob));
    append(root, std::slice::from_ref(&new));
    let blobs = BlobStore::new(root.join("blobs")).unwrap();
    let blocked = blobs.path_for(blake3::hash(bytes).as_bytes());
    std::fs::create_dir(&blocked).unwrap();
    assert!(index.refresh().is_err(), "backend read errors propagate");
    assert_eq!(index.get(new.id).unwrap(), None);
    assert_eq!(index.operations(None, 10).unwrap(), vec![old.id]);
    std::fs::remove_dir(blocked).unwrap();
    assert_eq!(put(root, bytes), blob);
    assert_eq!(index.refresh().unwrap().work.records.records_decoded, 1);
    drop(index);
    let index = ChainIndex::open(root).unwrap();
    assert_eq!(index.get(new.id).unwrap(), Some(new));
    assert_eq!(index.stats().accepted, 2);
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "retry recovers from old durable checkpoint"
    );
}

#[test]
fn completing_an_interrupted_record_updates_the_retained_index_once() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let op = message(1, Payload::Empty);
    let mut page = Page::new(0);
    page.add_record(0, encode_op(&op).unwrap());
    let encoded = encode_page(&page).unwrap();
    let cut = encoded.len().saturating_sub(1);
    let path = root.join("000000.eclog");
    std::fs::write(&path, encoded.get(..cut).unwrap()).unwrap();
    let mut index = ChainIndex::open(root).unwrap();
    assert_eq!(index.stats().incomplete_tails, 1);
    assert_eq!(index.get(op.id).unwrap(), None);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(encoded.get(cut..).unwrap())
        .unwrap();
    assert_eq!(
        index
            .refresh()
            .unwrap()
            .added
            .into_iter()
            .collect::<Vec<_>>(),
        vec![op.id]
    );
    assert_eq!(index.stats().incomplete_tails, 0);
    assert_eq!(index.get(op.id).unwrap(), Some(op));
    assert!(
        index.refresh().unwrap().added.is_empty(),
        "completion admitted once"
    );
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "completed record agrees with replay"
    );
}

#[test]
fn failed_checkpoint_publication_retains_reads_and_recovers_after_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let old = message(1, Payload::Empty);
    append(root, std::slice::from_ref(&old));
    let mut index = ChainIndex::open(root).unwrap();
    let published = std::fs::read(root.join("index-v1/root")).unwrap();
    let new = message(2, Payload::Empty);
    append(root, std::slice::from_ref(&new));
    let blocked = root.join("index-v1/root.next");
    std::fs::create_dir(&blocked).unwrap();
    assert!(index.refresh().is_err(), "publication failure propagates");
    assert_eq!(index.get(old.id).unwrap(), Some(old));
    assert_eq!(index.get(new.id).unwrap(), None);
    assert_eq!(
        std::fs::read(root.join("index-v1/root")).unwrap(),
        published
    );
    std::fs::remove_dir(blocked).unwrap();
    assert!(index.refresh().is_err(), "failed writers require reopening");
    assert!(
        index.rebuild().is_err(),
        "rebuild must also reopen a failed writer"
    );
    drop(index);
    let index = ChainIndex::open(root).unwrap();
    assert_eq!(index.get(new.id).unwrap(), Some(new));
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "unpublished pages do not lose changes"
    );
}
