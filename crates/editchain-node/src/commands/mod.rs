//! CLI command implementations.

pub mod append;
pub mod dump;
pub mod import;
pub mod init;
pub mod merge;
pub mod retrieve;
pub mod search;
pub mod tail;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Editchain CLI — subcommands and dispatch.
#[derive(Parser, Debug)]
#[command(
    name = "editchain",
    version,
    about = "Editchain CLI — CRDT-based agent edit history"
)]
pub struct Cli {
    #[command(subcommand)]
    /// The subcommand to execute.
    pub command: Commands,
}

/// Available subcommands.
#[derive(Subcommand, Debug)]
pub enum Commands {
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

/// Dispatch a command to its handler.
///
/// # Errors
///
/// Returns an error if the command fails.
pub fn dispatch(command: Commands) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Commands::Init { path } => init::run(path),
        Commands::Append { path, json } => append::run(path, json),
        Commands::Dump { path } => dump::run(path),
        Commands::Merge { chain_a, chain_b } => merge::run(chain_a, chain_b),
        Commands::Search {
            path,
            query,
            mode,
            top,
            kind,
        } => search::run(path, query, mode, top, kind),
        Commands::Tail {
            path,
            follow,
            since,
        } => tail::run(path, follow, since),
        Commands::Retrieve { path, op } => retrieve::run(path, op),
        Commands::Import {
            sessions_dir,
            workspace,
            chain,
            dry_run,
        } => import::run(sessions_dir, workspace, chain, dry_run),
    }
}
