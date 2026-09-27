//! Command-line adapter over the shared `EditChain` engine APIs.

mod cli;

fn main() -> std::process::ExitCode {
    cli::run()
}
