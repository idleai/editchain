//! Read operations from a segment store.

use std::path::Path;

use crate::segment::SegmentStore;
use editchain_codec::frame::decode_op;
use editchain_core::Op;

/// Read all decoded operations from a chain directory.
///
/// # Errors
///
/// Returns an IO error if the chain directory cannot be read.
#[expect(
    clippy::print_stderr,
    reason = "CLI service; warning output is intentional"
)]
pub fn read_all_ops(path: &Path) -> Result<Vec<Op>, Box<dyn std::error::Error>> {
    let store = SegmentStore::open(path)?;
    let pages = store.read_all()?;
    let mut ops = Vec::new();
    for page in &pages {
        for record in &page.records {
            match decode_op(&record.data) {
                Ok(op) => ops.push(op),
                Err(e) => {
                    eprintln!("Warning: failed to decode record: {e}");
                }
            }
        }
    }
    Ok(ops)
}
