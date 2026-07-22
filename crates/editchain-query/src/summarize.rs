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
    /// End byte offset within the original operation.
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
#[expect(
    clippy::arithmetic_side_effects,
    reason = "total_bytes accumulation is bounded by budget_bytes; no overflow in practice"
)]
#[must_use]
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
    snippets.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

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
#[expect(
    clippy::arithmetic_side_effects,
    reason = "total_bytes accumulation is bounded by budget_bytes; no overflow in practice"
)]
#[must_use]
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
