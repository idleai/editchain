//! Exact Claude Code provider-topology facts.
//!
//! Claude transcripts preserve two different orders: physical JSONL source order
//! and the provider event graph named by `uuid` / `parentUuid`. This module emits
//! immutable, provider-neutral relationship notes for the latter directly from
//! the parsed envelope, before raw payloads can spill to blob storage. An
//! importer-owned canonical payload fingerprint distinguishes copied
//! occurrences from genuine revisions without comparing timestamps, session
//! lengths, or discovery order.

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
/// module and retires inferred Claude `ForkOf` generation. Version 3 adds an
/// exact content fingerprint to provider-event occurrence facts so copied
/// session prefixes share one display trunk while payload revisions stay
/// distinct.
pub const CLAUDE_NORMALIZATION_VERSION: u32 = 3;

/// Resolver identifier retained in every relation fact's evidence payload.
pub const CLAUDE_TOPOLOGY_RESOLVER: &str = "claude-topology-v3";

const EVENT_NAMESPACE: &str = "claude-code:event";
const TOOL_NAMESPACE: &str = "claude-code:tool-call";
const TOPOLOGY_NOTE_BASE: u16 = 0xC000;
const OCCURRENCE_FINGERPRINT_NOTE_DISC: u16 = 0xFFC0;
const SPAWN_NOTE_DISC: u16 = 0xFFD0;
const EVENT_PAYLOAD_FINGERPRINT_DOMAIN: &[u8] = b"editchain:claude-event-payload:v1\0";

/// Top-level transport/topology fields that may legitimately change when
/// Claude copies or relocates a session while the provider event payload stays
/// unchanged. They remain preserved byte-for-byte in the raw import; only the
/// exact occurrence-equivalence fingerprint omits them.
const OCCURRENCE_ENVELOPE_FIELDS: [&str; 11] = [
    "sessionId",
    "session_id",
    "sessionKind",
    "slug",
    "cwd",
    "parentUuid",
    "logicalParentUuid",
    "leafUuid",
    "timestamp",
    "isSidechain",
    "isMeta",
];

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
/// Returns an error if a source position overflows, source JSON cannot be
/// canonicalized, or evidence JSON cannot be encoded.
pub fn relation_facts_for_envelope(
    envelope: &CcEnvelope,
    raw_bytes: &[u8],
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
    let payload_fingerprint = if envelope.uuid.is_empty() {
        None
    } else {
        event_payload_fingerprint(raw_bytes)?
    };

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
            payload_fingerprint.as_deref(),
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
            None,
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
            None,
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
                    None,
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
                        None,
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

/// Emit the version-3 payload-fingerprint supplement for a provider event that
/// already has immutable version-2 topology facts.
///
/// The fixed derived lane is disjoint from the version-2 fact range used by
/// ordinary Claude envelopes and from the spawn lane. Existing facts therefore
/// remain untouched during an in-place cursor upgrade.
///
/// # Errors
///
/// Returns an error if a source position overflows, the raw JSON cannot be
/// encoded canonically, or evidence JSON cannot be encoded.
pub fn occurrence_fingerprint_fact(
    envelope: &CcEnvelope,
    raw_bytes: &[u8],
    stream: &SourceStream,
    source_ordinal: u64,
    fallback_session_id: &str,
) -> Result<Option<Op>, ImportError> {
    if envelope.uuid.is_empty() {
        return Ok(None);
    }
    let Some(payload_fingerprint) = event_payload_fingerprint(raw_bytes)? else {
        return Ok(None);
    };
    let session = if envelope.session_id.is_empty() {
        fallback_session_id
    } else {
        envelope.session_id.as_str()
    };
    let id = stream.op_from_position(SourcePosition::derived(
        source_ordinal,
        OCCURRENCE_FINGERPRINT_NOTE_DISC,
    ))?;
    Ok(Some(build_fact(
        id,
        FactSpec {
            anchor: stream.op_from_position(SourcePosition::raw(source_ordinal))?,
            target: event_entity_id(&envelope.uuid),
            scope: ScopeRef::Session(derive_session_id(session)),
            relationship: NoteRelationship::OccurrenceOf,
            entity_kind: "event",
            external_id: &envelope.uuid,
        },
        Some(&payload_fingerprint),
    )?))
}

/// Hash the canonical provider-event payload while excluding fields that only
/// locate one physical occurrence in a copied session. The provider UUID is
/// retained in the canonical JSON and the projection additionally groups only
/// within that UUID's external-entity handle.
fn event_payload_fingerprint(raw_bytes: &[u8]) -> Result<Option<String>, serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_slice(raw_bytes)?;
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    for field in OCCURRENCE_ENVELOPE_FIELDS {
        drop(object.remove(field));
    }
    let canonical = serde_json::to_vec(&value)?;
    let mut hasher = blake3::Hasher::new();
    let _: &mut blake3::Hasher = hasher.update(EVENT_PAYLOAD_FINGERPRINT_DOMAIN);
    let _: &mut blake3::Hasher = hasher.update(&canonical);
    Ok(Some(hasher.finalize().to_hex().to_string()))
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
        None,
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
    payload_fingerprint: Option<&str>,
) -> Result<(), ImportError> {
    let derived_ordinal = TOPOLOGY_NOTE_BASE + facts.len() as u16;
    let id = stream.op_from_position(SourcePosition::derived(source_ordinal, derived_ordinal))?;
    facts.push(build_fact(id, spec, payload_fingerprint)?);
    Ok(())
}

fn build_fact(
    id: OpId,
    spec: FactSpec<'_>,
    payload_fingerprint: Option<&str>,
) -> Result<Op, serde_json::Error> {
    let mut evidence = serde_json::json!({
        "confidence": "exact",
        "entityKind": spec.entity_kind,
        "externalId": spec.external_id,
        "provider": "claude-code",
        "resolver": CLAUDE_TOPOLOGY_RESOLVER,
    });
    if let Some(fingerprint) = payload_fingerprint {
        if let Some(object) = evidence.as_object_mut() {
            drop(object.insert(
                "payloadFingerprint".to_string(),
                serde_json::Value::String(fingerprint.to_string()),
            ));
        }
    }
    let evidence = serde_json::to_vec(&evidence)?;
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

    fn payload_fingerprint(op: &Op) -> Option<String> {
        let OpKind::Note(note) = &op.kind else {
            return None;
        };
        let Payload::Inline(evidence) = &note.content else {
            return None;
        };
        serde_json::from_slice::<serde_json::Value>(evidence)
            .ok()?
            .get("payloadFingerprint")?
            .as_str()
            .map(ToOwned::to_owned)
    }

    #[test]
    fn emits_exact_event_parent_and_tool_facts() {
        let raw = br#"{"type":"assistant","uuid":"event-2","parentUuid":"event-1","sessionId":"session-1","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-1","name":"Read","input":{}}]}}"#;
        let envelope = parse_envelope(raw).unwrap();
        let stream = SourceStream::new(NodeId(7), 3);

        let facts = relation_facts_for_envelope(&envelope, raw, &stream, 9, "fallback").unwrap();

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
        assert!(payload_fingerprint(&facts[0]).is_some());
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
        let left_raw = br#"{"type":"user","uuid":"shared","sessionId":"session-1","sessionKind":"bg","slug":"one","cwd":"/one","message":{"role":"user","content":"hello"}}"#;
        let right_raw = br#"{"type":"user","uuid":"shared","sessionId":"session-2","slug":"two","cwd":"/two","message":{"role":"user","content":"hello"}}"#;
        let left_envelope = parse_envelope(left_raw).unwrap();
        let right_envelope = parse_envelope(right_raw).unwrap();
        let left = SourceStream::new(NodeId(1), 0);
        let right = SourceStream::new(NodeId(2), 0);
        let a =
            relation_facts_for_envelope(&left_envelope, left_raw, &left, 1, "fallback").unwrap();
        let b =
            relation_facts_for_envelope(&right_envelope, right_raw, &right, 4, "fallback").unwrap();

        let target = |op: &Op| match &op.kind {
            OpKind::Note(note) => note.target_ids[0],
            _ => panic!("expected note"),
        };
        assert_eq!(target(&a[0]), target(&b[0]));
        assert_eq!(payload_fingerprint(&a[0]), payload_fingerprint(&b[0]));
        assert_ne!(a[0].parents, b[0].parents);
        assert_ne!(a[0].id, b[0].id);
    }

    #[test]
    fn reused_uuid_with_revised_content_has_a_distinct_fingerprint() {
        let first_raw = br#"{"type":"user","uuid":"shared","sessionId":"session-1","message":{"role":"user","content":"first"}}"#;
        let revised_raw = br#"{"type":"user","uuid":"shared","sessionId":"session-2","message":{"role":"user","content":"revised"}}"#;
        let first_envelope = parse_envelope(first_raw).unwrap();
        let revised_envelope = parse_envelope(revised_raw).unwrap();
        let stream = SourceStream::new(NodeId(1), 0);
        let first = relation_facts_for_envelope(&first_envelope, first_raw, &stream, 1, "fallback")
            .unwrap();
        let revised =
            relation_facts_for_envelope(&revised_envelope, revised_raw, &stream, 2, "fallback")
                .unwrap();

        assert_ne!(
            payload_fingerprint(&first[0]),
            payload_fingerprint(&revised[0])
        );
    }

    #[test]
    fn version_two_upgrade_fact_uses_a_collision_free_lane() {
        let raw = br#"{"type":"user","uuid":"shared","sessionId":"session-1","message":{"role":"user","content":"hello"}}"#;
        let envelope = parse_envelope(raw).unwrap();
        let stream = SourceStream::new(NodeId(1), 0);
        let fact = occurrence_fingerprint_fact(&envelope, raw, &stream, 3, "fallback")
            .unwrap()
            .unwrap();

        assert_eq!(
            fact.id,
            stream
                .op_from_position(SourcePosition::derived(3, OCCURRENCE_FINGERPRINT_NOTE_DISC))
                .unwrap()
        );
        assert!(payload_fingerprint(&fact).is_some());
    }
}
