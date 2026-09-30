//! Schema three shares EC03 framing while keeping earlier envelopes frozen.

use blake3 as _;
use crc as _;
use editchain_index_pages as _;
use postcard as _;
use proptest as _;
use serde as _;
use serde_json as _;
use tempfile as _;

use editchain_core::activity::{ItemId, Kind, Note, NoteKind, Operation};
use editchain_core::{OpId, Payload};
use editchain_store::format::{decode_op, encode_op, EC03_FORMAT_VERSION};

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Test assertions report contract failures; Result propagates setup errors"
)]
fn schema_three_roundtrip_and_invalid_wrappers() -> Result<(), Box<dyn std::error::Error>> {
    let record = Operation::new(
        OpId::from_bytes([3; 32]),
        ItemId::derive("note", b"one"),
        ItemId::derive("recorder", b"one"),
        Kind::Note(Note {
            category: NoteKind::Comment,
            targets: Vec::new(),
            items: Vec::new(),
            version: 1,
            content: Payload::Inline(b"hello".to_vec()),
            code: Payload::Empty,
        }),
    );
    let mut op = record.into_op()?;
    let encoded = encode_op(&op)?;
    assert_eq!(
        encoded.get(14),
        Some(&3),
        "operation schema occupies its reserved byte"
    );
    assert_eq!(EC03_FORMAT_VERSION, 2, "wire framing does not change");
    assert_eq!(decode_op(&encoded)?, op, "schema-three bytes round trip");
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(
        decode_op(&trailing).is_err(),
        "unknown suffixes are rejected"
    );
    let mut wrong_schema = encoded;
    *wrong_schema.get_mut(14).ok_or("missing schema byte")? = 1;
    assert!(
        decode_op(&wrong_schema).is_err(),
        "schema-three bytes cannot pretend to be schema one"
    );
    op.id = OpId::from_bytes([4; 32]);
    assert!(
        encode_op(&op).is_err(),
        "outer and inner identities must agree"
    );
    Ok(())
}
