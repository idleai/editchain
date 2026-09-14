//! Shared human-work decoding for historical and retained live projections.

use editchain_core::{human::HumanWorkRecord, Op, OpKind, Payload};

/// Decode the bounded, versioned semantic envelope, never raw buffer contents.
#[must_use]
pub fn work_record(op: &Op) -> Option<HumanWorkRecord> {
    let OpKind::Import(import) = &op.kind else {
        return None;
    };
    let Payload::Inline(bytes) = &import.raw_ref else {
        return None;
    };
    if bytes.len() > 65536 {
        return None;
    }
    let record: HumanWorkRecord = serde_json::from_slice(bytes).ok()?;
    (record.source == "vscode.work" && record.schema == 1).then_some(record)
}
