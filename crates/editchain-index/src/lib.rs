//! Editchain lexical index for VS Code history search.

/// Text chunking — deterministic overlapping segments from operation text.
pub mod chunker;
/// Tantivy-based BM25 lexical index.
pub mod lexical;

pub use chunker::*;
pub use lexical::*;
