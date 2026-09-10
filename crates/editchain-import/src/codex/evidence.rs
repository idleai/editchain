//! Capture provider evidence without requiring another source to be present.

use std::collections::BTreeSet;

use editchain_core::provider::{
    CodexLifecycleEvent, CodexLifecycleEvidence, CodexSourceEvidence, CodexSpawnSignal,
    CodexThreadId, ProviderEvidence, ProviderEvidenceSchema, ProviderFact,
};
use editchain_core::{
    ActorId, Clock, NoteOp, NoteRelationship, Op, OpKind, ParentSet, Payload, ScopeRef,
};

use serde_json::Value;

use super::normalize::completed_agent_paths_from_tool;
use super::projection::{FinalItem, Projection, ProjectionKind};
use crate::ids::{derive_external_entity_id, derive_session_id, SourcePosition, SourceStream};
use crate::source_read::SourceReadPlan;
use crate::ImportError;

/// First checkpoint with source extents and occurrence-bound lifecycle facts.
pub(super) const CODEX_PROVIDER_EVIDENCE_VERSION: u32 = 6;

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
        for event in lifecycle_events(item, stream, thread, plan.checkpoint().ops_emitted)? {
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
    last_complete_ordinal: u64,
) -> Result<Vec<CodexLifecycleEvent>, ImportError> {
    let mut events = Vec::new();
    if item.first_seen > 0 && item.first_seen <= last_complete_ordinal {
        capture_activation(item, stream, thread, &mut events)?;
    }
    if item.kind != ProjectionKind::Tool
        || item.last_seen == 0
        || item.last_seen > last_complete_ordinal
    {
        return Ok(events);
    }
    if let Some(states) = item.payload.get("agentsStates").and_then(Value::as_object) {
        for (child, state) in states {
            if state
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status.eq_ignore_ascii_case("completed"))
            {
                events.push(CodexLifecycleEvent::Completed {
                    child: CodexThreadId(child.clone()),
                });
            }
        }
    }
    if item.payload.get("tool").and_then(Value::as_str) == Some("list_agents") {
        events.extend(
            completed_agent_paths_from_tool(&item.payload)
                .into_iter()
                .map(|agent_path| CodexLifecycleEvent::LegacyCompleted { agent_path }),
        );
    }
    Ok(events)
}

/// Preserve both exact activation shapes at their first physical occurrence.
fn capture_activation(
    item: &FinalItem,
    stream: &SourceStream,
    thread: &str,
    events: &mut Vec<CodexLifecycleEvent>,
) -> Result<(), ImportError> {
    match item.kind {
        ProjectionKind::Note => {
            if !item
                .payload
                .get("activityKind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("started"))
            {
                return Ok(());
            }
            if let Some(child) = item
                .payload
                .get("agentThreadId")
                .and_then(Value::as_str)
                .filter(|child| !child.is_empty())
            {
                events.push(CodexLifecycleEvent::Spawn {
                    activation: stream.op_from_position(SourcePosition::raw(item.first_seen))?,
                    child: CodexThreadId(child.to_owned()),
                    agent_path: item
                        .payload
                        .get("agentPath")
                        .and_then(Value::as_str)
                        .filter(|path| !path.is_empty())
                        .map(ToString::to_string),
                    signal: CodexSpawnSignal::SubagentActivity,
                });
            }
        }
        ProjectionKind::Tool => capture_collab_activation(item, stream, thread, events)?,
        ProjectionKind::Message
        | ProjectionKind::Reflection
        | ProjectionKind::Command
        | ProjectionKind::File
        | ProjectionKind::Unknown => {}
    }
    Ok(())
}

fn capture_collab_activation(
    item: &FinalItem,
    stream: &SourceStream,
    thread: &str,
    events: &mut Vec<CodexLifecycleEvent>,
) -> Result<(), ImportError> {
    if item.payload.get("tool").and_then(Value::as_str) != Some("spawnAgent")
        || item.payload.get("senderThreadId").and_then(Value::as_str) != Some(thread)
    {
        return Ok(());
    }
    let Some(receivers) = item
        .payload
        .get("receiverThreadIds")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let activation = stream.op_from_position(SourcePosition::raw(item.first_seen))?;
    let children: BTreeSet<&str> = receivers
        .iter()
        .filter_map(Value::as_str)
        .filter(|child| !child.is_empty())
        .collect();
    events.extend(
        children
            .into_iter()
            .map(|child| CodexLifecycleEvent::Spawn {
                activation,
                child: CodexThreadId(child.to_owned()),
                agent_path: None,
                signal: CodexSpawnSignal::CollabTool,
            }),
    );
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event_json(
        item: &FinalItem,
        stream: &SourceStream,
        last: u64,
    ) -> Result<String, ImportError> {
        Ok(serde_json::to_string(&lifecycle_events(
            item, stream, "parent", last,
        )?)?)
    }

    #[test]
    fn lifecycle_capture_keeps_v1_event_bytes_and_exact_physical_boundaries() {
        let stream = SourceStream::new(editchain_core::NodeId(1), 0);
        let mut item = FinalItem {
            item_id: "call".into(),
            turn_id: "turn".into(),
            first_seen: 2,
            last_seen: 3,
            kind: ProjectionKind::Tool,
            actor: "agent".into(),
            payload: serde_json::json!({
                "tool": "spawnAgent", "senderThreadId": "parent",
                "receiverThreadIds": ["b", "a", "b", "", null],
                "agentsStates": {"c": {"status": "COMPLETED"}, "b": {"status": "pendingInit"}}
            }),
        };
        assert!(lifecycle_events(&item, &stream, "parent", 1).is_ok_and(|events| events.is_empty()));
        assert_eq!(
            event_json(&item, &stream, 3)
                .as_deref()
                .map_err(ToString::to_string),
            Ok(concat!(
                r#"[{"Spawn":{"activation":{"node":1,"boot":0,"seq":131072},"child":"a","agent_path":null,"signal":"CollabTool"}},"#,
                r#"{"Spawn":{"activation":{"node":1,"boot":0,"seq":131072},"child":"b","agent_path":null,"signal":"CollabTool"}},"#,
                r#"{"Completed":{"child":"c"}}]"#,
            ))
        );
        assert_eq!(
            lifecycle_events(&item, &stream, "parent", 2)
                .map(|events| events.len())
                .map_err(|error| error.to_string()),
            Ok(2),
            "a partial completion adds no state observation"
        );
        assert_eq!(
            lifecycle_events(&item, &stream, "other", 3).map_err(|error| error.to_string()),
            Ok(vec![CodexLifecycleEvent::Completed {
                child: CodexThreadId("c".into())
            }]),
            "a foreign sender adds no activation"
        );
        item.payload = serde_json::json!({
            "tool": "list_agents",
            "agentsStates": {"c": {"status": "completed"}},
            "result": r#"{"agents":[{"agent_name":"/root/a","agent_status":{"completed":"done"}}]}"#
        });
        assert_eq!(
            event_json(&item, &stream, 3)
                .as_deref()
                .map_err(ToString::to_string),
            Ok(r#"[{"Completed":{"child":"c"}},{"LegacyCompleted":{"agent_path":"/root/a"}}]"#)
        );
    }

    #[test]
    fn dedicated_activity_captures_only_named_started_children() {
        let stream = SourceStream::new(editchain_core::NodeId(1), 0);
        let mut item = FinalItem {
            item_id: "spawn".into(),
            turn_id: "turn".into(),
            first_seen: 2,
            last_seen: 2,
            kind: ProjectionKind::Note,
            actor: "agent".into(),
            payload: serde_json::json!({
                "activityKind": "STARTED", "agentThreadId": "a", "agentPath": "/root/a"
            }),
        };
        assert_eq!(
            event_json(&item, &stream, 2)
                .as_deref()
                .map_err(ToString::to_string),
            Ok(
                r#"[{"Spawn":{"activation":{"node":1,"boot":0,"seq":131072},"child":"a","agent_path":"/root/a","signal":"SubagentActivity"}}]"#
            )
        );
        for payload in [
            serde_json::json!({"activityKind": "started", "agentThreadId": ""}),
            serde_json::json!({"activityKind": "interacted", "agentThreadId": "a"}),
            serde_json::json!({"activityKind": "interrupted", "agentThreadId": "a"}),
        ] {
            item.payload = payload;
            assert!(
                lifecycle_events(&item, &stream, "parent", 2).is_ok_and(|events| events.is_empty())
            );
        }
    }
}
