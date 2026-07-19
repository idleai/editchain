use editchain_core::Op;
use serde_json;

/// Export a slice of operations as pretty-printed JSON lines.
pub fn export_json(ops: &[Op]) -> serde_json::Result<String> {
    let mut lines = Vec::new();
    for op in ops {
        let json = serde_json::to_string_pretty(op)?;
        lines.push(json);
    }
    Ok(lines.join("\n"))
}

/// Export a single operation as a compact JSON string.
pub fn op_to_json(op: &Op) -> serde_json::Result<String> {
    serde_json::to_string(op)
}

