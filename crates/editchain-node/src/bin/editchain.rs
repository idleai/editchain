use std::path::PathBuf;

use clap::{Parser, Subcommand};

use editchain_codec::frame::{decode_op, encode_op};
use editchain_codec::page::Page;
use editchain_core::*;
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