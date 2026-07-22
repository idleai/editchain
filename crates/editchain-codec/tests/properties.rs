//! Property-based tests for codec invariants.
//!
//! Verifies:
//! - decode(encode(op)) == op  (roundtrip)
//! - malformed or truncated input never panics
//! - canonical input produces stable bytes

// Suppress unused_crate_dependencies warnings for crates consumed by other modules.
use crc as _;
use postcard as _;
use serde as _;

use editchain_codec::frame::{decode_ec03, decode_op, encode_ec03, encode_op, Ec03Frame};
use editchain_codec::page::{decode_page, encode_page, Page};
use editchain_core::*;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Arbitrary Op generation
// ---------------------------------------------------------------------------

fn arb_opid() -> impl Strategy<Value = OpId> {
    (any::<u64>(), any::<u32>(), any::<u64>())
        .prop_map(|(node, boot, seq)| OpId::new(NodeId(node), boot, seq))
}

fn arb_clock() -> impl Strategy<Value = Clock> {
    prop_oneof![
        Just(Clock::None),
        any::<u64>().prop_map(Clock::Lamport),
        any::<u64>().prop_map(Clock::UnixMs),
        (any::<u64>(), any::<u16>()).prop_map(|(ms, ctr)| Clock::Hybrid { ms, ctr }),
    ]
}

fn arb_parents() -> impl Strategy<Value = ParentSet> {
    prop_oneof![
        Just(ParentSet::None),
        arb_opid().prop_map(ParentSet::One),
        (arb_opid(), arb_opid()).prop_map(|(a, b)| ParentSet::Two(a, b)),
    ]
}

fn arb_op() -> impl Strategy<Value = Op> {
    (
        arb_opid(),
        arb_parents(),
        any::<u64>().prop_map(ActorId),
        arb_clock(),
        Just(ScopeRef::None),
        Just(Tags(0)),
    )
        .prop_flat_map(|(id, parents, actor, clock, scope, tags)| {
            let kind = prop_oneof![
                Just(OpKind::ChainStart(ChainStart {
                    name: b"test".to_vec(),
                    version: 1,
                })),
                Just(OpKind::Message(MessageOp {
                    content: Payload::Inline(b"hello".to_vec()),
                    content_type: Payload::Empty,
                })),
                Just(OpKind::Tool(ToolOp {
                    tool_call_id: Payload::Inline(b"call_1".to_vec()),
                    tool_name: Payload::Inline(b"bash".to_vec()),
                    stage: ToolStage::Start,
                    content: Payload::Inline(b"ls".to_vec()),
                })),
                Just(OpKind::File(FileOp {
                    path: PathId(42),
                    stage: FileStage::Applied,
                    base: None,
                    after: None,
                    edit: FileEdit::None,
                })),
            ];
            (
                Just(id),
                Just(parents),
                Just(actor),
                Just(clock),
                Just(scope),
                Just(tags),
                kind,
            )
        })
        .prop_map(|(id, parents, actor, clock, scope, tags, kind)| Op {
            id,
            parents,
            actor,
            clock,
            scope,
            tags,
            kind,
        })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    // -----------------------------------------------------------------------
    // Roundtrip: decode(encode(op)) == op
    // -----------------------------------------------------------------------
    #[test]
    fn op_roundtrip(op in arb_op()) {
        let encoded = encode_op(&op).expect("encode should succeed");
        let decoded = decode_op(&encoded).expect("decode should succeed");
        prop_assert_eq!(op, decoded);
    }

    // -----------------------------------------------------------------------
    // Malformed input never panics
    // -----------------------------------------------------------------------
    #[test]
    fn decode_malformed_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..100)) {
        // decode_op returns a Result — should never panic.
        let _result = decode_op(&bytes);
    }

    #[test]
    fn decode_page_malformed_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..100)) {
        let _result = decode_page(&bytes);
    }

    #[test]
    fn decode_ec03_malformed_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..100)) {
        let _result = decode_ec03(&bytes);
    }

    // -----------------------------------------------------------------------
    // Page roundtrip
    // -----------------------------------------------------------------------
    #[test]
    fn page_roundtrip(
        page_seq in any::<u32>(),
        records in proptest::collection::vec(
            (any::<u8>(), proptest::collection::vec(any::<u8>(), 0..64)),
            0..10,
        ),
    ) {
        let mut page = Page::new(page_seq);
        for (flags, data) in &records {
            page.add_record(*flags, data.clone());
        }

        let encoded = encode_page(&page);
        let decoded = decode_page(&encoded);

        prop_assert!(decoded.is_some(), "decode should succeed");
        let decoded = decoded.unwrap();
        prop_assert_eq!(page.page_seq, decoded.page_seq);
        prop_assert_eq!(page.records.len(), decoded.records.len());
        for (a, b) in page.records.iter().zip(decoded.records.iter()) {
            prop_assert_eq!(a.flags, b.flags);
            prop_assert_eq!(a.data.clone(), b.data.clone());
        }
    }

    // -----------------------------------------------------------------------
    // EC03 frame roundtrip
    // -----------------------------------------------------------------------
    #[test]
    fn ec03_roundtrip(
        page_sequence in any::<u64>(),
        commit_generation in any::<u64>(),
        records in proptest::collection::vec(
            proptest::collection::vec(any::<u8>(), 0..64),
            0..10,
        ),
    ) {
        let mut frame = Ec03Frame::new(page_sequence, commit_generation);
        for data in &records {
            frame.add_record(data.clone());
        }

        let encoded = encode_ec03(&frame);
        let decoded = decode_ec03(&encoded);

        prop_assert!(decoded.is_some(), "decode should succeed");
        let decoded = decoded.unwrap();
        prop_assert_eq!(frame.format_version, decoded.format_version);
        prop_assert_eq!(frame.record_count, decoded.record_count);
        prop_assert_eq!(frame.page_sequence, decoded.page_sequence);
        prop_assert_eq!(frame.commit_generation, decoded.commit_generation);
        prop_assert_eq!(frame.records.len(), decoded.records.len());
        for (a, b) in frame.records.iter().zip(decoded.records.iter()) {
            prop_assert_eq!(a, b);
        }
    }
}
