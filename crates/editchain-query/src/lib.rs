//! Editchain query plane — request/response types, rank fusion, and graph algorithms.

// serde is available for downstream consumers that derive Serialize/Deserialize on query types.
use serde as _;

/// Reciprocal Rank Fusion — combines multiple ranked result lists into one.
pub mod fusion;
/// DAG-aware retrieval — causal cone expansion, causal corridor, branch diversity MMR.
pub mod graph;
/// Hybrid search orchestrator — combines BM25 and vector results via RRF.
pub mod hybrid;
/// Search request/response types and filters.
pub mod search;
/// Extractive summarization — deterministic, cited context packs.
pub mod summarize;

pub use fusion::*;
pub use graph::*;
pub use hybrid::*;
pub use search::*;
pub use summarize::*;
