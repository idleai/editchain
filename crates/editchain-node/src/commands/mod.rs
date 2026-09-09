//! CLI command implementations.

pub mod import;
pub mod prepare_view;

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
    /// Import agent sessions (Claude Code or Codex) into the edit chain
    Import {
        /// Sessions directory — auto-detected when empty (Claude:
        /// `~/.claude/projects/<encoded-cwd>`; Codex: `~/.codex/sessions`)
        #[arg(long, default_value = "")]
        sessions_dir: String,
        /// Session provider to import from
        #[arg(long, value_enum, default_value_t = Provider::Claude)]
        provider: Provider,
        /// Helper program that projects Codex rollouts (default:
        /// `codex-session-exporter` on PATH); requires `--provider codex`
        #[arg(long)]
        codex_helper: Option<String>,
        /// Fixed prefix argument passed to the Codex helper before the rollout
        /// path (repeatable); requires `--provider codex`
        #[arg(long, action = clap::ArgAction::Append, allow_hyphen_values = true)]
        codex_helper_arg: Vec<String>,
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
    /// Pregenerate the fixed-view VS Code render snapshot
    PrepareView {
        /// Path to the workspace root
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        /// Path to the `EditChain` directory, relative to the workspace root
        #[arg(long, default_value = ".editchain")]
        chain: PathBuf,
    },
}

/// Session provider to import from.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Claude Code sessions (`~/.claude/projects`).
    Claude,
    /// Codex rollouts (`~/.codex/sessions`).
    Codex,
}

/// Dispatch a command to its handler.
///
/// # Errors
///
/// Returns an error if the command fails.
pub fn dispatch(command: Commands) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Commands::Import {
            sessions_dir,
            workspace,
            chain,
            dry_run,
            provider,
            codex_helper,
            codex_helper_arg,
        } => import::run(
            sessions_dir,
            workspace,
            chain,
            dry_run,
            provider,
            codex_helper,
            codex_helper_arg,
        ),
        Commands::PrepareView { workspace, chain } => prepare_view::run(workspace, chain),
    }
}
