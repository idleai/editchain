use editchain_core::{ActorId, NodeId, OpId};
use editchain_query::fusion::rrf_fuse;
use editchain_query::search::{ChunkId, ChunkMetadata, ScoredChunk};

fn make_chunk(node: u64, seq: u64, score: f64) -> ScoredChunk {
    let op_id = OpId::new(NodeId(node), 0, seq);
    ScoredChunk {
        chunk_id: ChunkId { op_id, chunk_ordinal: 0 },
        op_id,
        score,
        text: format!("chunk {}/{}", node, seq),
        metadata: ChunkMetadata {
            op_id,
            chunk_id: ChunkId { op_id, chunk_ordinal: 0 },
            session_id: None,
            actor_id: ActorId(0),
            kind_tags: 0,
            timestamp_ms: 0,
            generation: 0,
        },
    }
}

#[test]
fn rrf_empty_lists() {
    let result = rrf_fuse(&[], 60.0, 10);
    assert!(result.is_empty());
}

#[test]
fn rrf_single_list() {
    let list = vec![make_chunk(1, 1, 10.0), make_chunk(1, 2, 5.0)];
    let result = rrf_fuse(&[list], 60.0, 10);
    assert_eq!(result.len(), 2);
    assert!(result[0].score > result[1].score);
}

#[test]
fn rrf_two_lists() {
    let list_a = vec![make_chunk(1, 1, 10.0), make_chunk(1, 2, 5.0)];
    let list_b = vec![make_chunk(1, 2, 8.0), make_chunk(1, 3, 3.0)];

    let result = rrf_fuse(&[list_a, list_b], 60.0, 10);

    assert_eq!(result.len(), 3);
    assert_eq!(result[0].op_id.seq, 2);
}