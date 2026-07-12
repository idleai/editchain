//! Extractive summarization — deterministic, cited context packs.

use crate::search::{ScoredChunk, SummarizeRequest, SummarizeStrategy};

/// A single evidence snippet in a summary.
#[derive(Debug, Clone)]
pub struct EvidenceSnippet {
    /// The operation ID this snippet comes from.
    pub op_id: String,
    /// The text content.
    pub text: String,
    /// Relevance score.
    pub score: f64,
    /// Byte range within the original operation.
    pub byte_start: u32,
    pub byte_end: u32,
}

/// An extractive summary result.
#[derive(Debug, Clone)]
pub struct SummaryResult {
    /// The query that produced this summary.
    pub query: String,
    /// Strategy used.
    pub strategy: SummarizeStrategy,
    /// Evidence snippets in presentation order.
    pub snippets: Vec<EvidenceSnippet>,
    /// Total tokens in the summary.
    pub total_tokens: usize,
}

/// Build an extractive summary from search results.
///
/// Selects top distinct evidence snippets up to the token budget.
pub fn build_extractive_summary(
    request: &SummarizeRequest,
    results: Vec<ScoredChunk>,
) -> SummaryResult {
    let mut snippets: Vec<EvidenceSnippet> = results
        .into_iter()
        .map(|r| EvidenceSnippet {
            op_id: r.op_id.to_string(),
            text: r.text,
            score: r.score,
            byte_start: 0,
            byte_end: 0,
        })
        .collect();

    // Sort by score descending.
    snippets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Select snippets up to token budget (rough estimate: 4 bytes per token).
    let budget_bytes = request.budget_tokens.saturating_mul(4);
    let mut selected = Vec::new();
    let mut total_bytes = 0usize;

    for snippet in snippets {
        let snippet_bytes = snippet.text.len();
        if total_bytes + snippet_bytes <= budget_bytes {
            selected.push(snippet);
            total_bytes += snippet_bytes;
        } else {
            break;
        }
    }

    SummaryResult {
        query: request.query.clone(),
        strategy: request.strategy,
        snippets: selected,
        total_tokens: total_bytes / 4,
    }
}

/// Build a timeline summary — causal/time-ordered excerpts.
pub fn build_timeline_summary(
    request: &SummarizeRequest,
    results: Vec<ScoredChunk>,
) -> SummaryResult {
    let mut snippets: Vec<EvidenceSnippet> = results
        .into_iter()
        .map(|r| EvidenceSnippet {
            op_id: r.op_id.to_string(),
            text: r.text,
            score: r.score,
            byte_start: 0,
            byte_end: 0,
        })
        .collect();

    // Sort by OpId (which follows causal order within a node).
    snippets.sort_by(|a, b| a.op_id.cmp(&b.op_id));

    let budget_bytes = request.budget_tokens.saturating_mul(4);
    let mut selected = Vec::new();
    let mut total_bytes = 0usize;

    for snippet in snippets {
        let snippet_bytes = snippet.text.len();
        if total_bytes + snippet_bytes <= budget_bytes {
            selected.push(snippet);
            total_bytes += snippet_bytes;
        } else {
            break;
        }
    }

    SummaryResult {
        query: request.query.clone(),
        strategy: request.strategy,
        snippets: selected,
        total_tokens: total_bytes / 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{ChunkId, ChunkMetadata};
    use editchain_core::{ActorId, NodeId, OpId};

    fn make_chunk(seq: u64, text: &str, score: f64) -> ScoredChunk {
        let op_id = OpId::new(NodeId(1), 0, seq);
        ScoredChunk {
            chunk_id: ChunkId { op_id, chunk_ordinal: 0 },
            op_id,
            score,
            text: text.to_string(),
            metadata: ChunkMetadata {
                op_id,
                chunk_id: ChunkId { op_id, chunk_ordinal: 0 },
                session_id: None,
                actor_id: ActorId(0),
                kind_tags: 0,
                timestamp_ms: 0,
                generation: 1,
            },
        }
    }

    #[test]
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
        // Highest score first.
        assert!(summary.snippets[0].text.contains("high"));
    }

    #[test]
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
}