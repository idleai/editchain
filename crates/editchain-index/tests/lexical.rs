#![doc = "Tests for the lexical index module."]

use tantivy as _;

use editchain_core::*;
use editchain_index::lexical::LexicalIndex;

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-valid indices; panic is acceptable in tests"
)]
fn index_and_search_message() {
    let mut index = LexicalIndex::new().unwrap();

    let op = Op {
        id: OpId {
            node: NodeId(1),
            boot: 0,
            seq: 1,
        },
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE | Tags::AGENT,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"hello world test query".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    let chunks = index.index_op(&op, 1).unwrap();
    assert!(!chunks.is_empty());

    index.commit().unwrap();

    let results = index.search_internal("hello", 10).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].op_id.seq, 1);
}
