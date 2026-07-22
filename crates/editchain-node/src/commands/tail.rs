//! Tail the edit chain.

use std::path::PathBuf;

use crate::segment::SegmentStore;
use editchain_codec::frame::decode_op;

/// Run the `tail` command.
///
/// # Errors
///
/// Returns an error if the chain directory cannot be read.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::needless_pass_by_value,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "CLI command; path consumed by design; start_gen is a bounded counter"
)]
pub fn run(
    path: PathBuf,
    follow: bool,
    since: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = SegmentStore::open(&path)?;
    let pages = store.read_all()?;

    let mut start_gen = since.unwrap_or(0);
    for page in &pages {
        for record in &page.records {
            if let Ok(op) = decode_op(&record.data) {
                if start_gen > 0 {
                    start_gen -= 1;
                    continue;
                }
                let json = serde_json::to_string(&op)?;
                println!("{json}");
            }
        }
    }

    if follow {
        eprintln!("Warning: --follow requires daemon mode (not yet implemented)");
    }

    Ok(())
}
