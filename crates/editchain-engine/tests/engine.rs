//! Public facade contracts: exact evidence, replay, recovery, and late content.

use std::io;

use editchain_core as _;
use editchain_engine::{
    decode_op, encode_op, ActorId, Admission, BlobRef, BlobResolution, ChainSnapshot, Clock,
    ContentId, Engine, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef,
    SessionId, Tags,
};
use editchain_store::{format::Page, SegmentStore};

fn operation(sequence: u64, bytes: &[u8]) -> Op {
    Op {
        id: OpId::new(NodeId(1), 7, sequence),
        parents: ParentSet::One(OpId::new(NodeId(9), 3, 2)),
        actor: ActorId(u64::MAX),
        clock: Clock::None,
        scope: ScopeRef::Session(SessionId(u64::MAX)),
        tags: Tags::MESSAGE | Tags::SOURCE_TIME_UNKNOWN,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(bytes.to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

#[test]
fn conflicts_retain_all_bytes_across_reopen_and_replay() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let original = operation(1, b"original\0\xff\r\n");
    let revised = operation(1, b"different evidence");
    let other = operation(2, b"unaffected");
    let bytes = encode_op(&original).unwrap();
    let conflict = encode_op(&revised).unwrap();

    assert_eq!(engine.append_encoded(&bytes).unwrap(), Admission::Accepted);
    assert_eq!(engine.append_encoded(&bytes).unwrap(), Admission::Duplicate);
    let before_conflict = engine.snapshot().unwrap();
    assert_eq!(before_conflict.get(original.id), Some(&original));
    assert_eq!(
        engine.append_encoded(&conflict).unwrap(),
        Admission::Conflict
    );
    assert_eq!(engine.append(&other).unwrap(), Admission::Accepted);

    let reopened = Engine::open(directory.path()).unwrap();
    for retained in [&bytes, &conflict] {
        assert_eq!(
            reopened.append_encoded(retained).unwrap(),
            Admission::Duplicate
        );
    }
    let snapshot = reopened.snapshot().unwrap();
    assert_eq!(snapshot.get(original.id), None);
    assert_eq!(snapshot.operations().collect::<Vec<_>>(), vec![&other]);
    let variants = snapshot.evidence().conflicts().next().unwrap().1;
    assert_eq!(variants.len(), 2);
    assert!(
        variants.contains(&bytes),
        "original encoded evidence survives"
    );
    assert!(variants.contains(&conflict), "conflict evidence survives");
    assert_eq!(snapshot.stats().records, 3);
    assert_eq!(snapshot.stats().quarantined, 2);
    assert_eq!(before_conflict.get(original.id), Some(&original));

    let replica_directory = tempfile::tempdir().unwrap();
    let replica = Engine::open(replica_directory.path()).unwrap();
    for (_, encoded) in snapshot.evidence().evidence() {
        let _admission = replica.append_encoded(encoded).unwrap();
    }
    assert_eq!(replica.snapshot().unwrap().evidence(), snapshot.evidence());
}

#[test]
fn equal_decoded_values_with_different_encodings_are_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let original = operation(1, b"bytes are authoritative");
    let canonical = encode_op(&original).unwrap();
    assert_eq!(canonical.first(), Some(&1));
    // Postcard accepts an overlong varint for the first NodeId. These bytes
    // decode identically, but normalization would erase conflict evidence.
    let mut alternate = vec![0x81, 0];
    alternate.extend_from_slice(canonical.get(1..).unwrap());
    assert_eq!(decode_op(&alternate).unwrap(), original);
    assert_eq!(
        engine.append_encoded(&alternate).unwrap(),
        Admission::Accepted
    );
    assert_eq!(engine.append(&original).unwrap(), Admission::Conflict);
    assert_eq!(
        engine.append_encoded(&alternate).unwrap(),
        Admission::Duplicate
    );
    let snapshot = engine.snapshot().unwrap();
    assert_eq!(snapshot.get(original.id), None);
    assert_eq!(snapshot.evidence().evidence().count(), 2);
    assert!(
        snapshot
            .evidence()
            .evidence()
            .any(|(_, bytes)| bytes == alternate),
        "the original noncanonical encoding is retained verbatim"
    );
}

#[test]
fn late_blobs_resolve_without_reopening_and_preserve_record_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let bytes = b"\0binary evidence\xff\r\n";
    let hash = *blake3::hash(bytes).as_bytes();
    let reference = BlobRef {
        id: ContentId::Hash256(hash),
        len: u32::try_from(bytes.len()).unwrap(),
    };
    let mut record = operation(1, b"");
    record.kind = OpKind::Message(MessageOp {
        content: Payload::Blob(reference),
        content_type: Payload::Empty,
    });
    assert_eq!(engine.append(&record).unwrap(), Admission::Accepted);
    let original = engine.snapshot().unwrap();
    assert_eq!(
        engine.resolve_blob(&reference).unwrap(),
        BlobResolution::Missing
    );
    assert!(
        !directory.path().join("blobs").exists(),
        "reads create no blob directory"
    );
    assert_eq!(engine.store_blob(bytes).unwrap(), reference);
    assert_eq!(engine.store_blob(bytes).unwrap(), reference);
    assert_eq!(
        engine.resolve_content(reference.id).unwrap(),
        BlobResolution::Found(bytes.to_vec())
    );
    assert_eq!(
        engine.resolve_payload(&Payload::Blob(reference)).unwrap(),
        BlobResolution::Found(bytes.to_vec())
    );
    assert_eq!(engine.snapshot().unwrap().evidence(), original.evidence());
    assert_eq!(
        engine
            .resolve_blob(&BlobRef {
                len: 0,
                ..reference
            })
            .unwrap(),
        BlobResolution::Corrupt
    );
    assert_eq!(
        engine
            .resolve_content(ContentId::Local {
                node: NodeId(1),
                seq: 1
            })
            .unwrap(),
        BlobResolution::Unresolvable
    );
    assert_eq!(
        engine.resolve_content(ContentId::Hash128([0; 16])).unwrap(),
        BlobResolution::Unresolvable
    );

    let blob_path = directory
        .path()
        .join("blobs")
        .join(blake3::Hash::from(hash).to_hex().as_str());
    std::fs::write(&blob_path, b"corrupt").unwrap();
    assert_eq!(
        engine.resolve_blob(&reference).unwrap(),
        BlobResolution::Corrupt
    );
    assert_eq!(
        engine.store_blob(bytes).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(std::fs::read(blob_path).unwrap(), b"corrupt");
}

#[test]
fn read_only_snapshots_and_invalid_appends_do_not_create_history() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");
    assert_eq!(ChainSnapshot::read(&missing).unwrap().stats().records, 0);
    assert!(
        !missing.exists(),
        "read-only snapshots never create a chain"
    );
    let engine = Engine::open(&missing).unwrap();
    let mut trailing = encode_op(&operation(1, b"valid")).unwrap();
    trailing.push(0);
    for bytes in [Vec::new(), vec![0xff], trailing] {
        assert_eq!(
            engine.append_encoded(&bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(engine.snapshot().unwrap().stats().records, 0);
    assert!(
        !missing.join("000000.eclog").exists(),
        "invalid input writes no segment"
    );
}

#[test]
fn handles_observe_other_writers_and_retry_after_lock_failure() {
    let directory = tempfile::tempdir().unwrap();
    let first = Engine::open(directory.path()).unwrap();
    let second = Engine::open(directory.path()).unwrap();
    let original = operation(1, b"first");
    let lock = SegmentStore::open(directory.path()).unwrap();
    assert_eq!(
        first.append(&original).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(lock);
    assert_eq!(second.append(&original).unwrap(), Admission::Accepted);
    assert_eq!(first.append(&original).unwrap(), Admission::Duplicate);
    assert_eq!(
        first.append(&operation(1, b"changed")).unwrap(),
        Admission::Conflict
    );
    assert_eq!(second.snapshot().unwrap().get(original.id), None);
}

#[test]
fn interrupted_tails_and_unknown_records_survive_subsequent_appends() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let original = operation(1, b"before interruption");
    assert_eq!(engine.append(&original).unwrap(), Admission::Accepted);
    let segment = directory.path().join("000000.eclog");
    let mut interrupted = std::fs::read(&segment).unwrap();
    interrupted.extend_from_slice(&[100, 0, 0, 0, 0, 42]);
    std::fs::write(&segment, &interrupted).unwrap();
    let mut store = SegmentStore::open(directory.path()).unwrap();
    let mut future = Page::new(store.segment_sequence());
    future.add_record(0, vec![0xff]);
    store.append_page(&future).unwrap();
    drop(store);
    assert_eq!(engine.append(&original).unwrap(), Admission::Duplicate);
    assert_eq!(
        engine.append(&operation(2, b"after interruption")).unwrap(),
        Admission::Accepted
    );
    let snapshot = engine.snapshot().unwrap();
    assert_eq!(snapshot.stats().incomplete_tails, 1);
    assert_eq!(snapshot.stats().undecodable, 1);
    assert_eq!(snapshot.stats().accepted, 2);
    assert_eq!(std::fs::read(segment).unwrap(), interrupted);
    assert_eq!(snapshot.get(original.id), Some(&original));
}
