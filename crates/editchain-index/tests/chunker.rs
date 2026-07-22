#![doc = "Tests for the chunker module."]

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_embed as _;
use editchain_query as _;
use half as _;
use roaring as _;
use tantivy as _;

use editchain_core::*;
use editchain_index::chunker::{chunk_text, extract_op_text};

#[test]
fn extract_message_text() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"hello world".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    let text = extract_op_text(&op, false, false);
    assert_eq!(text, Some("hello world".to_string()));
}

#[test]
fn extract_private_content_blocked() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::None,
        tags: Tags::PRIVATE | Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"secret".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    assert!(extract_op_text(&op, false, false).is_none());
    assert!(extract_op_text(&op, false, true).is_some());
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-valid indices; panic is acceptable in tests"
)]
fn chunk_short_text() {
    let text = "short";
    let op_id = OpId::new(NodeId(1), 0, 1);
    let chunks = chunk_text(text, op_id, 0, 768, 96);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].byte_start, 0);
    assert_eq!(chunks[0].byte_end, 5);
}
