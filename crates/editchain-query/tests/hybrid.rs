use editchain_core::{ActorId, NodeId, OpId};
use editchain_query::hybrid::{HybridSearch, LexicalSearch, VectorSearch};
use editchain_query::search::{ChunkId, ChunkMetadata, ScoredChunk, SearchFilters, SearchMode, SearchRequest};

struct MockLexical;
impl LexicalSearch for MockLexical {
    fn search(&self, query: &str, _filters: &SearchFilters, _top_k: usize) -> Vec<ScoredChunk> {
        let op_id = OpId::new(NodeId(1), 0, 1);
        vec![ScoredChunk {
            chunk_id: ChunkId { op_id, chunk_ordinal: 0 },
            op_id,
            score: 10.0,
            text: format!("lexical match for {}", query),
            metadata: ChunkMetadata {
                op_id,
                chunk_id: ChunkId { op_id, chunk_ordinal: 0 },
                session_id: None,
                actor_id: ActorId(0),
                kind_tags: 0,
                timestamp_ms: 0,
                generation: 1,
            },
        }]
    }
}

struct MockVector;
impl VectorSearch for MockVector {
    fn search(&self, query: &str, _filters: &SearchFilters, _top_k: usize) -> Vec<ScoredChunk> {
        let op_id = OpId::new(NodeId(1), 0, 2);
        vec![ScoredChunk {
            chunk_id: ChunkId { op_id, chunk_ordinal: 0 },
            op_id,
            score: 8.0,
            text: format!("vector match for {}", query),
            metadata: ChunkMetadata {
                op_id,
                chunk_id: ChunkId { op_id, chunk_ordinal: 0 },
                session_id: None,
                actor_id: ActorId(0),
                kind_tags: 0,
                timestamp_ms: 0,
                generation: 1,
            },
        }]
    }
}

#[test]
fn hybrid_fuses_results() {
    let engine = HybridSearch::new(Box::new(MockLexical), Box::new(MockVector));
    let request = SearchRequest {
        query: "test".to_string(),
        mode: SearchMode::Hybrid,
        top_k: 10,
        ..SearchRequest::default()
    };

    let result = engine.search(&request);
    assert_eq!(result.results.len(), 2);
}

#[test]
fn lexical_mode_only() {
    let engine = HybridSearch::new(Box::new(MockLexical), Box::new(MockVector));
    let request = SearchRequest {
        query: "test".to_string(),
        mode: SearchMode::Lexical,
        top_k: 10,
        ..SearchRequest::default()
    };

    let result = engine.search(&request);
    assert_eq!(result.results.len(), 1);
    assert!(result.results[0].text.contains("lexical"));
}