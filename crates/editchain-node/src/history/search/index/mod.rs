//! Private BM25 indexing and deterministic text chunking for history search.

mod chunker;
mod lexical;

#[cfg(test)]
pub(crate) use chunker::chunk_text;
pub(crate) use chunker::ChunkOptions;
#[cfg(test)]
pub(crate) use lexical::MAX_CANDIDATES;
pub(crate) use lexical::{
    DocumentId, LexicalHit, LexicalIndex, LexicalIndexBuilder, SearchDocument,
};

#[cfg(test)]
mod tests;
