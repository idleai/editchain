#![expect(missing_docs, reason = "Test file")]

use editchain_core::{ActorId, NodeId, OpId};
use editchain_query::search::{
    ChunkId, ChunkMetadata, ScoredChunk, SummarizeRequest, SummarizeStrategy,
};
use editchain_query::summarize::{build_extractive_summary, build_timeline_summary};
use serde as _;

fn make_chunk(seq: u64, text: &str, score: f64) -> ScoredChunk {
    let op_id = OpId::new(NodeId(1), 0, seq);
    ScoredChunk {
        chunk_id: ChunkId {
            op_id,
            chunk_ordinal: 0,
        },
        op_id,
        score,
        text: text.to_string(),
        metadata: ChunkMetadata {
            op_id,
            chunk_id: ChunkId {
                op_id,
                chunk_ordinal: 0,
            },
            session_id: None,
            actor_id: ActorId(0),
            kind_tags: 0,
            timestamp_ms: 0,
            generation: 1,
        },
    }
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-length vec"
)]
fn extractive_summary_selects_top() {
    let request = SummarizeRequest {
        query: "test".to_string(),
        budget_tokens: 100,
        strategy: SummarizeStrategy::Extractive,
    };

    let results = vec![
        make_chunk(1, "low relevance text", 1.0),
        make_chunk(2, "high relevance text", 10.0),
        make_chunk(3, "medium relevance text", 5.0),
    ];

    let summary = build_extractive_summary(&request, results);
    assert_eq!(summary.snippets.len(), 3);
    assert!(summary.snippets[0].text.contains("high"));
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-length vec"
)]
fn timeline_summary_orders_by_op_id() {
    let request = SummarizeRequest {
        query: "test".to_string(),
        budget_tokens: 100,
        strategy: SummarizeStrategy::Timeline,
    };

    let results = vec![
        make_chunk(3, "third", 5.0),
        make_chunk(1, "first", 10.0),
        make_chunk(2, "second", 1.0),
    ];

    let summary = build_timeline_summary(&request, results);
    assert_eq!(summary.snippets.len(), 3);
    assert!(summary.snippets[0].text.contains("first"));
    assert!(summary.snippets[1].text.contains("second"));
    assert!(summary.snippets[2].text.contains("third"));
}
