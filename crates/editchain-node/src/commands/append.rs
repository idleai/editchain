//! Append an operation from JSON.

use std::path::PathBuf;

use crate::segment::SegmentStore;
use editchain_codec::frame::encode_op;
use editchain_codec::page::Page;
use editchain_core::Op;

/// Run the `append` command.
///
/// # Errors
///
/// Returns an error if the JSON cannot be parsed or the operation
/// cannot be written to the chain.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stdout,
    reason = "CLI command; path and json are consumed by design"
)]
pub fn run(path: PathBuf, json: String) -> Result<(), Box<dyn std::error::Error>> {
    let op: Op = serde_json::from_str(&json)?;
    let encoded = encode_op(&op)?;
    let mut page = Page::new(0);
    page.add_record(0, encoded);
    let mut store = SegmentStore::open(&path)?;
    store.append_page(&page)?;
    println!("Appended operation: {}", op.id);
    Ok(())
}
