//! Editchain CLI entry point — parses arguments and dispatches to command handlers.

use clap::Parser;
use dirs as _;
use editchain_codec as _;
use editchain_core as _;
use editchain_embed as _;
use editchain_import as _;
use editchain_index as _;
use editchain_query as _;
use serde as _;
use serde_json as _;

#[cfg(test)]
use tempfile as _;

use editchain_node::commands::Cli;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    editchain_node::commands::dispatch(cli.command)
}
