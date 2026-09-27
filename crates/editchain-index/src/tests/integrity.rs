use std::io::Write as _;

use editchain_core::{BlobRef, ContentId, Payload};
use editchain_store::{format::Page, BlobStore, SegmentStore};

use crate::{ChainIndex, ContentState, IndexKey};

use super::{append, message, put, reference, source_bytes};

#[test]
fn integrity_reports_all_record_and_content_gaps_including_conflict_variants() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let missing = message(1, Payload::Blob(reference(b"absent")));
    let corrupt_ref = reference(b"correct bytes");
    let corrupt = message(2, Payload::Blob(corrupt_ref));
    let local_ref = BlobRef {
        id: ContentId::Hash128([1; 16]),
        len: 5,
    };
    let unsupported = message(3, Payload::Blob(local_ref));
    let conflict_missing = message(4, Payload::Blob(reference(b"conflicting record")));
    let conflict_other = message(4, Payload::Empty);
    let valid = put(root, b"correct length");
    let wrong_length = message(
        5,
        Payload::Blob(BlobRef {
            len: valid.len.saturating_add(1),
            ..valid
        }),
    );
    append(
        root,
        &[
            missing.clone(),
            corrupt.clone(),
            unsupported.clone(),
            conflict_missing.clone(),
            conflict_other,
            wrong_length.clone(),
        ],
    );
    let blobs = BlobStore::new(root.join("blobs")).unwrap();
    std::fs::write(
        blobs.path_for(blake3::hash(b"correct bytes").as_bytes()),
        b"corrupt bytes",
    )
    .unwrap();
    let mut page = Page::new(1);
    page.add_record(0, Vec::new());
    let mut writer = SegmentStore::open(root).unwrap();
    writer.append_page(&page).unwrap();
    let path = root.join(format!("{:06}.eclog", writer.segment_sequence()));
    drop(writer);
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"EC")
        .unwrap();
    let original = source_bytes(root);
    let mut index = ChainIndex::open(root).unwrap();
    let report = index.verify_integrity().unwrap();
    assert!(
        report.index_matches,
        "record and content problems do not invalidate a faithful derived index"
    );
    assert!(!report.is_clean(), "integrity gaps are explicit");
    assert_eq!(report.chain.quarantined, 2);
    assert_eq!(report.chain.undecodable, 1);
    assert_eq!(report.chain.incomplete_tails, 1);
    assert_eq!(
        report
            .content_issues
            .iter()
            .map(|issue| (issue.operation, issue.state))
            .collect::<Vec<_>>(),
        vec![
            (missing.id, ContentState::Missing),
            (corrupt.id, ContentState::Corrupt),
            (unsupported.id, ContentState::Unresolvable),
            (conflict_missing.id, ContentState::Missing),
            (wrong_length.id, ContentState::Corrupt),
        ]
    );
    assert_eq!(
        index.lookup(IndexKey::Content(valid.id), None, 10).unwrap(),
        vec![wrong_length.id],
        "references remain indexed even when invalid"
    );
    let _stats = index.rebuild().unwrap();
    assert_eq!(
        index.verify_integrity().unwrap().content_issues,
        report.content_issues
    );
    assert_eq!(
        source_bytes(root),
        original,
        "integrity and rebuild preserve every canonical byte"
    );
}

#[test]
fn stale_results_and_external_blob_damage_are_detected_without_mutating_queries() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let blob = put(root, b"verified content");
    let op = message(1, Payload::Blob(blob));
    append(root, std::slice::from_ref(&op));
    let mut index = ChainIndex::open(root).unwrap();
    let newer = message(2, Payload::Empty);
    append(root, std::slice::from_ref(&newer));
    assert!(
        !index.verify_integrity().unwrap().index_matches,
        "audit detects an unrefreshed append"
    );
    assert_eq!(
        index.get(newer.id).unwrap(),
        None,
        "audit does not refresh queries"
    );
    drop(index.refresh().unwrap());
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "refresh restores agreement"
    );
    let blobs = BlobStore::new(root.join("blobs")).unwrap();
    std::fs::write(
        blobs.path_for(blake3::hash(b"verified content").as_bytes()),
        b"externally damaged",
    )
    .unwrap();
    assert_eq!(
        index.refresh().unwrap().work.content_reads,
        0,
        "incremental reads trust previously verified immutable blobs"
    );
    let report = index.verify_integrity().unwrap();
    assert!(
        !report.index_matches,
        "full audit hashes available blobs again"
    );
    assert_eq!(
        report.content_issues.first().unwrap().state,
        ContentState::Corrupt
    );
    let _stats = index.rebuild().unwrap();
    assert_eq!(
        index
            .content(op.id)
            .unwrap()
            .unwrap()
            .first()
            .unwrap()
            .state,
        ContentState::Corrupt
    );
    assert!(
        index.verify_integrity().unwrap().index_matches,
        "rebuild reports damaged content faithfully"
    );
    std::fs::write(
        blobs.path_for(blake3::hash(b"verified content").as_bytes()),
        b"verified content",
    )
    .unwrap();
    assert_eq!(
        index
            .refresh()
            .unwrap()
            .content_changed
            .into_iter()
            .collect::<Vec<_>>(),
        vec![op.id]
    );
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "repaired content is retried"
    );
}

#[test]
fn integrity_detects_missing_postings_even_in_a_valid_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let op = message(1, Payload::Empty);
    append(root, std::slice::from_ref(&op));
    let mut index = ChainIndex::open(root).unwrap();
    drop(index.state.postings.remove(&IndexKey::Actor(op.actor)));
    index.state = index.storage.commit(&index.state).unwrap();
    let report = index.verify_integrity().unwrap();
    assert!(
        !report.index_matches,
        "checksum-valid semantic damage is detected"
    );
    assert!(
        report.index_error.is_none(),
        "pages themselves are readable"
    );
    let _stats = index.rebuild().unwrap();
    assert_eq!(
        index.lookup(IndexKey::Actor(op.actor), None, 10).unwrap(),
        vec![op.id]
    );
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "rebuild restores postings"
    );
}

#[test]
fn corrupt_lazy_pages_and_roots_can_be_rebuilt_without_touching_sources() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let op = message(1, Payload::Empty);
    append(root, std::slice::from_ref(&op));
    let original = source_bytes(root);
    let mut index = ChainIndex::open(root).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("index-v1/pages"))
        .unwrap()
        .write_all(&[0xff])
        .unwrap();
    assert!(
        index.get(op.id).is_err(),
        "lazy faults must not become absent operations"
    );
    let report = index.verify_integrity().unwrap();
    assert!(!report.index_matches, "damaged pages differ");
    assert!(
        report.index_error.is_some(),
        "derived corruption is reported separately from source corruption"
    );
    let _stats = index.rebuild().unwrap();
    assert_eq!(index.get(op.id).unwrap(), Some(op.clone()));
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "fresh pages restore integrity"
    );
    drop(index);
    std::fs::write(root.join("index-v1/root"), b"bad root").unwrap();
    assert!(
        ChainIndex::open(root).is_err(),
        "invalid root requires explicit rebuild"
    );
    let index = ChainIndex::rebuild_at(root).unwrap();
    assert_eq!(index.get(op.id).unwrap(), Some(op));
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "rebuild ignores the damaged root"
    );
    assert_eq!(source_bytes(root), original);
}

#[test]
fn failed_rebuild_keeps_the_last_query_state_and_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let op = message(1, Payload::Empty);
    append(root, std::slice::from_ref(&op));
    let mut index = ChainIndex::open(root).unwrap();
    let checkpoint = std::fs::read(root.join("index-v1/root")).unwrap();
    let path = root.join("000000.eclog");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"bad framing that is not an EC02 header").unwrap();
    assert!(
        index.rebuild().is_err(),
        "rebuild must reject canonical corruption"
    );
    assert!(
        index.verify_integrity().is_err(),
        "canonical corruption is an error, not an empty report"
    );
    assert_eq!(index.get(op.id).unwrap(), Some(op.clone()));
    assert_eq!(
        std::fs::read(root.join("index-v1/root")).unwrap(),
        checkpoint
    );
    std::fs::write(path, original).unwrap();
    let _stats = index.rebuild().unwrap();
    assert_eq!(index.get(op.id).unwrap(), Some(op));
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "source repair enables rebuild"
    );
}

#[test]
fn warm_query_pages_do_not_hide_persisted_page_or_root_damage_from_the_audit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let op = message(1, Payload::Empty);
    append(root, std::slice::from_ref(&op));
    let mut index = ChainIndex::open(root).unwrap();
    assert_eq!(index.get(op.id).unwrap(), Some(op.clone()));
    std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("index-v1/pages"))
        .unwrap()
        .write_all(&[0xff])
        .unwrap();
    assert_eq!(
        index.get(op.id).unwrap(),
        Some(op),
        "queries retain their decoded snapshot"
    );
    assert!(
        index.verify_integrity().unwrap().index_error.is_some(),
        "audit uses cold published pages"
    );
    let _stats = index.rebuild().unwrap();
    std::fs::write(root.join("index-v1/root"), b"damaged root").unwrap();
    assert!(
        index.verify_integrity().unwrap().index_error.is_some(),
        "audit also reloads the root"
    );
}
