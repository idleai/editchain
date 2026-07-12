use crate::search::ScoredChunk;

/// Reciprocal Rank Fusion — combines multiple ranked result lists into one.
///
/// RRF(d) = Σ 1 / (k + rank_r(d))
///
/// Where `rank_r(d)` is the position (1-based) of document `d` in result list `r`,
/// and `k` is a constant (default 60).
///
/// This avoids the problem of combining incompatible raw scores (BM25 vs cosine).
pub fn rrf_fuse(
    lists: &[Vec<ScoredChunk>],
    k: f64,
    top_k: usize,
) -> Vec<ScoredChunk> {
    if lists.is_empty() {
        return Vec::new();
    }

    // Collect RRF scores keyed by (node, boot, seq) for uniqueness.
    let mut scores: std::collections::HashMap<(u64, u32, u64), (f64, &ScoredChunk)> =
        std::collections::HashMap::new();

    for list in lists {
        for (rank, chunk) in list.iter().enumerate() {
            let key = (chunk.op_id.node.0, chunk.op_id.boot, chunk.op_id.seq);
            let entry = scores.entry(key).or_insert((0.0, chunk));
            entry.0 += 1.0 / (k + (rank + 1) as f64);
        }
    }

    // Sort by RRF score descending, tie-break by OpId.
    let mut sorted: Vec<_> = scores.into_values().collect();
    sorted.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.op_id.cmp(&b.1.op_id))
    });

    // Take top_k and build result chunks.
    sorted
        .into_iter()
        .take(top_k)
        .map(|(score, chunk)| ScoredChunk {
            score,
            ..chunk.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{ChunkId, ChunkMetadata};
    use editchain_core::{ActorId, NodeId, OpId};

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
        let result = rrf_fuse(&[list.clone()], 60.0, 10);
        assert_eq!(result.len(), 2);
        // First item should have higher RRF score.
        assert!(result[0].score > result[1].score);
    }

    #[test]
    fn rrf_two_lists() {
        let list_a = vec![make_chunk(1, 1, 10.0), make_chunk(1, 2, 5.0)];
        let list_b = vec![make_chunk(1, 2, 8.0), make_chunk(1, 3, 3.0)];

        let result = rrf_fuse(&[list_a, list_b], 60.0, 10);

        // All 3 unique items should appear.
        assert_eq!(result.len(), 3);
        // Item seq=2 appears in both lists → highest RRF.
        assert_eq!(result[0].op_id.seq, 2);
    }

}