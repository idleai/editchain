#![doc = "Frame encoding round-trip tests."]

use crc as _;
use postcard as _;
use proptest as _;
use serde as _;

use editchain_codec::frame::{
    decode_ec03, decode_op, detect_format, encode_ec03, encode_op, Ec03Frame, FrameFormat,
    EC03_FORMAT_VERSION,
};
use editchain_core::*;

#[test]
#[expect(clippy::panic, reason = "test assertion")]
fn round_trip_message_op() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 42),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_700_000_000_000),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"hello world".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    let encoded = encode_op(&op).unwrap();
    let decoded: Op = decode_op(&encoded).unwrap();

    assert_eq!(op.id, decoded.id);
    assert_eq!(op.actor, decoded.actor);
    assert_eq!(op.clock, decoded.clock);
    assert_eq!(op.tags, decoded.tags);

    match (&op.kind, &decoded.kind) {
        (OpKind::Message(a), OpKind::Message(b)) => {
            assert_eq!(a.content, b.content);
        }
        _ => panic!("kind mismatch"),
    }
}

#[test]
#[expect(clippy::panic, reason = "test assertion")]
fn round_trip_file_op() {
    let op = Op {
        id: OpId::new(NodeId(2), 1, 7),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::Lamport(99),
        scope: ScopeRef::File(PathId(42)),
        tags: Tags::FILE,
        kind: OpKind::File(FileOp {
            path: PathId(42),
            stage: FileStage::Applied,
            base: None,
            after: Some(ContentId::Hash128([0xAB; 16])),
            edit: FileEdit::None,
        }),
    };

    let encoded = encode_op(&op).unwrap();
    let decoded: Op = decode_op(&encoded).unwrap();

    assert_eq!(op.id, decoded.id);
    match (&op.kind, &decoded.kind) {
        (OpKind::File(a), OpKind::File(b)) => {
            assert_eq!(a.path, b.path);
            assert_eq!(a.stage, b.stage);
            assert_eq!(a.after, b.after);
        }
        _ => panic!("kind mismatch"),
    }
}

#[test]
fn ec03_round_trip_empty() {
    let frame = Ec03Frame::new(0, 0);
    let encoded = encode_ec03(&frame);
    let decoded = decode_ec03(&encoded).unwrap();

    assert_eq!(decoded.format_version, EC03_FORMAT_VERSION);
    assert_eq!(decoded.page_sequence, 0);
    assert_eq!(decoded.commit_generation, 0);
    assert_eq!(decoded.records.len(), 0);
    assert_eq!(decoded.record_count, 0);
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "test assertions on known-length vec"
)]
fn ec03_round_trip_with_records() {
    let mut frame = Ec03Frame::new(42, 7);
    frame.add_record(vec![1, 2, 3]);
    frame.add_record(vec![4, 5, 6, 7]);
    frame.add_record(vec![8]);

    let encoded = encode_ec03(&frame);
    let decoded = decode_ec03(&encoded).unwrap();

    assert_eq!(decoded.page_sequence, 42);
    assert_eq!(decoded.commit_generation, 7);
    assert_eq!(decoded.records.len(), 3);
    assert_eq!(decoded.records[0], vec![1, 2, 3]);
    assert_eq!(decoded.records[1], vec![4, 5, 6, 7]);
    assert_eq!(decoded.records[2], vec![8]);
    assert_eq!(decoded.record_count, 3);
}

#[test]
fn ec03_detect_format() {
    let mut frame = Ec03Frame::new(0, 0);
    frame.add_record(vec![1]);
    let encoded = encode_ec03(&frame);

    assert_eq!(detect_format(&encoded), Some(FrameFormat::Ec03));
    assert_eq!(detect_format(b"EC02"), Some(FrameFormat::Ec02));
    assert_eq!(detect_format(b"XXXX"), None);
    assert_eq!(detect_format(b""), None);
}

#[test]
fn ec03_power_loss_partial_frame() {
    let mut frame = Ec03Frame::new(0, 0);
    frame.add_record(vec![1, 2, 3]);
    let mut encoded = encode_ec03(&frame);

    // Truncate in the middle of the payload.
    encoded.truncate(encoded.len() - 6);

    assert!(decode_ec03(&encoded).is_none());
}
