//! Command-line adapter over the shared `EditChain` engine APIs.

#[cfg(test)]
use tempfile as _;

mod cli;

fn main() -> std::process::ExitCode {
    cli::run()
}
