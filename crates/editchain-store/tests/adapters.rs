//! Portable admission, exact recovery and filesystem packing contracts.

use std::io;

use crc as _;
use editchain_index as _;
use postcard as _;
use proptest as _;
use serde as _;

use editchain_core::{
    ActorId, Admission, Clock, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef,
    Tags,
};
use editchain_store::format::{decode_op, encode_op, encode_page, Page};
use editchain_store::{
    AppendLog, BlobReader, BlobResolution, BlobSource, BlobStorage, BlobStore, CanonicalChain,
    CanonicalTail, LogReadStats, LogStore, SegmentOptions, SegmentStore,
};

fn operation(sequence: u64, bytes: &[u8]) -> Op {
    Op {
        id: OpId::new(NodeId(1), 99, sequence),
        parents: ParentSet::None,
        actor: ActorId(u64::MAX),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(bytes.to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

#[test]
fn every_interrupted_page_prefix_preserves_original_evidence_and_locations() {
    let original = operation(7, b"\xff\0original\r\n");
    let encoded = encode_op(&original).unwrap();
    // Same decoded identity, distinct byte evidence. Never normalize on recovery.
    let mut alternate = vec![0x81, 0];
    alternate.extend_from_slice(encoded.get(1..).unwrap());
    assert_eq!(decode_op(&alternate).unwrap(), original);
    let later = encode_op(&operation(8, b"after interrupted write")).unwrap();
    let mut page = Page::new(73);
    page.add_record(0xa5, alternate.clone());
    page.add_record(0x5a, later.clone());
    let interrupted = encode_page(&page).unwrap();

    for cut in 0..=interrupted.len() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = LogStore::new(SegmentStore::open(directory.path()).unwrap());
        assert_eq!(store.append_encoded(&encoded).unwrap(), Admission::Accepted);
        drop(store);
        let original_location = CanonicalChain::read(directory.path())
            .unwrap()
            .located_ops()
            .next()
            .unwrap()
            .1
            .unwrap();
        let path = directory.path().join("000000.eclog");
        let mut before = std::fs::read(&path).unwrap();
        before.extend_from_slice(interrupted.get(..cut).unwrap());
        std::fs::write(&path, &before).unwrap();
        let mut tail = CanonicalTail::open(directory.path()).unwrap();
        let mut recovered = LogStore::new(SegmentStore::open(directory.path()).unwrap());
        let _admission = recovered.append_encoded(&alternate).unwrap();
        let _admission = recovered.append_encoded(&later).unwrap();
        assert_eq!(
            recovered.append_encoded(&encoded).unwrap(),
            Admission::Duplicate
        );
        let snapshot = recovered.snapshot().unwrap();
        assert_eq!(snapshot.evidence().evidence().count(), 3, "cut {cut}");
        assert_eq!(snapshot.get(original.id), None);
        assert_eq!(snapshot.stats().quarantined, 2);
        assert_eq!(
            editchain_store::read_encoded_at(directory.path(), original_location).unwrap(),
            encoded
        );
        assert!(
            std::fs::read(&path).unwrap().starts_with(&before),
            "prefix at cut {cut} must remain unchanged"
        );
        drop(tail.drain().unwrap());
        assert_eq!(tail.chain().evidence(), snapshot.evidence());
        drop(recovered);
        let reopened = LogStore::new(SegmentStore::open(directory.path()).unwrap());
        assert_eq!(reopened.snapshot().unwrap().evidence(), snapshot.evidence());
    }
}

#[test]
fn repeated_writer_lifetimes_pack_records_and_size_rotation_preserves_flags() {
    let directory = tempfile::tempdir().unwrap();
    let options = SegmentOptions {
        max_segment_bytes: 64,
    };
    for value in 0u8..100 {
        let mut store = SegmentStore::open_with_options(directory.path(), options).unwrap();
        store.append_record(value, &[value; 3]).unwrap();
    }
    let store = SegmentStore::open_with_options(directory.path(), options).unwrap();
    let files = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "eclog")
        })
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 25);
    assert!(
        files
            .iter()
            .all(|path| std::fs::metadata(path).unwrap().len() == 64),
        "four framed records share each segment"
    );
    let mut observed = Vec::new();
    let stats = store
        .visit_records(&mut |flags, bytes| {
            observed.push((flags, bytes.to_vec()));
            Ok(())
        })
        .unwrap();
    assert_eq!(stats.incomplete_tails, 0);
    assert_eq!(
        observed,
        (0u8..100)
            .map(|value| (value, vec![value; 3]))
            .collect::<Vec<_>>()
    );
}

#[test]
fn oversized_pages_stay_whole_and_corrupt_frontiers_are_never_extended() {
    let directory = tempfile::tempdir().unwrap();
    let options = SegmentOptions {
        max_segment_bytes: 8,
    };
    let mut store = SegmentStore::open_with_options(directory.path(), options).unwrap();
    store.append_record(3, &[42; 100]).unwrap();
    store.append_record(4, b"next").unwrap();
    assert_eq!(store.segment_sequence(), 1);
    drop(store);
    let corrupt = b"EC99future framing";
    let path = directory.path().join("000001.eclog");
    std::fs::write(&path, corrupt).unwrap();
    assert_eq!(
        SegmentStore::open(directory.path()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(std::fs::read(path).unwrap(), corrupt);
    assert!(
        !directory.path().join("000002.eclog").exists(),
        "corrupt framing cannot be bypassed"
    );
}

/// A backend wrapper simulating a lost acknowledgement and failed durability
/// fence. It demonstrates that admission depends only on the public adapter.
struct UncertainLog {
    inner: SegmentStore,
    fail_sync: bool,
}

impl AppendLog for UncertainLog {
    fn visit_records(
        &self,
        visitor: &mut editchain_store::RecordVisitor<'_>,
    ) -> io::Result<LogReadStats> {
        self.inner.visit_records(visitor)
    }
    fn append_record(&mut self, flags: u8, bytes: &[u8]) -> io::Result<()> {
        self.inner.append_record(flags, bytes)?;
        Err(io::Error::other("lost append acknowledgement"))
    }
    fn sync(&self) -> io::Result<()> {
        if self.fail_sync {
            Err(io::Error::other("durability fence failed"))
        } else {
            self.inner.sync()
        }
    }
}

#[test]
fn uncertain_commits_and_conflicts_are_not_acknowledged_until_sync_succeeds() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = LogStore::new(UncertainLog {
        inner: SegmentStore::open(directory.path()).unwrap(),
        fail_sync: false,
    });
    for bytes in [b"first".as_slice(), b"conflict".as_slice()] {
        let encoded = encode_op(&operation(1, bytes)).unwrap();
        assert!(
            store.append_encoded(&encoded).is_err(),
            "commit without acknowledgement is reported as failure"
        );
        let mut adapter = store.into_inner();
        adapter.fail_sync = true;
        store = LogStore::new(adapter);
        assert!(
            store.append_encoded(&encoded).is_err(),
            "readable duplicate must not bypass sync failure"
        );
        let mut adapter = store.into_inner();
        adapter.fail_sync = false;
        store = LogStore::new(adapter);
        assert_eq!(
            store.append_encoded(&encoded).unwrap(),
            Admission::Duplicate
        );
    }
    assert_eq!(store.snapshot().unwrap().stats().quarantined, 2);
    assert_eq!(store.snapshot().unwrap().stats().records, 2);
}

#[test]
fn blob_adapters_distinguish_absence_io_corruption_and_late_content() {
    let directory = tempfile::tempdir().unwrap();
    let reader = BlobReader::open(directory.path()).unwrap();
    let bytes = b"\0\xffexact bytes\r\n";
    let id = editchain_core::ContentId::Hash256(*blake3::hash(bytes).as_bytes());
    assert_eq!(reader.read_content(id).unwrap(), BlobResolution::Missing);
    let mut writer = BlobStore::new(directory.path().join("blobs")).unwrap();
    let reference = writer.put(bytes).unwrap();
    assert_eq!(reference.id, id);
    assert_eq!(
        reader.read_blob(&reference).unwrap(),
        BlobResolution::Found(bytes.to_vec())
    );
    let path = writer.path_for(blake3::hash(bytes).as_bytes());
    std::fs::write(&path, b"bad bytes").unwrap();
    assert_eq!(
        reader.read_blob(&reference).unwrap(),
        BlobResolution::Corrupt
    );
    assert!(
        writer.put(bytes).is_err(),
        "conflicting existing bytes cannot be overwritten"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"bad bytes");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(path).unwrap();
    assert!(
        reader.read_blob(&reference).is_err(),
        "backend IO failure is not missing content"
    );
}

#[test]
fn failed_layout_publication_recovers_without_acknowledging_or_rewriting_records() {
    let directory = tempfile::tempdir().unwrap();
    let writer = SegmentStore::open(directory.path()).unwrap();
    let mut log = LogStore::new(writer);
    let marker = directory.path().join(".segment-layout");
    std::fs::create_dir(&marker).unwrap();
    let first = encode_op(&operation(1, b"written before layout failure")).unwrap();
    assert!(
        log.append_encoded(&first).is_err(),
        "publication error is not acknowledged"
    );
    let retained = std::fs::read(directory.path().join("000000.eclog")).unwrap();
    drop(log);
    std::fs::remove_dir(&marker).unwrap();
    let mut reopened = LogStore::new(SegmentStore::open(directory.path()).unwrap());
    assert_eq!(
        reopened.append_encoded(&first).unwrap(),
        Admission::Duplicate
    );
    assert_eq!(
        reopened
            .append_encoded(&encode_op(&operation(2, b"next")).unwrap())
            .unwrap(),
        Admission::Accepted
    );
    drop(reopened);
    assert_eq!(
        std::fs::read(directory.path().join("000000.eclog")).unwrap(),
        retained
    );
    assert_eq!(
        SegmentStore::open(directory.path())
            .unwrap()
            .segment_sequence(),
        1
    );
}
