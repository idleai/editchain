//! Editchain CLI entry point — parses arguments and dispatches to command handlers.

use blake3 as _;
use clap::Parser;
use dirs as _;
use editchain_core as _;
use editchain_git as _;
use editchain_import as _;
use editchain_index as _;
use editchain_project as _;
use editchain_protocol as _;
use editchain_store as _;
use serde as _;
use serde_json as _;
use tantivy as _;
#[cfg(test)]
use tempfile as _;

use editchain_node::commands::Cli;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let options = editchain_import::ImportOptions::default();
    if matches!(cli.command, editchain_node::commands::Commands::Import(_)) {
        let cancellation = options.cancellation.clone();
        ctrlc::set_handler(move || cancellation.cancel())?;
    }
    editchain_node::commands::dispatch_with_import_options(cli.command, &options)
}
