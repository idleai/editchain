use crate::search::ScoredChunk;

/// Reciprocal Rank Fusion — combines multiple ranked result lists into one.
///
/// RRF(d) = Σ 1 / (k + `rank_r(d)`)
///
/// Where `rank_r(d)` is the position (1-based) of document `d` in result list `r`,
/// and `k` is a constant (default 60).
///
/// This avoids the problem of combining incompatible raw scores (BM25 vs cosine).
#[expect(
    clippy::type_complexity,
    clippy::cast_precision_loss,
    clippy::as_conversions,
    clippy::arithmetic_side_effects,
    reason = "HashMap of (node,boot,seq) -> (score, chunk) is a natural local structure; rank is small (<200), f64 precision sufficient; rank+1 cast and RRF accumulation are intentional"
)]
#[must_use]
pub fn rrf_fuse(lists: &[Vec<ScoredChunk>], k: f64, top_k: usize) -> Vec<ScoredChunk> {
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
