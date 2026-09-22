// Exact occurrence fixtures shared by replication and cached-view tests.

use editchain_core::provider::{
    CodexDerivationContract, CodexDerivationEvidence, CodexLogicalChange, CodexThreadId,
    ProviderEvidence, ProviderEvidenceSchema, ProviderFact,
};
use editchain_core::{
    ActorId, Clock, ImportOp, MessageOp, NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind,
    ParentSet, Payload, ScopeRef, SessionId, Tags,
};
use std::{io, path::Path};

pub(super) fn occurrence(ordinal: u64, incarnation: u64, text: &str) -> io::Result<Vec<Op>> {
    let sequence = ordinal
        .checked_shl(16)
        .ok_or_else(|| io::Error::other("ordinal"))?;
    let source = OpId::new(NodeId(90), 0, sequence);
    let message = OpId::new(NodeId(91), 0, sequence.saturating_add(1));
    let raw = serde_json::to_vec(&serde_json::json!({
        "type":"response_item", "payload":{"type":"message", "role":"assistant",
        "content":[{"type":"output_text", "text":text}]}
    }))
    .map_err(io::Error::other)?;
    let raw_hash = *blake3::hash(&raw).as_bytes();
    let base = Op {
        id: source,
        parents: ordinal
            .checked_sub(1)
            .filter(|value| *value > 0)
            .map_or(ParentSet::None, |previous| {
                ParentSet::One(OpId::new(NodeId(90), 0, previous << 16))
            }),
        actor: ActorId(1),
        clock: Clock::UnixMs(ordinal.saturating_mul(1000)),
        scope: ScopeRef::Session(SessionId(73)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(raw),
            raw_hash: Some(raw_hash),
        }),
    };
    let proof = ProviderEvidence {
        schema: ProviderEvidenceSchema::V1,
        source,
        raw_hash,
        fact: ProviderFact::CodexDerivation(CodexDerivationEvidence {
            thread: CodexThreadId("shared-session".into()),
            contract: CodexDerivationContract::OccurrencesV2,
            includes_thinking: false,
            outputs: vec![message],
            changes: vec![CodexLogicalChange::Upsert {
                turn: "ongoing-turn".into(),
                item: format!("message-{incarnation}"),
                incarnation: OpId::new(NodeId(90), 0, incarnation << 16),
                outputs: vec![message],
            }],
        }),
    };
    Ok(vec![
        base.clone(),
        Op {
            id: message,
            parents: ParentSet::One(source),
            tags: Tags::AGENT | Tags::MESSAGE,
            kind: OpKind::Message(MessageOp {
                content: Payload::Inline(text.as_bytes().to_vec()),
                content_type: Payload::Empty,
            }),
            ..base.clone()
        },
        Op {
            id: OpId::new(NodeId(92), 0, sequence.saturating_add(2)),
            parents: ParentSet::One(source),
            tags: Tags::META | Tags::IMPORT,
            kind: OpKind::Note(NoteOp {
                relationship: NoteRelationship::ProviderEvidence,
                target_ids: Vec::new(),
                content: Payload::Inline(serde_json::to_vec(&proof).map_err(io::Error::other)?),
            }),
            ..base
        },
    ])
}

pub(super) fn append(chain: &Path, ops: &[Op]) -> io::Result<()> {
    let mut store = editchain_store::SegmentStore::open(chain)?;
    let mut page = editchain_store::format::Page::new(0);
    for op in ops {
        page.add_record(
            0,
            editchain_store::format::encode_op(op).map_err(io::Error::other)?,
        );
    }
    store.append_page(&page)
}
