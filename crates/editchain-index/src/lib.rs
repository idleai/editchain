//! Editchain index — text projection, chunking, and query snapshots.

/// Text chunking — deterministic overlapping segments from operation text.
pub mod chunker;
/// Tantivy-based BM25 lexical index.
pub mod lexical;
/// Immutable query snapshots for consistent multi-index reads.
pub mod snapshot;
/// Flat f16 vector index with RoaringBitmap filters.
pub mod vector;

pub use chunker::*;
pub use lexical::*;
pub use snapshot::*;
pub use vector::*;
