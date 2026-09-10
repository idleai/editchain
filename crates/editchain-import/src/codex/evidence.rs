//! Capture provider evidence without requiring another source to be present.

use editchain_core::provider::{
    CodexLifecycleEvent, CodexLifecycleEvidence, CodexSourceEvidence, CodexSpawnSignal,
    CodexThreadId, ProviderEvidence, ProviderEvidenceSchema, ProviderFact,
};
use editchain_core::{
    ActorId, Clock, NoteOp, NoteRelationship, Op, OpKind, ParentSet, Payload, ScopeRef,
};

use super::import::collect_topology_evidence;
use super::link::{ThreadTopology, SPAWN_SIGNAL_SUBAGENT_ACTIVITY};
use super::projection::{FinalItem, Projection};
use crate::ids::{derive_external_entity_id, derive_session_id, SourcePosition, SourceStream};
use crate::source_read::SourceReadPlan;
use crate::ImportError;

pub(super) fn source_evidence_ops(
    projection: &Projection,
    plan: &SourceReadPlan,
    stream: &SourceStream,
    thread: &str,
    replay: bool,
) -> Result<Vec<Op>, ImportError> {
    let historical = (replay && plan.start_seq() > 0)
        .then(|| plan.all_lines())
        .transpose()?;
    let records = historical.as_deref().unwrap_or_else(|| plan.lines());
    let Some(last_record) = records.last() else {
        return Ok(Vec::new());
    };
    let start = if replay { 0 } else { plan.start_seq() };
    let last = stream.op_from_position(SourcePosition::raw(plan.checkpoint().ops_emitted))?;
    let first = stream.op_from_position(SourcePosition::raw(1))?;
    let mut notes = Vec::new();
    for item in &projection.item_occurrences {
        if item.last_seen <= start || item.last_seen > plan.checkpoint().ops_emitted {
            continue;
        }
        let index = item.last_seen.saturating_sub(start).saturating_sub(1);
        let raw = usize::try_from(index)
            .ok()
            .and_then(|index| records.get(index))
            .ok_or_else(|| ImportError::OpSink("lifecycle source occurrence missing".into()))?;
        let source = stream.op_from_position(SourcePosition::raw(item.last_seen))?;
        for event in lifecycle_events(item, stream, thread, last)? {
            notes.push(evidence_note(
                thread,
                &ProviderEvidence {
                    schema: ProviderEvidenceSchema::V1,
                    source,
                    raw_hash: raw.hash,
                    fact: ProviderFact::CodexLifecycle(CodexLifecycleEvidence {
                        thread: CodexThreadId(thread.to_owned()),
                        item_id: item.item_id.clone(),
                        turn_id: item.turn_id.clone(),
                        event,
                    }),
                },
            )?);
        }
    }
    let meta = projection.session_meta.as_ref();
    notes.push(evidence_note(
        thread,
        &ProviderEvidence {
            schema: ProviderEvidenceSchema::V1,
            source: last,
            raw_hash: last_record.hash,
            fact: ProviderFact::CodexSource(Box::new(CodexSourceEvidence {
                thread: CodexThreadId(thread.to_owned()),
                parent: meta
                    .and_then(|meta| meta.parent_thread_id.clone())
                    .map(CodexThreadId),
                forked_from: meta
                    .and_then(|meta| meta.forked_from_id.clone())
                    .map(CodexThreadId),
                agent_path: meta.and_then(|meta| meta.agent_path.clone()),
                first,
                last,
                prefix_hash: plan.checkpoint().content_hash,
            })),
        },
    )?);
    Ok(notes)
}

fn lifecycle_events(
    item: &FinalItem,
    stream: &SourceStream,
    thread: &str,
    last: editchain_core::OpId,
) -> Result<Vec<CodexLifecycleEvent>, ImportError> {
    let mut topology = ThreadTopology {
        thread_id: thread.to_owned(),
        last_raw: Some(last),
        ..ThreadTopology::default()
    };
    collect_topology_evidence(std::slice::from_ref(item), stream, &mut topology)?;
    let mut events = Vec::new();
    for marker in topology.markers.into_iter().filter(|marker| marker.started) {
        events.push(CodexLifecycleEvent::Spawn {
            activation: marker.op_id,
            child: CodexThreadId(marker.agent_thread_id),
            agent_path: marker.agent_path,
            signal: if marker.signal == SPAWN_SIGNAL_SUBAGENT_ACTIVITY {
                CodexSpawnSignal::SubagentActivity
            } else {
                CodexSpawnSignal::CollabTool
            },
        });
    }
    for completed in topology.completions {
        events.push(CodexLifecycleEvent::Completed {
            child: CodexThreadId(completed.agent_thread_id),
        });
    }
    for completed in topology.legacy_completions {
        events.push(CodexLifecycleEvent::LegacyCompleted {
            agent_path: completed.agent_path,
        });
    }
    Ok(events)
}

pub(super) fn evidence_note(thread: &str, evidence: &ProviderEvidence) -> Result<Op, ImportError> {
    let content = serde_json::to_string(&evidence)?;
    let id = derive_external_entity_id("codex:provider-evidence:v1", &content);
    Ok(Op {
        id,
        parents: ParentSet::One(evidence.source),
        actor: ActorId(0),
        clock: Clock::None,
        scope: ScopeRef::Session(derive_session_id(thread)),
        tags: editchain_core::Tags::META | editchain_core::Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::ProviderEvidence,
            content: Payload::Inline(content.into_bytes()),
        }),
    })
}
