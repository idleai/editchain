use editchain_core::*;
use editchain_index::lexical::LexicalIndex;
use editchain_query::search::SearchFilters;

#[test]
fn index_and_search_message() {
    let mut index = LexicalIndex::new().unwrap();

    let op = Op {
        id: OpId { node: NodeId(1), boot: 0, seq: 1 },
        parents: parents::ParentSet::None,
        actor: ActorId(1),
        clock: clock::Clock::UnixMs(1000),
        scope: scope::ScopeRef::None,
        tags: tags::Tags::MESSAGE | tags::Tags::AGENT,
        kind: op::OpKind::Message(op::MessageOp {
            content: payload::Payload::Inline(b"hello world test query".to_vec()),
            content_type: payload::Payload::Empty,
        }),
    };

    let chunks = index.index_op(&op, 1).unwrap();
    assert!(!chunks.is_empty());

    index.commit().unwrap();

    let filters = SearchFilters::default();
    let results = index.search_internal("hello", &filters, 10).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].op_id.seq, 1);
}