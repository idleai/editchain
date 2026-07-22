use std::sync::Arc;

use crate::chunker::Generation;

/// A lexical (BM25) snapshot — an immutable view of the text index at a point in time.
#[derive(Debug, Clone)]
pub struct LexicalSnapshot {
    /// The generation this snapshot was built from.
    pub generation: Generation,
    /// Number of indexed documents.
    pub num_docs: usize,
}

/// A vector snapshot — an immutable view of the dense embedding index.
pub trait VectorSnapshot: Send + Sync + std::fmt::Debug {
    /// The generation this snapshot was built from.
    fn generation(&self) -> Generation;
    /// Number of indexed vectors.
    fn num_vectors(&self) -> usize;
    /// Vector dimensionality.
    fn dimensions(&self) -> u32;
}

/// A graph snapshot — adjacency and frontier data for DAG-aware retrieval.
#[derive(Debug, Clone)]
pub struct GraphSnapshot {
    /// The generation this snapshot was built from.
    pub generation: Generation,
}

/// A metadata snapshot — tag/session/actor filter bitmaps.
#[derive(Debug, Clone)]
pub struct MetadataSnapshot {
    /// The generation this snapshot was built from.
    pub generation: Generation,
}

/// A complete query snapshot — all projections at a consistent point.
///
/// Readers load one immutable snapshot for an entire query via `ArcSwap`.
#[derive(Debug, Clone)]
pub struct QuerySnapshot {
    /// Generation of the hydrated (text-extracted) data.
    pub hydrated_generation: Generation,
    /// Generation of the graph projection.
    pub graph_generation: Generation,
    /// Generation of the lexical (BM25) index.
    pub lexical_generation: Generation,
    /// Generation of the vector index.
    pub vector_generation: Generation,
    /// Lexical (BM25) snapshot.
    pub lexical: Arc<LexicalSnapshot>,
    /// Vector index snapshot.
    pub vectors: Arc<dyn VectorSnapshot>,
    /// Graph (DAG) snapshot.
    pub graph: Arc<GraphSnapshot>,
    /// Metadata snapshot (tag/session/actor filter bitmaps).
    pub metadata: Arc<MetadataSnapshot>,
}

impl QuerySnapshot {
    /// Create a new empty query snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            hydrated_generation: 0,
            graph_generation: 0,
            lexical_generation: 0,
            vector_generation: 0,
            lexical: Arc::new(LexicalSnapshot {
                generation: 0,
                num_docs: 0,
            }),
            vectors: Arc::new(EmptyVectorSnapshot),
            graph: Arc::new(GraphSnapshot { generation: 0 }),
            metadata: Arc::new(MetadataSnapshot { generation: 0 }),
        }
    }
}

impl Default for QuerySnapshot {
    fn default() -> Self {
        Self::new()
    }
}

/// An empty vector snapshot (used before any vectors are indexed).
#[derive(Debug, Clone)]
struct EmptyVectorSnapshot;

impl VectorSnapshot for EmptyVectorSnapshot {
    fn generation(&self) -> Generation {
        0
    }

    fn num_vectors(&self) -> usize {
        0
    }

    fn dimensions(&self) -> u32 {
        0
    }
}
