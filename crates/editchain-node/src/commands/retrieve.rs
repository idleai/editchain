//! Retrieve an operation or chunk by ID.

use std::path::PathBuf;

use crate::segment::SegmentStore;
use editchain_codec::frame::decode_op;

/// Run the `retrieve` command.
///
/// # Errors
///
/// Returns an error if the chain directory cannot be read.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stdout,
    reason = "CLI command; path consumed by design"
)]
pub fn run(path: PathBuf, op: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(op_str) = op {
        let store = SegmentStore::open(&path)?;
        let pages = store.read_all()?;
        for page in &pages {
            for record in &page.records {
                if let Ok(op) = decode_op(&record.data) {
                    if op.id.to_string() == op_str {
                        let json = serde_json::to_string_pretty(&op)?;
                        println!("{json}");
                    }
                }
            }
        }
    }
    Ok(())
}
