//! Prepare the resumable native live checkpoint.

use std::path::PathBuf;

/// Run the `prepare-view` command.
///
/// # Errors
///
/// Returns an error if the chain cannot be projected or the derived snapshot
/// cannot be published durably.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stdout,
    reason = "CLI command consumes paths and reports the generated artifact"
)]
pub(super) fn run(workspace: PathBuf, chain: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let report = crate::history::prepare_live_checkpoint(&workspace, &chain)?;
    println!(
        "Live checkpoint ready: {} visible rows, {} operations at {}/live-v1",
        report.nodes, report.chain_generation, report.chain
    );
    Ok(())
}
