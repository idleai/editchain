//! Search the edit chain (BM25, vector, or hybrid).

use std::path::PathBuf;

use crate::segment::SegmentStore;
use editchain_codec::frame::decode_op;
use editchain_core::op::OpKind;
use editchain_embed::http::HttpEmbedder;
use editchain_embed::EmbeddingManifest;
use editchain_index::chunker::extract_op_text;
use editchain_index::vector::{f32_to_f16_vec, VectorIndex, VectorSearchWrapper};
use editchain_index::LexicalIndex;
use editchain_query::hybrid::{HybridSearch, LexicalSearch, VectorSearch};
use editchain_query::search::{SearchFilters, SearchMode, SearchRequest, TagFilter};

/// Text entry for embedding collection.
struct TextEntry {
    op_id: editchain_core::OpId,
    chunk_ordinal: u32,
    text: String,
    kind: String,
    session_id: Option<u64>,
}

/// Run the `search` command.
///
/// # Errors
///
/// Returns an error if the chain cannot be read or search fails.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stdout,
    clippy::wildcard_enum_match_arm,
    reason = "CLI command; strings consumed by design; wildcard arm for unknown OpKind variants"
)]
pub fn run(
    path: PathBuf,
    query: String,
    mode: String,
    top: usize,
    kind: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = SegmentStore::open(&path)?;
    let pages = store.read_all()?;

    // Build a lexical index from the chain.
    let mut lexical = LexicalIndex::new()?;
    let mut r#gen = 0u64;
    for page in &pages {
        for record in &page.records {
            if let Ok(op) = decode_op(&record.data) {
                drop(lexical.index_op(&op, r#gen)?);
                r#gen += 1;
            }
        }
    }
    lexical.commit()?;

    // Parse filters.
    let mut filters = SearchFilters::default();
    if let Some(k) = kind {
        filters.kinds = Some(
            k.split(',')
                .filter_map(|s| match s.trim() {
                    "message" => Some(TagFilter::Message),
                    "tool" => Some(TagFilter::Tool),
                    "command" => Some(TagFilter::Command),
                    "file" => Some(TagFilter::File),
                    _ => None,
                })
                .collect(),
        );
    }

    let search_mode = match mode.as_str() {
        "lexical" => SearchMode::Lexical,
        "vector" => SearchMode::Vector,
        _ => SearchMode::Hybrid,
    };

    // Build the hybrid search engine based on mode.
    let engine: HybridSearch = match search_mode {
        SearchMode::Lexical => HybridSearch::new(Box::new(lexical), Box::new(EmptyVectorSearch)),
        SearchMode::Vector | SearchMode::Hybrid => {
            let manifest = EmbeddingManifest::qwen3_embedding_0_6b();
            let vindex = VectorIndex::new(manifest.clone());
            let embedder: Box<dyn editchain_embed::Embedder> = Box::new(HttpEmbedder::new(
                manifest,
                "http://127.0.0.1:8001".to_string(),
            ));
            let mut wrapper = VectorSearchWrapper::new(vindex, embedder);

            // Collect all texts first, then embed in parallel batches.
            let mut entries: Vec<TextEntry> = Vec::new();
            for page in &pages {
                for record in &page.records {
                    if let Ok(op) = decode_op(&record.data) {
                        if let Some(text) = extract_op_text(&op, false, false) {
                            entries.push(TextEntry {
                                op_id: op.id,
                                chunk_ordinal: 0,
                                text,
                                kind: kind_to_string(&op.kind).to_string(),
                                session_id: None,
                            });
                        }
                    }
                }
            }

            // Split into small batches (server shares GPU with main model).
            let batch_size = 16;
            let text_batches: Vec<Vec<String>> = entries
                .chunks(batch_size)
                .map(|chunk| chunk.iter().map(|e| e.text.clone()).collect())
                .collect();

            // Use the embedder's parallel batch method.
            let all_vecs = wrapper
                .embedder_mut()
                .embed_batches(&text_batches)
                .map_err(|e| format!("embedding error: {e}"))?;

            // Flatten and add to vector index.
            for (entry, vec) in entries.iter().zip(all_vecs.iter().flat_map(|v| v.iter())) {
                let f16v = f32_to_f16_vec(vec);
                wrapper.index_mut().add_vector(
                    entry.op_id,
                    entry.chunk_ordinal,
                    &f16v,
                    &entry.kind,
                    entry.session_id,
                    r#gen,
                );
            }

            match search_mode {
                SearchMode::Vector => {
                    HybridSearch::new(Box::new(EmptyLexicalSearch), Box::new(wrapper))
                }
                _ => HybridSearch::new(Box::new(lexical), Box::new(wrapper)),
            }
        }
    };

    let request = SearchRequest {
        query,
        mode: search_mode,
        top_k: top,
        filters,
        ..SearchRequest::default()
    };

    let result = engine.search(&request);

    for chunk in &result.results {
        println!(
            "{} | score={:.4} | op={}",
            chunk.text, chunk.score, chunk.op_id
        );
    }
    println!("--- {} results ---", result.results.len());

    Ok(())
}

const fn kind_to_string(kind: &OpKind) -> &'static str {
    match kind {
        OpKind::ChainStart(_) => "chain_start",
        OpKind::Actor(_) => "actor",
        OpKind::Message(_) => "message",
        OpKind::Tool(_) => "tool",
        OpKind::Command(_) => "command",
        OpKind::File(_) => "file",
        OpKind::Reflection(_) => "reflection",
        OpKind::Import(_) => "import",
        OpKind::Note(_) => "note",
        OpKind::Error(_) => "error",
        OpKind::Unknown(_) => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Empty search stubs for single-mode operation
// ---------------------------------------------------------------------------

struct EmptyLexicalSearch;
impl LexicalSearch for EmptyLexicalSearch {
    fn search(
        &self,
        _query: &str,
        _filters: &SearchFilters,
        _top_k: usize,
    ) -> Vec<editchain_query::search::ScoredChunk> {
        Vec::new()
    }
}

struct EmptyVectorSearch;
impl VectorSearch for EmptyVectorSearch {
    fn search(
        &self,
        _query: &str,
        _filters: &SearchFilters,
        _top_k: usize,
    ) -> Vec<editchain_query::search::ScoredChunk> {
        Vec::new()
    }
}
