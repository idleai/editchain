use editchain_core::*;
use editchain_node::export::op_to_json;

#[test]
fn export_message_op() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: parents::ParentSet::None,
        actor: ActorId(0),
        clock: clock::Clock::UnixMs(1700000000000),
        scope: scope::ScopeRef::None,
        tags: tags::Tags::MESSAGE,
        kind: op::OpKind::Message(op::MessageOp {
            content: payload::Payload::Inline(b"hello".to_vec()),
            content_type: payload::Payload::Empty,
        }),
    };

    let json = op_to_json(&op).unwrap();
    // Vec<u8> serializes as a JSON array of integers
    assert!(json.contains("104,101,108,108,111"));
    assert!(json.contains("Message"));
}