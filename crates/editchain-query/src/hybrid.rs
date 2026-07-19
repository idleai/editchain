//! Hybrid search orchestrator — combines BM25 and vector results via RRF.

use crate::fusion::rrf_fuse;
use crate::search::{ScoredChunk, SearchFilters, SearchMode, SearchRequest, SearchResult};

/// Default RRF constant.
pub const DEFAULT_RRF_K: f64 = 60.0;

/// Default number of candidates per index.
pub const DEFAULT_CANDIDATES: usize = 200;

/// Default final result count.
pub const DEFAULT_TOP_K: usize = 20;

/// A trait for lexical (BM25) search backends.
pub trait LexicalSearch: Send + Sync {
    fn search(&self, query: &str, filters: &SearchFilters, top_k: usize) -> Vec<ScoredChunk>;
}

/// A trait for vector search backends.
pub trait VectorSearch: Send + Sync {
    fn search(&self, query: &str, filters: &SearchFilters, top_k: usize) -> Vec<ScoredChunk>;
}

/// Hybrid search orchestrator.
///
/// Runs BM25 and vector searches in parallel (conceptually), then fuses
/// results via Reciprocal Rank Fusion.
pub struct HybridSearch {
    lexical: Box<dyn LexicalSearch>,
    vector: Box<dyn VectorSearch>,
}

impl HybridSearch {
    pub fn new(lexical: Box<dyn LexicalSearch>, vector: Box<dyn VectorSearch>) -> Self {
        Self { lexical, vector }
    }

    /// Execute a search request.
    pub fn search(&self, request: &SearchRequest) -> SearchResult {
        match request.mode {
            SearchMode::Lexical => {
                let results = self.lexical.search(&request.query, &request.filters, request.top_k);
                SearchResult {
                    results,
                    watermarks: crate::search::ProjectionWatermarks {
                        log: 0,
                        hydrated: 0,
                        graph: 0,
                        lexical: 0,
                        vector: 0,
                    },
                }
            }
            SearchMode::Vector => {
                let results = self.vector.search(&request.query, &request.filters, request.top_k);
                SearchResult {
                    results,
                    watermarks: crate::search::ProjectionWatermarks {
                        log: 0,
                        hydrated: 0,
                        graph: 0,
                        lexical: 0,
                        vector: 0,
                    },
                }
            }
            SearchMode::Hybrid => {
                let lexical_results = self.lexical.search(
                    &request.query,
                    &request.filters,
                    DEFAULT_CANDIDATES,
                );
                let vector_results = self.vector.search(
                    &request.query,
                    &request.filters,
                    DEFAULT_CANDIDATES,
                );

                let fused = rrf_fuse(&[lexical_results, vector_results], DEFAULT_RRF_K, request.top_k);

                SearchResult {
                    results: fused,
                    watermarks: crate::search::ProjectionWatermarks {
                        log: 0,
                        hydrated: 0,
                        graph: 0,
                        lexical: 0,
                        vector: 0,
                    },
                }
            }
        }
    }
}

