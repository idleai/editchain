//! Admission work, uncertain batch outcomes and exact storage evidence.

use std::{cell::Cell, io, rc::Rc};

use blake3 as _;
use crc as _;
use editchain_index_pages as _;
use postcard as _;
use proptest as _;
use serde as _;
use serde_json as _;

use editchain_core::{
    ActorId, Admission, Clock, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef,
    Tags,
};
use editchain_store::format::encode_op;
use editchain_store::{
    AppendLog, CanonicalChain, LogReadStats, LogStore, RecordVisitor, SegmentOptions, SegmentStore,
};

fn record(sequence: u64, content: &[u8]) -> io::Result<Vec<u8>> {
    encode_op(&Op {
        source: Some(editchain_core::SourceId::new(NodeId(1), 0, sequence)),
        id: OpId::new(NodeId(1), 0, sequence),
        parents: ParentSet::None,
        actor: ActorId(7),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(content.to_vec()),
            content_type: Payload::Empty,
        }),
    })
    .map_err(io::Error::other)
}

#[derive(Default)]
struct ObservedLog {
    records: Vec<Vec<u8>>,
    reads: Cell<usize>,
    syncs: Cell<usize>,
    writes: usize,
    fail_at: Option<usize>,
    retain_failed: bool,
    fail_sync: Rc<Cell<bool>>,
}

impl AppendLog for ObservedLog {
    fn visit_records(&self, visitor: &mut RecordVisitor<'_>) -> io::Result<LogReadStats> {
        self.reads.set(self.reads.get().saturating_add(1));
        for bytes in &self.records {
            visitor(0, bytes)?;
        }
        Ok(LogReadStats::default())
    }

    fn append_record(&mut self, _flags: u8, encoded: &[u8]) -> io::Result<()> {
        self.writes = self.writes.saturating_add(1);
        let fail = self.fail_at == Some(self.writes);
        if !fail || self.retain_failed {
            self.records.push(encoded.to_vec());
        }
        if fail {
            Err(io::Error::other("injected unknown write outcome"))
        } else {
            Ok(())
        }
    }

    fn sync(&self) -> io::Result<()> {
        self.syncs.set(self.syncs.get().saturating_add(1));
        if self.fail_sync.get() {
            Err(io::Error::other("injected durability failure"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn thousands_of_single_and_batched_appends_replay_once_per_writer_lifetime() {
    let mut writer = LogStore::new(ObservedLog::default());
    for sequence in 0..2048 {
        assert_eq!(
            writer
                .append_encoded(&record(sequence, b"single").unwrap())
                .unwrap(),
            Admission::Accepted
        );
    }
    let records: Vec<_> = (2048..4096)
        .map(|seq| record(seq, b"batch").unwrap())
        .collect();
    let refs: Vec<_> = records.iter().map(Vec::as_slice).collect();
    assert_eq!(
        writer.append_encoded_batch(&refs).unwrap(),
        vec![Admission::Accepted; 2048]
    );
    let log = writer.into_inner();
    assert_eq!(
        log.reads.get(),
        1,
        "admission work cannot rescan growing history"
    );
    assert_eq!(log.records.len(), 4096);
    let mut reopened = LogStore::new(log);
    assert_eq!(
        reopened.append_encoded(records.first().unwrap()).unwrap(),
        Admission::Duplicate
    );
    assert_eq!(reopened.into_inner().reads.get(), 2);
}

#[test]
fn uncertain_batch_prefixes_are_replayed_without_optimistic_acknowledgements() {
    let records: Vec<_> = (0..8).map(|seq| record(seq, b"exact").unwrap()).collect();
    let refs: Vec<_> = records.iter().map(Vec::as_slice).collect();
    for retained in [false, true] {
        let log = ObservedLog {
            fail_at: Some(3),
            retain_failed: retained,
            ..ObservedLog::default()
        };
        let mut writer = LogStore::new(log);
        assert!(writer.append_encoded_batch(&refs).is_err());
        let admissions = writer.append_encoded_batch(&refs).unwrap();
        let committed = if retained { 3 } else { 2 };
        assert_eq!(
            admissions
                .iter()
                .filter(|&&a| a == Admission::Duplicate)
                .count(),
            committed
        );
        let log = writer.into_inner();
        assert_eq!(log.reads.get(), 2, "uncertain state must be discarded");
        assert_eq!(log.syncs.get(), 1, "readable prefix must be fenced");
        assert_eq!(
            log.records, records,
            "retry preserves exact physical evidence"
        );
    }
}

#[test]
fn failed_duplicate_fence_invalidates_the_cached_admission_state() {
    let fail = Rc::new(Cell::new(false));
    let mut writer = LogStore::new(ObservedLog {
        fail_sync: Rc::clone(&fail),
        ..ObservedLog::default()
    });
    let bytes = record(1, b"persisted").unwrap();
    assert_eq!(writer.append_encoded(&bytes).unwrap(), Admission::Accepted);
    fail.set(true);
    assert!(writer.append_encoded(&bytes).is_err());
    fail.set(false);
    assert_eq!(writer.append_encoded(&bytes).unwrap(), Admission::Duplicate);
    let log = writer.into_inner();
    assert_eq!(log.reads.get(), 2);
    assert_eq!(log.records, vec![bytes]);
}

#[test]
fn batch_conflicts_keep_alternate_encodings_and_duplicates_inert() {
    let original = record(1, b"\xff\0evidence").unwrap();
    assert_eq!(original.get(48), Some(&1));
    let mut alternate = original.get(..48).unwrap().to_vec();
    alternate.extend_from_slice(&[0x81, 0]);
    alternate.extend_from_slice(original.get(49..).unwrap());
    let records = [
        original.as_slice(),
        original.as_slice(),
        &alternate,
        &alternate,
    ];
    let mut writer = LogStore::new(ObservedLog::default());
    assert_eq!(
        writer.append_encoded_batch(&records).unwrap(),
        vec![
            Admission::Accepted,
            Admission::Duplicate,
            Admission::Conflict,
            Admission::Duplicate
        ]
    );
    let snapshot = writer.snapshot().unwrap();
    assert_eq!(snapshot.stats().accepted, 0);
    assert_eq!(snapshot.stats().quarantined, 2);
    assert_eq!(writer.into_inner().records, vec![original, alternate]);
}

#[test]
fn invalid_or_empty_batches_write_nothing_and_do_not_load_history() {
    let mut writer = LogStore::new(ObservedLog::default());
    assert!(writer.append_encoded_batch(&[]).unwrap().is_empty());
    assert!(writer
        .append_encoded_batch(&[&record(1, b"valid").unwrap(), b"\xff"])
        .is_err());
    let log = writer.into_inner();
    assert!(log.records.is_empty());
    assert_eq!(log.reads.get(), 0);
}

#[test]
fn filesystem_batches_pack_pages_preserving_record_order_and_flags() {
    let directory = tempfile::tempdir().unwrap();
    let mut writer = SegmentStore::open_with_options(
        directory.path(),
        SegmentOptions {
            max_segment_bytes: 64,
        },
    )
    .unwrap();
    let records: Vec<_> = (0u8..100).map(|n| (n, vec![n; 3])).collect();
    let refs: Vec<_> = records
        .iter()
        .map(|(flags, bytes)| (*flags, bytes.as_slice()))
        .collect();
    writer.append_records(&refs).unwrap();
    assert_eq!(
        writer.segment_sequence(),
        49,
        "two records fit in each 64-byte EC03 frame"
    );
    let mut actual = Vec::new();
    let _stats = writer
        .visit_records(&mut |flags, bytes| {
            actual.push((flags, bytes.to_vec()));
            Ok(())
        })
        .unwrap();
    assert_eq!(actual, records);
    drop(writer);
    let mut writer = LogStore::new(SegmentStore::open(directory.path()).unwrap());
    let next = record(100, b"next complete operation").unwrap();
    assert_eq!(writer.append_encoded(&next).unwrap(), Admission::Accepted);
    assert_eq!(
        CanonicalChain::read(directory.path())
            .unwrap()
            .stats()
            .accepted,
        1
    );
}
