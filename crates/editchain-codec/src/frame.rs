use editchain_core::Op;
use postcard;

/// Encode an operation into a binary frame using postcard.
pub fn encode_op(op: &Op) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_stdvec(op)
}

/// Decode an operation from a binary frame.
pub fn decode_op(bytes: &[u8]) -> Result<Op, postcard::Error> {
    postcard::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editchain_core::*;

    #[test]
    fn round_trip_message_op() {
        let op = Op {
            id: OpId::new(NodeId(1), 0, 42),
            parents: parents::ParentSet::None,
            actor: ActorId(1),
            clock: clock::Clock::UnixMs(1700000000000),
            scope: scope::ScopeRef::None,
            tags: tags::Tags::MESSAGE,
            kind: op::OpKind::Message(op::MessageOp {
                content: payload::Payload::Inline(b"hello world".to_vec()),
                content_type: payload::Payload::Empty,
            }),
        };

        let encoded = encode_op(&op).unwrap();
        let decoded: Op = decode_op(&encoded).unwrap();

        assert_eq!(op.id, decoded.id);
        assert_eq!(op.actor, decoded.actor);
        assert_eq!(op.clock, decoded.clock);
        assert_eq!(op.tags, decoded.tags);

        // Verify the message content round-tripped
        match (&op.kind, &decoded.kind) {
            (op::OpKind::Message(a), op::OpKind::Message(b)) => {
                assert_eq!(a.content, b.content);
            }
            _ => panic!("kind mismatch"),
        }
    }

    #[test]
    fn round_trip_file_op() {
        let op = Op {
            id: OpId::new(NodeId(2), 1, 7),
            parents: parents::ParentSet::None,
            actor: ActorId(0),
            clock: clock::Clock::Lamport(99),
            scope: scope::ScopeRef::File(ids::PathId(42)),
            tags: tags::Tags::FILE,
            kind: op::OpKind::File(op::FileOp {
                path: ids::PathId(42),
                stage: op::FileStage::Applied,
                base: None,
                after: Some(payload::ContentId::Hash128([0xAB; 16])),
                edit: op::FileEdit::None,
            }),
        };

        let encoded = encode_op(&op).unwrap();
        let decoded: Op = decode_op(&encoded).unwrap();

        assert_eq!(op.id, decoded.id);
        match (&op.kind, &decoded.kind) {
            (op::OpKind::File(a), op::OpKind::File(b)) => {
                assert_eq!(a.path, b.path);
                assert_eq!(a.stage, b.stage);
                assert_eq!(a.after, b.after);
            }
            _ => panic!("kind mismatch"),
        }
    }
}