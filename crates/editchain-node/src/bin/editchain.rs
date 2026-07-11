use std::path::PathBuf;

use clap::{Parser, Subcommand};

use editchain_codec::frame::{decode_op, encode_op};
use editchain_codec::page::Page;
use editchain_core::*;
use editchain_import::import::import_claude_code;
use editchain_import::model::{DiscoveryRequest, ImportOptions};
use editchain_import::sink::{MemoryBlobSink, MemoryCursorStore, MemoryOpSink};
use editchain_node::segment::SegmentStore;

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