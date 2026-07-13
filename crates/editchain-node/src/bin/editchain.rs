use std::path::PathBuf;

use clap::{Parser, Subcommand};

use editchain_codec::frame::{decode_op, encode_op};
use editchain_codec::page::Page;
use editchain_core::*;
use editchain_import::import::import_claude_code;
use editchain_import::model::{DiscoveryRequest, ImportOptions};
use editchain_import::sink::{MemoryBlobSink, MemoryCursorStore, MemoryOpSink};
use editchain_node::segment::SegmentStore;

use editchain_embed::http::HttpEmbedder;
use editchain_embed::EmbeddingManifest;
use editchain_index::vector::{VectorIndex, VectorSearchWrapper};
use editchain_index::LexicalIndex;
use editchain_query::hybrid::HybridSearch;
use editchain_query::search::{SearchFilters, SearchMode, SearchRequest, TagFilter};

#[derive(Parser)]
#[command(name = "editchain", version, about = "Editchain CLI — CRDT-based agent edit history")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new edit chain
    Init {
        /// Path to the chain directory
        #[arg(default_value = ".editchain")]
        path: PathBuf,
    },
    /// Append an operation from JSON
    Append {
        /// JSON string of the operation
        json: String,
        /// Path to the chain directory
        #[arg(default_value = ".editchain")]
        path: PathBuf,
    },
    /// Dump the chain as JSON lines
    Dump {
        /// Path to the chain directory
        #[arg(default_value = ".editchain")]
        path: PathBuf,
    },
    /// Merge two chains (outputs merged JSON lines to stdout)
    Merge {
        /// First chain directory
        chain_a: PathBuf,
        /// Second chain directory
        chain_b: PathBuf,
    },
    /// Search the edit chain (BM25, vector, or hybrid)
    Search {
        /// Path to the chain directory
        path: PathBuf,
        /// Query string
        query: String,
        /// Search mode: lexical, vector, or hybrid
        #[arg(long, default_value = "hybrid")]
        mode: String,
        /// Number of results
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Filter by kind (message,tool,command,file)
        #[arg(long)]
        kind: Option<String>,
    },
    /// Tail the edit chain (follow new operations)
    Tail {
        /// Path to the chain directory
        path: PathBuf,
        /// Follow new operations as they are appended
        #[arg(long, default_value_t = false)]
        follow: bool,
        /// Only show operations since this generation
        #[arg(long)]
        since: Option<u64>,
    },
    /// Retrieve an operation or chunk by ID
    Retrieve {
        /// Path to the chain directory
        path: PathBuf,
        /// Operation ID to retrieve
        #[arg(long)]
        op: Option<String>,
    },
    /// Import Claude Code sessions into the edit chain
    Import {
        /// Path to the Claude Code sessions directory
        #[arg(long, default_value = "")]
        sessions_dir: String,
        /// Path to the workspace root
        #[arg(long, default_value = ".")]
        workspace: String,
        /// Path to the output chain directory
        #[arg(long, default_value = ".editchain")]
        chain: String,
        /// Dry run — print ops without writing
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { path } => {
            println!("Initialized editchain at: {}", path.display());
            // Write a ChainStart operation as the first record
            let start_op = Op {
                id: OpId::new(NodeId(0), 0, 0),
                parents: parents::ParentSet::None,
                actor: ActorId(0),
                clock: clock::Clock::None,
                scope: scope::ScopeRef::None,
                tags: tags::Tags::NONE,
                kind: op::OpKind::ChainStart(op::ChainStart {
                    name: b"editchain".to_vec(),
                    version: 2,
                }),
            };
            let encoded = encode_op(&start_op)?;
            let mut page = Page::new(0);
            page.add_record(0, encoded);
            // We need a mutable store — re-open
            let mut store = SegmentStore::open(&path)?;
            store.append_page(&page)?;
            println!("Wrote ChainStart operation.");
        }
        Commands::Append { path, json } => {
            let op: Op = serde_json::from_str(&json)?;
            let encoded = encode_op(&op)?;
            let mut page = Page::new(0);
            page.add_record(0, encoded);
            let mut store = SegmentStore::open(&path)?;
            store.append_page(&page)?;
            println!("Appended operation: {}", op.id);
        }
        Commands::Dump { path } => {
            let store = SegmentStore::open(&path)?;
            let pages = store.read_all()?;
            for page in &pages {
                for record in &page.records {
                    match decode_op(&record.data) {
                        Ok(op) => {
                            let json = serde_json::to_string(&op)?;
                            println!("{}", json);
                        }
                        Err(e) => {
                            eprintln!("Warning: failed to decode record: {}", e);
                        }
                    }
                }
            }
        }
        Commands::Search { path, query, mode, top, kind } => {
            let store = SegmentStore::open(&path)?;
            let pages = store.read_all()?;

            // Build a lexical index from the chain.
            let mut lexical = LexicalIndex::new()?;
            let mut gen = 0u64;
            for page in &pages {
                for record in &page.records {
                    if let Ok(op) = decode_op(&record.data) {
                        lexical.index_op(&op, gen)?;
                        gen += 1;
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
                SearchMode::Lexical => {
                    HybridSearch::new(Box::new(lexical), Box::new(EmptyVectorSearch))
                }
                SearchMode::Vector | SearchMode::Hybrid => {
                    let manifest = EmbeddingManifest::qwen3_embedding_0_6b();
                    let vindex = VectorIndex::new(manifest.clone());
                    let embedder: Box<dyn editchain_embed::Embedder> = Box::new(HttpEmbedder::new(
                        manifest,
                        "http://127.0.0.1:8001".to_string(),
                    ));
                    let mut wrapper = VectorSearchWrapper::new(vindex, embedder);

                    // Collect all texts first, then embed in parallel batches.
                    struct TextEntry {
                        op_id: editchain_core::OpId,
                        chunk_ordinal: u32,
                        text: String,
                        kind: String,
                        session_id: Option<u64>,
                    }
                    let mut entries: Vec<TextEntry> = Vec::new();
                    for page in &pages {
                        for record in &page.records {
                            if let Ok(op) = decode_op(&record.data) {
                                if let Some(text) = editchain_index::chunker::extract_op_text(&op, false, false) {
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
                    let text_batches: Vec<Vec<String>> = entries.chunks(batch_size).map(|chunk| {
                        chunk.iter().map(|e| e.text.clone()).collect()
                    }).collect();

                    // Use the embedder's parallel batch method.
                    let all_vecs = wrapper.embedder_mut().embed_batches(&text_batches)
                        .map_err(|e| format!("embedding error: {}", e))?;

                    // Flatten and add to vector index.
                    for (entry, vec) in entries.iter().zip(all_vecs.iter().flat_map(|v| v.iter())) {
                        let f16v = editchain_index::vector::f32_to_f16_vec(vec);
                        wrapper.index_mut().add_vector(
                            entry.op_id,
                            entry.chunk_ordinal,
                            &f16v,
                            &entry.kind,
                            entry.session_id,
                            gen,
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
                println!("{} | score={:.4} | op={}", chunk.text, chunk.score, chunk.op_id);
            }
            println!("--- {} results ---", result.results.len());
        }
        Commands::Tail { path, follow, since } => {
            let store = SegmentStore::open(&path)?;
            let pages = store.read_all()?;

            let mut start_gen = since.unwrap_or(0);
            for page in &pages {
                for record in &page.records {
                    if let Ok(op) = decode_op(&record.data) {
                        if start_gen > 0 {
                            start_gen -= 1;
                            continue;
                        }
                        let json = serde_json::to_string(&op)?;
                        println!("{}", json);
                    }
                }
            }

            if follow {
                eprintln!("Warning: --follow requires daemon mode (not yet implemented)");
            }
        }
        Commands::Retrieve { path, op } => {
            if let Some(op_str) = op {
                let store = SegmentStore::open(&path)?;
                let pages = store.read_all()?;
                for page in &pages {
                    for record in &page.records {
                        if let Ok(op) = decode_op(&record.data) {
                            if op.id.to_string() == op_str {
                                let json = serde_json::to_string_pretty(&op)?;
                                println!("{}", json);
                            }
                        }
                    }
                }
            }
        }
        Commands::Import { sessions_dir, workspace, chain, dry_run } => {
            let sessions_path = if sessions_dir.is_empty() {
                // Try to auto-detect the Claude Code project directory.
                let cwd = std::env::current_dir()?;
                let cwd_str = cwd.to_string_lossy().to_string();
                let encoded = cwd_str.replace(['/', '.'], "-");
                let home = dirs::home_dir().ok_or("no home directory")?;
                home.join(".claude").join("projects").join(encoded)
            } else {
                PathBuf::from(&sessions_dir)
            };

            let request = DiscoveryRequest {
                workspace_path: PathBuf::from(&workspace),
                sessions_dir: sessions_path,
                chain_dir: PathBuf::from(&chain),
            };

            let options = ImportOptions::default();
            let mut ops_sink = MemoryOpSink::new();
            let mut blobs_sink = MemoryBlobSink::new();
            let mut cursors = MemoryCursorStore::new();

            let report = import_claude_code(&request, &options, &mut ops_sink, &mut blobs_sink, &mut cursors)?;

            println!("Import complete:");
            println!("  Files discovered: {}", report.files_discovered);
            println!("  Files processed: {}", report.files_processed);
            println!("  Raw ops: {}", report.raw_ops);
            println!("  Normalized ops: {}", report.normalized_ops);
            println!("  Duplicates: {}", report.duplicates);
            println!("  Malformed: {}", report.malformed);

            if !dry_run && !ops_sink.ops.is_empty() {
                // Write ops to the chain store.
                let mut store = SegmentStore::open(PathBuf::from(&chain))?;
                let mut page = Page::new(0);
                for op in &ops_sink.ops {
                    let encoded = encode_op(op)?;
                    page.add_record(0, encoded);
                }
                store.append_page(&page)?;
                println!("Wrote {} operations to chain.", ops_sink.ops.len());
            }

            if dry_run {
                println!("\n--- Dry run: first 5 ops ---");
                for op in ops_sink.ops.iter().take(5) {
                    let json = serde_json::to_string(op)?;
                    println!("{}", json);
                }
            }
        }
        Commands::Merge { chain_a, chain_b } => {
            let store_a = SegmentStore::open(&chain_a)?;
            let store_b = SegmentStore::open(&chain_b)?;

            let pages_a = store_a.read_all()?;
            let pages_b = store_b.read_all()?;

            // Collect all ops from both chains
            let mut ops_a = Vec::new();
            let mut ops_b = Vec::new();

            for page in &pages_a {
                for record in &page.records {
                    if let Ok(op) = decode_op(&record.data) {
                        ops_a.push(op);
                    }
                }
            }

            for page in &pages_b {
                for record in &page.records {
                    if let Ok(op) = decode_op(&record.data) {
                        ops_b.push(op);
                    }
                }
            }

            // Build OpSets and merge
            let mut state_a = state::ChainState::new();
            for op in &ops_a {
                let encoded = encode_op(op)?;
                state_a.ops.insert(op.id, encoded).ok();
            }

            let mut state_b = state::ChainState::new();
            for op in &ops_b {
                let encoded = encode_op(op)?;
                state_b.ops.insert(op.id, encoded).ok();
            }

            let (accepted, duplicates, quarantined) =
                state_a.merge(&state_b)?;

            eprintln!(
                "Merge complete: {} accepted, {} duplicates, {} quarantined",
                accepted, duplicates, quarantined
            );

            // Output merged ops as JSON lines
            for (id, bytes) in state_a.ops.iter() {
                match decode_op(bytes) {
                    Ok(op) => {
                        let json = serde_json::to_string(&op)?;
                        println!("{}", json);
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to decode op {}: {}", id, e);
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Empty search stubs for single-mode operation
// ---------------------------------------------------------------------------

struct EmptyLexicalSearch;
impl editchain_query::hybrid::LexicalSearch for EmptyLexicalSearch {
    fn search(&self, _query: &str, _filters: &SearchFilters, _top_k: usize) -> Vec<editchain_query::search::ScoredChunk> {
        Vec::new()
    }
}

struct EmptyVectorSearch;
impl editchain_query::hybrid::VectorSearch for EmptyVectorSearch {
    fn search(&self, _query: &str, _filters: &SearchFilters, _top_k: usize) -> Vec<editchain_query::search::ScoredChunk> {
        Vec::new()
    }
}

fn kind_to_string(kind: &editchain_core::op::OpKind) -> &'static str {
    use editchain_core::op::OpKind;
    match kind {
        OpKind::Message(_) => "message",
        OpKind::Tool(_) => "tool",
        OpKind::Command(_) => "command",
        OpKind::File(_) => "file",
        OpKind::Reflection(_) => "reflection",
        OpKind::Import(_) => "import",
        OpKind::Note(_) => "note",
        OpKind::Error(_) => "error",
        _ => "unknown",
    }
}