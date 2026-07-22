//! Merge two chains.

use std::path::PathBuf;

use crate::segment::SegmentStore;
use editchain_codec::frame::{decode_op, encode_op};
use editchain_core::state;

/// Run the `merge` command.
///
/// # Errors
///
/// Returns an error if either chain cannot be read or operations
/// cannot be decoded.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "CLI command; paths consumed by design"
)]
pub fn run(chain_a: PathBuf, chain_b: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let store_a = SegmentStore::open(&chain_a)?;
    let store_b = SegmentStore::open(&chain_b)?;

    let pages_a = store_a.read_all()?;
    let pages_b = store_b.read_all()?;

    // Collect all ops from both chains
    let mut ops_a = Vec::new();
    let mut ops_b = Vec::new();

    for page in &pages_a {
        for record in &page.records {
            if let Ok(op) = decode_op(&record.data) {
                ops_a.push(op);
            }
        }
    }

    for page in &pages_b {
        for record in &page.records {
            if let Ok(op) = decode_op(&record.data) {
                ops_b.push(op);
            }
        }
    }

    // Build OpSets and merge
    let mut state_a = state::ChainState::new();
    for op in &ops_a {
        let encoded = encode_op(op)?;
        let _: Option<bool> = state_a.ops.insert(op.id, encoded).ok();
    }

    let mut state_b = state::ChainState::new();
    for op in &ops_b {
        let encoded = encode_op(op)?;
        let _: Option<bool> = state_b.ops.insert(op.id, encoded).ok();
    }

    let (accepted, duplicates, quarantined) = state_a.merge(&state_b)?;

    eprintln!(
        "Merge complete: {accepted} accepted, {duplicates} duplicates, {quarantined} quarantined"
    );

    // Output merged ops as JSON lines
    for (id, bytes) in state_a.ops.iter() {
        match decode_op(bytes) {
            Ok(op) => {
                let json = serde_json::to_string(&op)?;
                println!("{json}");
            }
            Err(e) => {
                eprintln!("Warning: failed to decode op {id}: {e}");
            }
        }
    }

    Ok(())
}
