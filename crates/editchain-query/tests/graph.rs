#![expect(missing_docs, reason = "Test file")]

use editchain_core::{ActorId, Frontier, NodeId, OpId};
use editchain_query::graph::{
    collapse_occurrences, mmr_diverse_rerank, CausalCone, CausalCorridor, DiversityConfig,
    FrontierMap,
};
use editchain_query::search::{ChunkId, ChunkMetadata, ScoredChunk};
use serde as _;

#[test]
fn frontier_visibility() {
    let mut map = FrontierMap::new();
    map.insert(NodeId(1), 0, 100);

    assert!(map.is_visible(&OpId::new(NodeId(1), 0, 50)));
    assert!(map.is_visible(&OpId::new(NodeId(1), 0, 100)));
    assert!(!map.is_visible(&OpId::new(NodeId(1), 0, 101)));
    assert!(!map.is_visible(&OpId::new(NodeId(2), 0, 50)));
}

#[test]
fn frontier_from_slice() {
    let frontiers = vec![
        Frontier {
            node: NodeId(1),
            boot: 0,
            max_seq: 100,
        },
        Frontier {
            node: NodeId(2),
            boot: 0,
            max_seq: 200,
        },
    ];
    let map = FrontierMap::from_frontiers(&frontiers);
    assert_eq!(map.len(), 2);
    assert!(map.is_visible(&OpId::new(NodeId(1), 0, 50)));
    assert!(map.is_visible(&OpId::new(NodeId(2), 0, 150)));
}

#[test]
fn causal_cone_counts() {
    let seed = OpId::new(NodeId(1), 0, 10);
    let mut cone = CausalCone::new(seed);
    cone.ancestors.push(OpId::new(NodeId(1), 0, 8));
    cone.ancestors.push(OpId::new(NodeId(1), 0, 9));
    cone.descendants.push(OpId::new(NodeId(1), 0, 11));

    assert_eq!(cone.total_ops(), 4);
}

#[test]
fn causal_corridor_basic() {
    let source = OpId::new(NodeId(1), 0, 5);
    let target = OpId::new(NodeId(1), 0, 10);
    let mut corridor = CausalCorridor::new(source, target);
    corridor.path.push(OpId::new(NodeId(1), 0, 6));
    corridor.path.push(OpId::new(NodeId(1), 0, 7));
    corridor.path.push(OpId::new(NodeId(1), 0, 8));

    assert_eq!(corridor.len(), 3);
    assert!(!corridor.is_empty());
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-length vec"
)]
fn collapse_occurrences_deduplicates() {
    let op_id = OpId::new(NodeId(1), 0, 1);
    let chunk = ScoredChunk {
        chunk_id: ChunkId {
            op_id,
            chunk_ordinal: 0,
        },
        op_id,
        score: 1.0,
        text: "hello world".to_string(),
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
    };

    let results = vec![chunk.clone(), chunk];
    let collapsed = collapse_occurrences(results, 0.8);
    assert_eq!(collapsed.len(), 1);
    assert_eq!(collapsed[0].occurrence_count, 2);
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-length vec"
)]
fn mmr_diverse_rerank_selects_top() {
    let op_id1 = OpId::new(NodeId(1), 0, 1);
    let op_id2 = OpId::new(NodeId(2), 0, 2);

    let results = vec![
        ScoredChunk {
            chunk_id: ChunkId {
                op_id: op_id1,
                chunk_ordinal: 0,
            },
            op_id: op_id1,
            score: 10.0,
            text: "alpha".to_string(),
            metadata: ChunkMetadata {
                op_id: op_id1,
                chunk_id: ChunkId {
                    op_id: op_id1,
                    chunk_ordinal: 0,
                },
                session_id: None,
                actor_id: ActorId(0),
                kind_tags: 0,
                timestamp_ms: 0,
                generation: 1,
            },
        },
        ScoredChunk {
            chunk_id: ChunkId {
                op_id: op_id2,
                chunk_ordinal: 0,
            },
            op_id: op_id2,
            score: 5.0,
            text: "beta".to_string(),
            metadata: ChunkMetadata {
                op_id: op_id2,
                chunk_id: ChunkId {
                    op_id: op_id2,
                    chunk_ordinal: 0,
                },
                session_id: None,
                actor_id: ActorId(0),
                kind_tags: 0,
                timestamp_ms: 0,
                generation: 1,
            },
        },
    ];

    let reranked = mmr_diverse_rerank(&results, &DiversityConfig::default(), 2);
    assert_eq!(reranked.len(), 2);
    assert_eq!(reranked[0].op_id.seq, 1);
}
