//! Pregenerate the immutable VS Code render snapshot.

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
pub fn run(workspace: PathBuf, chain: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let report = editchain_vscode_service::prepare_render_snapshot(&workspace, &chain)?;
    println!(
        "Render snapshot {}: {} rows ({} top-level), {} bytes at {}",
        if report.reused { "reused" } else { "generated" },
        report.rows,
        report.top_level_rows,
        report.bytes,
        report.path.display()
    );
    Ok(())
}
