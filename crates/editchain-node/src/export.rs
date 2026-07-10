use editchain_core::Op;
use serde_json;

/// Export a slice of operations as pretty-printed JSON lines.
pub fn export_json(ops: &[Op]) -> serde_json::Result<String> {
    let mut lines = Vec::new();
    for op in ops {
        let json = serde_json::to_string_pretty(op)?;
        lines.push(json);
    }
    Ok(lines.join("\n"))
}

/// Export a single operation as a compact JSON string.
pub fn op_to_json(op: &Op) -> serde_json::Result<String> {
    serde_json::to_string(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editchain_core::*;

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
}