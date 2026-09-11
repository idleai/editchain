//! Immutable content-block revisions and complete per-record manifests.

use editchain_core::provider::{
    ClaudeDerivationContract, ClaudeDerivationEvidence, ProviderEvidence, ProviderEvidenceSchema,
    ProviderFact,
};
use editchain_core::{
    ActorId, Clock, ImportOp, NoteOp, NoteRelationship, Op, OpId, OpKind, ParentSet, Payload,
    ScopeRef, Tags,
};

use super::content::{normalize_content, Contract};
use super::envelope::CcEnvelope;
use super::normalize::{normalize_envelope, NormalizeOptions};
use crate::ids::{derive_external_entity_id, SourcePosition, SourceStream};
use crate::sink::BlobSink;
use crate::source_read::LineWithHash;
use crate::ImportError;

pub(crate) const CONTRACT: &str = "claude-blocks-v1";

pub(crate) fn raw_record(
    envelope: Option<&CcEnvelope>,
    line: &LineWithHash,
    source: OpId,
    fallback_session: &str,
    blobs: &mut dyn BlobSink,
) -> Result<Op, ImportError> {
    let stream = SourceStream::new(source.node, source.boot);
    let ordinal = source.seq >> 16;
    let mut raw = if let Some(envelope) = envelope {
        normalize_envelope(
            envelope,
            line.hash,
            &line.data,
            &stream,
            ordinal,
            &NormalizeOptions {
                normalize: false,
                include_thinking: false,
            },
            blobs,
            fallback_session,
        )?
        .0
    } else {
        // Existing malformed raw records use inline bytes. Preserve that
        // representation under their physical IDs; source and sink bounds
        // still reject oversized records without accepting a checkpoint.
        Op {
            id: stream.op_from_position(SourcePosition::raw(ordinal))?,
            parents: ParentSet::None,
            actor: ActorId(0),
            clock: Clock::None,
            scope: ScopeRef::None,
            tags: Tags::IMPORT | Tags::ERROR,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(line.data.clone()),
                raw_hash: Some(line.hash),
            }),
        }
    };
    if let Some(previous) = ordinal.checked_sub(1).filter(|previous| *previous > 0) {
        raw.parents = ParentSet::One(stream.op_from_position(SourcePosition::raw(previous))?);
    }
    Ok(raw)
}

pub(crate) fn record_outputs(
    envelope: Option<&CcEnvelope>,
    raw: &Op,
    raw_hash: [u8; 32],
    include_thinking: bool,
    blobs: &mut dyn BlobSink,
) -> Result<Vec<Op>, ImportError> {
    let mut ops = match envelope {
        Some(envelope) => {
            normalize_content(envelope, raw, include_thinking, Contract::BlocksV1, blobs)?
        }
        None => Vec::new(),
    };
    let evidence = ProviderEvidence {
        schema: ProviderEvidenceSchema::V1,
        source: raw.id,
        raw_hash,
        fact: ProviderFact::ClaudeDerivation(ClaudeDerivationEvidence {
            contract: ClaudeDerivationContract::BlocksV1,
            includes_thinking: include_thinking,
            outputs: ops.iter().map(|op| op.id).collect(),
        }),
    };
    let content = serde_json::to_string(&evidence)?;
    ops.push(Op {
        id: derive_external_entity_id("claude:provider-evidence:v1", &content),
        parents: ParentSet::One(raw.id),
        actor: ActorId(0),
        clock: Clock::None,
        scope: raw.scope,
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::ProviderEvidence,
            content: Payload::Inline(content.into_bytes()),
        }),
    });
    Ok(ops)
}
