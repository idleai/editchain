//! The `editchain` command-line interface over shared engine APIs.
//!
//! See `docs/cli.md` for inputs, output framing, evidence archives and exit codes.

mod archive;
mod error;
mod follow;
mod imports;
mod input;
mod operations;
pub(crate) mod output;
mod query;
mod replication;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use editchain_engine::Engine;
use editchain_import::ImportOptions;
use serde_json::json;

use error::{Failure, Result};
use output::{Format, Output};

#[derive(Debug, Parser)]
#[command(
    name = "editchain",
    args_override_self = true,
    version,
    about = "Immutable human and agent history",
    after_help = "Exit codes: 0 success, 1 failure, 2 invalid input, 3 missing/incomplete, 4 conflict/integrity, 5 busy, 130 interrupted. See docs/cli.md."
)]
struct Cli {
    /// Chain directory; relative paths resolve against the current directory.
    #[arg(long, global = true, default_value = ".editchain")]
    chain: PathBuf,
    /// Result format. Diagnostics always go to stderr.
    #[arg(
        long,
        alias = "format",
        global = true,
        value_enum,
        default_value = "human"
    )]
    output: Format,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize storage, optionally appending caller-authored `ChainStart` records.
    Init {
        /// JSON/JSONL `ChainStart` operation file, or - for stdin.
        #[arg(long)]
        input: Option<PathBuf>,
    },
    /// Append operation envelopes or replay an exact evidence archive.
    Append(operations::Append),
    /// Import provider history through reusable capture APIs.
    Import(imports::Args),
    /// Migrate old storage into a new EC03 chain; interrupted work is resumable.
    Migrate {
        /// New chain directory, outside the source chain.
        #[arg(long)]
        destination: PathBuf,
        /// Convert operations to the ten-type schema, preserving the source chain.
        #[arg(long)]
        schema3: bool,
    },
    /// Export all exact record variants and their referenced blobs.
    Export,
    /// Append caller-authored Note operation envelopes.
    Annotate(input::Input),
    /// Append caller-authored Reflection operation envelopes.
    Reflect(input::Input),
    #[command(flatten)]
    Query(Box<query::Command>),
    /// Store raw bytes, including bytes piped on stdin.
    StoreBlob(input::Input),
    /// Resolve a content ID (JSON), optionally writing exact bytes.
    Blob {
        #[arg(value_parser = input::json::<editchain_engine::ContentId>)]
        id: editchain_engine::ContentId,
        #[arg(long)]
        raw: bool,
    },
    /// Follow accepted history, retractions and late content via index refresh.
    #[command(alias = "subscribe")]
    Follow(follow::Args),
    /// Verify stored records, referenced content and the derived index.
    #[command(alias = "check")]
    Integrity,
    /// Rebuild the derived index, including recovery from a corrupt checkpoint.
    Rebuild,
    /// Replicate with a local chain or an authorized caller-owned stdio transport.
    Replicate(replication::Args),
}

/// Parse process arguments, execute the command, and return a stable exit code.
/// Result bytes use stdout; usage and execution diagnostics use stderr.
#[must_use]
pub(crate) fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            if let Err(error) = error.print() {
                return report(&error.into());
            }
            return ExitCode::from(code);
        }
    };
    let may_close_result_pipe = matches!(
        &cli.command,
        Command::Query(_)
            | Command::Export
            | Command::Blob { .. }
            | Command::Follow(_)
            | Command::Integrity
    );
    let options = ImportOptions::default();
    if matches!(
        &cli.command,
        Command::Import(_) | Command::Follow(_) | Command::Replicate(_) | Command::Migrate { .. }
    ) {
        let cancellation = options.cancellation.clone();
        if let Err(error) = ctrlc::set_handler(move || cancellation.cancel()) {
            return report(&Failure::new(1, error.to_string()));
        }
    }
    let mut output = Output::new(cli.output);
    let result = execute(cli, &mut output, &options);
    let finished = output.finish();
    match result.and(finished) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.code == 0 && !may_close_result_pipe => report(&Failure::new(
            1,
            "result pipe closed; writes may be partially durable, so retry the same immutable input",
        )),
        Err(error) => report(&error),
    }
}

fn report(error: &Failure) -> ExitCode {
    if error.code != 0 {
        // A closed stderr cannot change the already established process outcome.
        let _diagnostic = output::diagnostic(&error.message);
    }
    ExitCode::from(error.code)
}

fn execute(cli: Cli, output: &mut Output, options: &ImportOptions) -> Result<()> {
    let chain = &cli.chain;
    match cli.command {
        Command::Init { input } => {
            if let Some(input) = input {
                operations::append_kind(chain, &input, operations::Kind::Chain, output)?;
            } else {
                let engine = Engine::open(chain)?;
                output.emit(&json!({"chain":engine.chain_dir(),"initialized":true}))?;
            }
            Ok(())
        }
        Command::Append(args) => operations::append(chain, &args, output),
        Command::Import(args) => imports::run(chain, &args, options, output),
        Command::Export => archive::export(chain, output),
        Command::Migrate {
            destination,
            schema3,
        } => {
            require_chain(chain)?;
            let migrate = if schema3 {
                editchain_import::activity::migrate
            } else {
                editchain_store::migration::migrate
            };
            output.emit(&migrate(chain, &destination, || {
                options.cancellation.is_cancelled()
            })?)
        }
        Command::Annotate(args) => {
            operations::append_kind(chain, &args.input, operations::Kind::Note, output)
        }
        Command::Reflect(args) => {
            operations::append_kind(chain, &args.input, operations::Kind::Reflection, output)
        }
        Command::Query(command) => query::run(chain, *command, output),
        Command::StoreBlob(args) => {
            require_chain(chain)?;
            let bytes = input::bytes(&args.input)?;
            output.emit(&Engine::open(chain)?.store_blob(&bytes)?)
        }
        Command::Blob { id, raw } => operations::blob(chain, id, raw, output),
        Command::Follow(args) => follow::run(chain, &args, &options.cancellation, output),
        Command::Integrity => operations::integrity(chain, output),
        Command::Rebuild => {
            require_chain(chain)?;
            output.emit(&editchain_index::ChainIndex::rebuild_at(chain)?.stats())
        }
        Command::Replicate(args) => replication::run(chain, &args, &options.cancellation, output),
    }
}

fn require_chain(chain: &Path) -> Result<()> {
    match std::fs::metadata(chain) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(Failure::input(format!(
            "chain is not a directory: {}",
            chain.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(Failure::new(
            3,
            format!("chain does not exist: {}; run init first", chain.display()),
        )),
        Err(error) => Err(error.into()),
    }
}
