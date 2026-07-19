use std::collections::HashMap;

use editchain_core::{Frontier, NodeId, OpId};

use crate::search::ScoredChunk;

/// A frontier map — `(node, boot) -> max_seq` for fast visibility checks.
///
/// This enables "what was known at this checkpoint?" queries without
/// traversing the full DAG.
#[derive(Debug, Clone, Default)]
pub struct FrontierMap {
    frontiers: HashMap<(u64, u32), u64>,
}

impl FrontierMap {
    pub fn new() -> Self {
        Self {
            frontiers: HashMap::new(),
        }
    }

    /// Build a frontier map from a slice of frontiers.
    pub fn from_frontiers(frontiers: &[Frontier]) -> Self {
        let mut map = Self::new();
        for f in frontiers {
            map.insert(f.node, f.boot, f.max_seq);
        }
        map
    }

    /// Insert a frontier entry.
    pub fn insert(&mut self, node: NodeId, boot: u32, max_seq: u64) {
        self.frontiers.insert((node.0, boot), max_seq);
    }

    /// Check if an operation is visible at this frontier.
    pub fn is_visible(&self, op_id: &OpId) -> bool {
        self.frontiers
            .get(&(op_id.node.0, op_id.boot))
            .is_some_and(|&max_seq| op_id.seq <= max_seq)
    }

    /// Get the max sequence for a given node and boot.
    pub fn max_seq(&self, node: NodeId, boot: u32) -> Option<u64> {
        self.frontiers.get(&(node.0, boot)).copied()
    }

    /// Number of entries in the frontier map.
    pub fn len(&self) -> usize {
        self.frontiers.len()
    }

    /// Returns true if the frontier map is empty.
    pub fn is_empty(&self) -> bool {
        self.frontiers.is_empty()
    }
}

/// A causal cone — the set of ancestor and descendant operations around a seed.
///
/// This is used for context expansion around search hits.
#[derive(Debug, Clone)]
pub struct CausalCone {
    /// The seed operation at the center of the cone.
    pub seed: OpId,
    /// Ancestor operations (prompts, tool starts, command invocations).
    pub ancestors: Vec<OpId>,
    /// Descendant operations (results, file effects, conclusions).
    pub descendants: Vec<OpId>,
}

impl CausalCone {
    pub fn new(seed: OpId) -> Self {
        Self {
            seed,
            ancestors: Vec::new(),
            descendants: Vec::new(),
        }
    }

    /// Total number of operations in the cone (including seed).
    pub fn total_ops(&self) -> usize {
        1 + self.ancestors.len() + self.descendants.len()
    }
}

/// Branch diversity penalty for MMR (Maximal Marginal Relevance).
///
/// Used to avoid returning multiple nearly-identical chunks from one branch.
#[derive(Debug, Clone)]
pub struct DiversityConfig {
    /// Weight for text similarity penalty.
    pub lambda_text: f64,
    /// Weight for graph overlap penalty.
    pub lambda_graph: f64,
}

impl Default for DiversityConfig {
    fn default() -> Self {
        Self {
            lambda_text: 0.3,
            lambda_graph: 0.2,
        }
    }
}

// ---------------------------------------------------------------------------
// Causal corridor
// ---------------------------------------------------------------------------

/// A causal corridor — the shortest parent path from a hit to a selected tip.
///
/// Used by `retrieve --why` to show how evidence connects to later work.
#[derive(Debug, Clone)]
pub struct CausalCorridor {
    /// The source operation (evidence hit).
    pub source: OpId,
    /// The target operation (tip/frontier).
    pub target: OpId,
    /// Ordered OpIds along the shortest parent path (source → ... → target).
    pub path: Vec<OpId>,
}

impl CausalCorridor {
    pub fn new(source: OpId, target: OpId) -> Self {
        Self {
            source,
            target,
            path: Vec::new(),
        }
    }

    /// Number of hops in the corridor.
    pub fn len(&self) -> usize {
        self.path.len()
    }

    /// Returns true if the corridor is empty (no path found).
    pub fn is_empty(&self) -> bool {
        self.path.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Occurrence collapse
// ---------------------------------------------------------------------------

/// A logical occurrence — groups physical chunks that share content.
#[derive(Debug, Clone)]
pub struct LogicalOccurrence {
    /// The canonical chunk for this content.
    pub canonical: ScoredChunk,
    /// Number of physical occurrences across branches/sessions.
    pub occurrence_count: usize,
    /// All source OpIds where this content appears.
    pub source_op_ids: Vec<OpId>,
}

/// Collapse duplicate chunks by content similarity and group by logical identity.
///
/// Returns deduplicated results with occurrence counts.
pub fn collapse_occurrences(
    results: Vec<ScoredChunk>,
    text_similarity_threshold: f64,
) -> Vec<LogicalOccurrence> {
    let mut groups: Vec<LogicalOccurrence> = Vec::new();

    for chunk in results {
        let mut found = false;
        for group in &mut groups {
            // Simple text prefix match as a cheap similarity heuristic.
            let min_len = group.canonical.text.len().min(chunk.text.len());
            if min_len > 0 {
                let matches = group.canonical.text[..min_len]
                    .chars()
                    .zip(chunk.text[..min_len].chars())
                    .filter(|(a, b)| a == b)
                    .count();
                let similarity = matches as f64 / min_len as f64;
                if similarity >= text_similarity_threshold {
                    group.occurrence_count += 1;
                    group.source_op_ids.push(chunk.op_id);
                    found = true;
                    break;
                }
            }
        }
        if !found {
            groups.push(LogicalOccurrence {
                canonical: chunk,
                occurrence_count: 1,
                source_op_ids: Vec::new(),
            });
        }
    }

    groups
}

// ---------------------------------------------------------------------------
// Branch diversity MMR
// ---------------------------------------------------------------------------

/// Apply Maximal Marginal Relevance with graph-aware diversity penalty.
///
/// utility = relevance - λ_text * max_similarity - λ_graph * graph_overlap
pub fn mmr_diverse_rerank(
    results: &[ScoredChunk],
    config: &DiversityConfig,
    top_k: usize,
) -> Vec<ScoredChunk> {
    if results.is_empty() {
        return Vec::new();
    }

    let mut selected: Vec<ScoredChunk> = Vec::new();
    let mut candidate_scores: Vec<(usize, f64)> = results
        .iter()
        .enumerate()
        .map(|(i, c)| (i, c.score))
        .collect();

    while selected.len() < top_k && !candidate_scores.is_empty() {
        // Find the candidate with highest MMR score.
        let mut best_idx = 0;
        let mut best_score = f64::NEG_INFINITY;

        for (pos, &(cand_idx, _)) in candidate_scores.iter().enumerate() {
            let relevance = results[cand_idx].score;

            // Compute max similarity to already-selected items.
            let max_sim = selected
                .iter()
                .map(|s| text_similarity(&results[cand_idx].text, &s.text))
                .fold(0.0f64, f64::max);

            // Compute graph overlap penalty (same node = high overlap).
            let graph_overlap = selected
                .iter()
                .filter(|s| s.op_id.node == results[cand_idx].op_id.node)
                .count() as f64
                / selected.len().max(1) as f64;

            let mmr = relevance - config.lambda_text * max_sim - config.lambda_graph * graph_overlap;

            if mmr > best_score {
                best_score = mmr;
                best_idx = pos;
            }
        }

        let (sel_idx, _) = candidate_scores.remove(best_idx);
        selected.push(results[sel_idx].clone());
    }

    selected
}

/// Simple text similarity based on character overlap.
fn text_similarity(a: &str, b: &str) -> f64 {
    let min_len = a.len().min(b.len());
    if min_len == 0 {
        return 0.0;
    }
    let matches = a
        .chars()
        .zip(b.chars())
        .filter(|(x, y)| x == y)
        .count();
    matches as f64 / min_len as f64
}

