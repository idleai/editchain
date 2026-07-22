//! Export tests.

use clap as _;
use dirs as _;
use editchain_codec as _;
use editchain_embed as _;
use editchain_import as _;
use editchain_index as _;
use editchain_query as _;
use serde as _;
use serde_json as _;
use tempfile as _;

use editchain_core::*;
use editchain_node::export::op_to_json;

#[test]
fn export_message_op() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(1_700_000_000_000),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"hello".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    let json = op_to_json(&op).unwrap();
    // Vec<u8> serializes as a JSON array of integers
    assert!(json.contains("104,101,108,108,111"));
    assert!(json.contains("Message"));
}
