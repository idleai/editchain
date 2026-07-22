use clap as _;
use dirs as _;
use editchain_codec as _;
use editchain_embed as _;
use editchain_import as _;
use editchain_index as _;
use editchain_query as _;
use serde as _;
use serde_json as _;

#[cfg(test)]
use tempfile as _;

use editchain_core::Op;

/// Export a slice of operations as pretty-printed JSON lines.
///
/// # Errors
///
/// Returns an error if any operation cannot be serialized to JSON.
pub fn export_json(ops: &[Op]) -> serde_json::Result<String> {
    let mut lines = Vec::new();
    for op in ops {
        let json = serde_json::to_string_pretty(op)?;
        lines.push(json);
    }
    Ok(lines.join("\n"))
}

/// Export a single operation as a compact JSON string.
///
/// # Errors
///
/// Returns an error if the operation cannot be serialized to JSON.
pub fn op_to_json(op: &Op) -> serde_json::Result<String> {
    serde_json::to_string(op)
}
