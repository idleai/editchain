use editchain_core::*;
use editchain_index::chunker::{chunk_text, extract_op_text};

#[test]
fn extract_message_text() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: parents::ParentSet::None,
        actor: ActorId(0),
        clock: clock::Clock::UnixMs(1000),
        scope: scope::ScopeRef::None,
        tags: tags::Tags::MESSAGE,
        kind: op::OpKind::Message(op::MessageOp {
            content: payload::Payload::Inline(b"hello world".to_vec()),
            content_type: payload::Payload::Empty,
        }),
    };

    let text = extract_op_text(&op, false, false);
    assert_eq!(text, Some("hello world".to_string()));
}

#[test]
fn extract_private_content_blocked() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: parents::ParentSet::None,
        actor: ActorId(0),
        clock: clock::Clock::UnixMs(1000),
        scope: scope::ScopeRef::None,
        tags: tags::Tags::PRIVATE | tags::Tags::MESSAGE,
        kind: op::OpKind::Message(op::MessageOp {
            content: payload::Payload::Inline(b"secret".to_vec()),
            content_type: payload::Payload::Empty,
        }),
    };

    assert!(extract_op_text(&op, false, false).is_none());
    assert!(extract_op_text(&op, false, true).is_some());
}

#[test]
fn chunk_short_text() {
    let text = "short";
    let op_id = OpId::new(NodeId(1), 0, 1);
    let chunks = chunk_text(text, op_id, 0, 768, 96);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].byte_start, 0);
    assert_eq!(chunks[0].byte_end, 5);
}