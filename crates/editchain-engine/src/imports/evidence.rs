//! Typed provider observations, independent of presentation.

use crate::provider::ProviderEvidence;
use crate::{NoteRelationship, Op, OpKind, ParentSet, Payload, Tags};

/// A typed provider observation and the immutable note carrying it.
#[derive(Debug)]
pub struct EvidenceRecord<'a> {
    /// Original provider-evidence note.
    pub op: &'a Op,
    /// Decoded versioned provider fact.
    pub payload: ProviderEvidence,
}

/// Decode an inline provider-evidence note with the required envelope shape.
/// Source hashes, scope and derivation completeness are validated during reconciliation.
#[must_use]
pub fn decode_evidence(op: &Op) -> Option<EvidenceRecord<'_>> {
    let OpKind::Note(note) = &op.kind else {
        return None;
    };
    if note.relationship != NoteRelationship::ProviderEvidence
        || !note.target_ids.is_empty()
        || !op.tags.matches_all(Tags::META | Tags::IMPORT)
    {
        return None;
    }
    let Payload::Inline(content) = &note.content else {
        return None;
    };
    let payload: ProviderEvidence = serde_json::from_slice(content).ok()?;
    (op.parents == ParentSet::One(payload.source)).then_some(EvidenceRecord { op, payload })
}
