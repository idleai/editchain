//! Dump the chain as JSON lines.

use std::path::PathBuf;

use crate::segment::SegmentStore;
use editchain_codec::frame::decode_op;

/// Run the `dump` command.
///
/// # Errors
///
/// Returns an error if the chain directory cannot be read.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "CLI command; path consumed by design"
)]
pub fn run(path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let store = SegmentStore::open(&path)?;
    let pages = store.read_all()?;
    for page in &pages {
        for record in &page.records {
            match decode_op(&record.data) {
                Ok(op) => {
                    let json = serde_json::to_string(&op)?;
                    println!("{json}");
                }
                Err(e) => {
                    eprintln!("Warning: failed to decode record: {e}");
                }
            }
        }
    }
    Ok(())
}
