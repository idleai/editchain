//! Exact Claude Code provider-topology facts.
//!
//! Claude transcripts preserve two different orders: physical JSONL source order
//! and the provider event graph named by `uuid` / `parentUuid`. This module emits
//! immutable, provider-neutral relationship notes for the latter directly from
//! the parsed envelope, before raw payloads can spill to blob storage. It never
//! compares content, timestamps, session lengths, or discovery order.

use editchain_core::{
    clock::Clock,
    op::{NoteOp, NoteRelationship, OpKind},
    parents::ParentSet,
    payload::Payload,
    scope::ScopeRef,
    tags::Tags,
    ActorId, Op, OpId,
};

use super::envelope::{CcContentBlock, CcEnvelope};
use crate::{
    error::ImportError,
    ids::{derive_external_entity_id, derive_session_id, SourcePosition, SourceStream},
};

/// Cursor checkpoint for the exact event-graph normalizer.
///
/// Version 1 is the historical normalized-content importer. Version 2 adds the
/// provider-event, logical-parent, tool-correlation, and spawn facts in this
/// module and retires inferred Claude `ForkOf` generation.
pub const CLAUDE_NORMALIZATION_VERSION: u32 = 2;

/// Resolver identifier retained in every relation fact's evidence payload.
pub const CLAUDE_TOPOLOGY_RESOLVER: &str = "claude-topology-v2";

const EVENT_NAMESPACE: &str = "claude-code:event";
const TOOL_NAMESPACE: &str = "claude-code:tool-call";
const TOPOLOGY_NOTE_BASE: u16 = 0xC000;
const SPAWN_NOTE_DISC: u16 = 0xFFD0;

/// Return the stable graph handle for a Claude provider event UUID.
#[must_use]
pub fn event_entity_id(uuid: &str) -> OpId {
    derive_external_entity_id(EVENT_NAMESPACE, uuid)
}

/// Return the stable graph handle for a Claude tool-use identifier.
#[must_use]
pub fn tool_entity_id(tool_use_id: &str) -> OpId {
    derive_external_entity_id(TOOL_NAMESPACE, tool_use_id)
}

/// Emit exact topology/correlation facts carried by one parsed source record.
///
/// The note parent is always the physical raw occurrence. `OccurrenceOf` maps
/// copied provider events onto one stable entity; `ProviderParent` and
/// `LogicalParent` retain the two provider fields separately; `Contains` maps
/// embedded tool calls to their exact IDs; and `ToolResultOf` correlates results
/// without relying on source adjacency or tool names.
///
/// # Errors
///
/// Returns an error if a source position overflows or evidence JSON cannot be
/// encoded.
pub fn relation_facts_for_envelope(
    envelope: &CcEnvelope,
    stream: &SourceStream,
    source_ordinal: u64,
    fallback_session_id: &str,
) -> Result<Vec<Op>, ImportError> {
    let raw_id = stream.op_from_position(SourcePosition::raw(source_ordinal))?;
    let session = if envelope.session_id.is_empty() {
        fallback_session_id
    } else {
        envelope.session_id.as_str()
    };
    let scope = ScopeRef::Session(derive_session_id(session));
    let mut facts = Vec::new();

    if !envelope.uuid.is_empty() {
        push_fact(
            &mut facts,
            stream,
            source_ordinal,
            FactSpec {
                anchor: raw_id,
                target: event_entity_id(&envelope.uuid),
                scope,
                relationship: NoteRelationship::OccurrenceOf,
                entity_kind: "event",
                external_id: &envelope.uuid,
            },
        )?;
    }
    if !envelope.parent_uuid.is_empty() {
        push_fact(
            &mut facts,
            stream,
            source_ordinal,
            FactSpec {
                anchor: raw_id,
                target: event_entity_id(&envelope.parent_uuid),
                scope,
                relationship: NoteRelationship::ProviderParent,
                entity_kind: "event",
                external_id: &envelope.parent_uuid,
            },
        )?;
    }
    if !envelope.logical_parent_uuid.is_empty() {
        push_fact(
            &mut facts,
            stream,
            source_ordinal,
            FactSpec {
                anchor: raw_id,
                target: event_entity_id(&envelope.logical_parent_uuid),
                scope,
                relationship: NoteRelationship::LogicalParent,
                entity_kind: "event",
                external_id: &envelope.logical_parent_uuid,
            },
        )?;
    }

    if let Some(message) = &envelope.message {
        for block in &message.content {
            match block {
                CcContentBlock::ToolUse { id, .. } if !id.is_empty() => push_fact(
                    &mut facts,
                    stream,
                    source_ordinal,
                    FactSpec {
                        anchor: raw_id,
                        target: tool_entity_id(id),
                        scope,
                        relationship: NoteRelationship::Contains,
                        entity_kind: "tool_call",
                        external_id: id,
                    },
                )?,
                CcContentBlock::ToolResult { tool_use_id, .. } if !tool_use_id.is_empty() => {
                    push_fact(
                        &mut facts,
                        stream,
                        source_ordinal,
                        FactSpec {
                            anchor: raw_id,
                            target: tool_entity_id(tool_use_id),
                            scope,
                            relationship: NoteRelationship::ToolResultOf,
                            entity_kind: "tool_call",
                            external_id: tool_use_id,
                        },
                    )?;
                }
                CcContentBlock::Text { .. }
                | CcContentBlock::Thinking { .. }
                | CcContentBlock::ToolUse { .. }
                | CcContentBlock::ToolResult { .. } => {}
            }
        }
    }

    Ok(facts)
}

/// Emit an exact child-execution spawn relation from the child's first raw
/// occurrence to the parent tool entity named by its discovery sidecar.
///
/// The target may be unresolved when the parent source is absent; projection
/// then keeps the child source independent rather than guessing an attachment.
///
/// # Errors
///
/// Returns an error if the note source position overflows or evidence JSON
/// cannot be encoded.
pub fn spawn_fact(
    child_first_raw: OpId,
    child_scope: ScopeRef,
    tool_use_id: &str,
) -> Result<Op, ImportError> {
    let stream = SourceStream::new(child_first_raw.node, child_first_raw.boot);
    let source_ordinal = child_first_raw.seq >> 16;
    let id = stream.op_from_position(SourcePosition::derived(source_ordinal, SPAWN_NOTE_DISC))?;
    Ok(build_fact(
        id,
        FactSpec {
            anchor: child_first_raw,
            target: tool_entity_id(tool_use_id),
            scope: child_scope,
            relationship: NoteRelationship::SpawnedBy,
            entity_kind: "tool_call",
            external_id: tool_use_id,
        },
    )?)
}

/// Complete endpoint and evidence for one exact relation fact.
#[derive(Clone, Copy)]
struct FactSpec<'a> {
    anchor: OpId,
    target: OpId,
    scope: ScopeRef,
    relationship: NoteRelationship,
    entity_kind: &'a str,
    external_id: &'a str,
}

#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "one source envelope cannot contain enough relation fields/content blocks to exhaust the reserved u16 relation lane"
)]
fn push_fact(
    facts: &mut Vec<Op>,
    stream: &SourceStream,
    source_ordinal: u64,
    spec: FactSpec<'_>,
) -> Result<(), ImportError> {
    let derived_ordinal = TOPOLOGY_NOTE_BASE + facts.len() as u16;
    let id = stream.op_from_position(SourcePosition::derived(source_ordinal, derived_ordinal))?;
    facts.push(build_fact(id, spec)?);
    Ok(())
}

fn build_fact(id: OpId, spec: FactSpec<'_>) -> Result<Op, serde_json::Error> {
    let evidence = serde_json::to_vec(&serde_json::json!({
        "confidence": "exact",
        "entityKind": spec.entity_kind,
        "externalId": spec.external_id,
        "provider": "claude-code",
        "resolver": CLAUDE_TOPOLOGY_RESOLVER,
    }))?;
    Ok(Op {
        id,
        parents: ParentSet::One(spec.anchor),
        actor: ActorId(0),
        clock: Clock::None,
        scope: spec.scope,
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![spec.target],
            relationship: spec.relationship,
            content: Payload::Inline(evidence),
        }),
    })
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::indexing_slicing,
        clippy::panic,
        clippy::wildcard_enum_match_arm,
        reason = "fixed-size topology fixtures assert directly on known operation shapes"
    )]

    use editchain_core::NodeId;

    use super::*;
    use crate::claude_code::envelope::parse_envelope;

    #[test]
    fn emits_exact_event_parent_and_tool_facts() {
        let envelope = parse_envelope(
            br#"{"type":"assistant","uuid":"event-2","parentUuid":"event-1","sessionId":"session-1","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-1","name":"Read","input":{}}]}}"#,
        )
        .unwrap();
        let stream = SourceStream::new(NodeId(7), 3);

        let facts = relation_facts_for_envelope(&envelope, &stream, 9, "fallback").unwrap();

        assert_eq!(facts.len(), 3);
        let relationships: Vec<NoteRelationship> = facts
            .iter()
            .filter_map(|op| match &op.kind {
                OpKind::Note(note) => Some(note.relationship),
                _ => None,
            })
            .collect();
        assert_eq!(
            relationships,
            vec![
                NoteRelationship::OccurrenceOf,
                NoteRelationship::ProviderParent,
                NoteRelationship::Contains,
            ]
        );
        assert_eq!(facts[0].parents, ParentSet::One(stream.op_id(9 << 16)));
        assert_eq!(
            facts[0].kind,
            OpKind::Note(NoteOp {
                target_ids: vec![event_entity_id("event-2")],
                relationship: NoteRelationship::OccurrenceOf,
                content: match &facts[0].kind {
                    OpKind::Note(note) => note.content.clone(),
                    _ => Payload::Empty,
                },
            })
        );
    }

    #[test]
    fn copied_uuid_uses_one_entity_but_distinct_occurrence_notes() {
        let envelope = parse_envelope(
            br#"{"type":"user","uuid":"shared","sessionId":"session-1","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();
        let left = SourceStream::new(NodeId(1), 0);
        let right = SourceStream::new(NodeId(2), 0);
        let a = relation_facts_for_envelope(&envelope, &left, 1, "fallback").unwrap();
        let b = relation_facts_for_envelope(&envelope, &right, 4, "fallback").unwrap();

        let target = |op: &Op| match &op.kind {
            OpKind::Note(note) => note.target_ids[0],
            _ => panic!("expected note"),
        };
        assert_eq!(target(&a[0]), target(&b[0]));
        assert_ne!(a[0].parents, b[0].parents);
        assert_ne!(a[0].id, b[0].id);
    }
}
