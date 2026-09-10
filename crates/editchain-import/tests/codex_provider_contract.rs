//! Provider-contract integration test for the Codex semantic/topology slice.
//!
//! Uses the *real* Codex bridge schema values — `activityKind` values emitted
//! by `tools/codex-session-exporter` from the Codex protocol's
//! `SubAgentActivityKind` (`started`/`interacted`/`interrupted`), the real
//! copied-subagent `session_meta` shape where `parentThreadId == forkedFromId`
//! with explicit `agentPath` provenance, and the additive `agentsStates`
//! per-child status map on `collabToolCall` items.
//!
//! Pins the corrected provider contract:
//! - `started` renders as readable spawn prose; `interacted`/`interrupted`
//!   render truthfully (never invented completion prose);
//! - `started`/`interacted`/`interrupted` never fabricate a `ReconnectsTo`
//!   edge — the real completion signal is an explicit per-child status of
//!   `completed` in `collabToolCall.agentsStates` (or legacy pre-R2
//!   `collaboration.list_agents` results);
//! - overall collab tool completion / `CloseAgent` / `SendInput` is never a
//!   completion signal, and old bridge payloads without `agentsStates` remain
//!   deserializable;
//! - a `SpawnedBy` edge targets one exact physical current `spawnAgent` or
//!   older `started` occurrence;
//! - copied `forkedFromId` metadata is retained as an execution fact and never
//!   converted into timestamp-selected `ForkOf` geometry;
//! - persisted source and lifecycle evidence is session-scoped.
#![cfg(unix)]
#![expect(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::needless_pass_by_value,
    clippy::unwrap_used,
    reason = "test helpers index/panic/cast/unwrap on known-length fixture vectors"
)]

use blake3 as _;
use editchain_codec as _;
use editchain_core as _;
use editchain_project as _;
use editchain_store as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;
use time as _;
use tokio as _;

#[expect(
    dead_code,
    reason = "shared test harness module also used by sibling integration test binaries"
)]
mod common;

use editchain_core::op::{NoteRelationship, OpKind};
use editchain_core::payload::Payload;
use editchain_core::provider::{CodexLifecycleEvent, CodexSpawnSignal, ProviderFact};
use editchain_core::scope::ScopeRef;
use editchain_project::HistoryProjection;

use editchain_import::codex::HelperCommand;
use editchain_import::cursor::canonical_source_key;
use editchain_import::ids::{derive_keyed_source_stream, derive_session_id, SourcePosition};

use common::*;

fn helper_in(script: &std::path::Path) -> HelperCommand {
    sh_helper(script, &[])
}

fn source_stream(dir: &tempfile::TempDir, name: &str) -> editchain_import::SourceStream {
    let path = dir.path().join(name);
    let key = canonical_source_key("codex", dir.path(), &path).unwrap();
    derive_keyed_source_stream(&key, 0)
}

fn session_meta_line(thread: &str, session_id: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-08-26T12:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"{session_id}\",\"id\":\"{thread}\",\"timestamp\":\"t\",\"cwd\":\"/tmp\"}}}}"
    )
}

fn event_line(token: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-08-26T12:00:01.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"token\":\"{token}\",\"session_id\":\"parent-session\"}}}}"
    )
}

/// Serialize one `editchain-v1` line record built from typed values.
fn line_record(
    ordinal: u64,
    changed_items: Vec<serde_json::Value>,
    session_meta: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": "editchain-v1",
        "recordType": "line",
        "sourcePath": "x",
        "sourceOrdinal": ordinal,
        "decode": {"status": "ok"},
        "projection": {
            "changedItems": changed_items,
            "changedTurns": [],
            "removedTurnIds": [],
            "sessionMeta": session_meta,
        },
    })
}

/// Newline-join projection records into a helper stdout buffer.
fn projection_bytes(records: &[serde_json::Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        out.extend_from_slice(&serde_json::to_vec(record).unwrap());
        out.push(b'\n');
    }
    out
}

fn started_item(agent_thread: &str, agent_path: &str, id: &str) -> serde_json::Value {
    serde_json::json!({
        "turnId": "turn-1",
        "item": {
            "kind": "subAgentActivity",
            "id": id,
            "activityKind": "started",
            "agentThreadId": agent_thread,
            "agentPath": agent_path,
        }
    })
}

fn interacted_item(agent_thread: &str, id: &str) -> serde_json::Value {
    serde_json::json!({
        "turnId": "turn-1",
        "item": {
            "kind": "subAgentActivity",
            "id": id,
            "activityKind": "interacted",
            "agentThreadId": agent_thread,
        }
    })
}

fn write_parent_and_sub(
    dir: &tempfile::TempDir,
    parent_items: &[serde_json::Value],
) -> std::path::PathBuf {
    // Physical parent lines: session_meta plus one line per projected item
    // (started + interacted events). Every non-blank line needs a line record,
    // so ordinals 2..=1+item_count map to the item lines.
    let mut parent_lines = vec![session_meta_line("parent-1", "s")];
    for index in 0..parent_items.len() {
        parent_lines.push(event_line(&format!("P_ITEM_{index}")));
    }
    write_rollout(dir.path(), "rollout-parent.jsonl", &parent_lines);
    write_rollout(
        dir.path(),
        "rollout-sub.jsonl",
        &[session_meta_line("sub-1", "s"), event_line("SUB_WORK")],
    );
    let mut parent_records = vec![line_record(
        1,
        Vec::new(),
        Some(serde_json::json!({"sessionId": "s", "threadId": "parent-1"})),
    )];
    for (index, item) in parent_items.iter().enumerate() {
        parent_records.push(line_record(index as u64 + 2, vec![item.clone()], None));
    }
    let parent_projection = projection_bytes(&parent_records);
    let sub_projection = projection_bytes(&[
        // Real copied-subagent metadata: parentThreadId == forkedFromId, with
        // explicit agentPath provenance.
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({
                "sessionId": "s",
                "threadId": "sub-1",
                "parentThreadId": "parent-1",
                "forkedFromId": "parent-1",
                "agentPath": "/root/sub",
            })),
        ),
        line_record(
            2,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {"kind": "agentMessage", "id": "item-2", "text": "sub work"},
            })],
            None,
        ),
    ]);
    let helper = write_dispatching_helper(
        dir.path(),
        "dispatch-helper.sh",
        &[
            ("rollout-parent.jsonl", &parent_projection),
            ("rollout-sub.jsonl", &sub_projection),
        ],
    );
    helper
}

fn sub_stream(dir: &tempfile::TempDir) -> editchain_core::OpId {
    let sub = source_stream(dir, "rollout-sub.jsonl");
    sub.op_from_position(SourcePosition::raw(1)).unwrap()
}

#[test]
fn real_subagent_activity_schema_pins_link_geometry_and_summary() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_parent_and_sub(
        &dir,
        &[
            started_item("sub-1", "/root/sub", "spawn-1"),
            interacted_item("sub-1", "talk-1"),
        ],
    );
    let harness = import(dir.path(), &helper_in(&helper));
    let projection = HistoryProjection::from_ops(harness.ops.ops.clone());

    // Real activity kinds render truthfully: `started` reads as readable spawn
    // prose, `interacted` renders verbatim — never invented completion prose.
    let _started = harness
        .ops
        .ops
        .iter()
        .find(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.content == Payload::Inline(b"spawned subagent sub-1 (path /root/sub)".to_vec()))
        })
        .expect("started marker note with readable spawn prose");
    assert!(
        harness.ops.ops.iter().any(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.content == Payload::Inline(b"subagent sub-1: interacted".to_vec()))
        }),
        "interacted renders truthfully"
    );
    assert!(
        !harness.ops.ops.iter().any(|o| {
            matches!(&o.kind, OpKind::Note(n) if n.content == Payload::Inline(b"subagent sub-1 completed".to_vec()))
        }),
        "real activity kinds never fabricate completion prose"
    );

    assert!(
        relationship_edges(&projection, NoteRelationship::ReconnectsTo).is_empty(),
        "real SubAgentActivity kinds must not fabricate a ReconnectsTo edge"
    );
    let parent = source_stream(&dir, "rollout-parent.jsonl");
    assert_eq!(
        relationship_edges(&projection, NoteRelationship::SpawnedBy),
        [(
            sub_stream(&dir),
            parent.op_from_position(SourcePosition::raw(2)).unwrap()
        )]
        .into()
    );
    let facts = provider_facts(&harness.ops.ops);
    assert!(facts.iter().any(|(op, evidence)| matches!(&evidence.fact,
        ProviderFact::CodexSource(meta) if meta.first == sub_stream(&dir)
            && meta.forked_from.as_ref().is_some_and(|thread| thread.0 == "parent-1")
            && op.scope == ScopeRef::Session(derive_session_id("sub-1")))));
    assert!(facts.iter().any(|(op, evidence)| matches!(&evidence.fact,
        ProviderFact::CodexLifecycle(meta)
            if matches!(meta.event, CodexLifecycleEvent::Spawn { signal: CodexSpawnSignal::SubagentActivity, .. })
                && op.scope == ScopeRef::Session(derive_session_id("parent-1")))));
    assert!(
        relationship_edges(&projection, NoteRelationship::ForkOf).is_empty(),
        "forkedFromId must not manufacture ForkOf"
    );
}

#[test]
fn structured_agent_states_drive_reconnect_but_old_payloads_do_not() {
    let dir = tempfile::tempdir().unwrap();
    // Two collab calls: call-1 carries the realistic structured agentsStates
    // (per-child status + message) marking sub-1 completed; call-2 is an old
    // bridge payload without agentsStates whose overall tool status is
    // completed and tool is CloseAgent — neither may reconnect the child.
    let helper = write_parent_and_sub(
        &dir,
        &[
            started_item("sub-1", "/root/sub", "spawn-1"),
            serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "collabToolCall",
                    "id": "call-1",
                    "tool": "wait",
                    "status": "completed",
                    "senderThreadId": "parent-1",
                    "receiverThreadIds": ["sub-1"],
                    "agentsStates": {"sub-1": {"status": "completed", "message": "done"}},
                }
            }),
            serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "collabToolCall",
                    "id": "call-2",
                    "tool": "closeAgent",
                    "status": "completed",
                    "senderThreadId": "parent-1",
                    "receiverThreadIds": ["sub-1"],
                }
            }),
        ],
    );
    let harness = import(dir.path(), &helper_in(&helper));
    let projection = HistoryProjection::from_ops(harness.ops.ops.clone());

    let completion = source_stream(&dir, "rollout-parent.jsonl")
        .op_from_position(SourcePosition::raw(3))
        .unwrap();
    let terminal = source_stream(&dir, "rollout-sub.jsonl")
        .op_from_position(SourcePosition::raw(2))
        .unwrap();
    assert_eq!(
        relationship_edges(&projection, NoteRelationship::ReconnectsTo),
        [(completion, terminal)].into(),
        "only the explicit child completion reconnects at its exact physical occurrence"
    );
    assert!(provider_facts(&harness.ops.ops)
        .iter()
        .any(|(op, evidence)| matches!(&evidence.fact,
        ProviderFact::CodexLifecycle(meta) if evidence.source == completion
            && !matches!(meta.event, CodexLifecycleEvent::Spawn { .. })
            && op.scope == ScopeRef::Session(derive_session_id("parent-1")))));
}

#[test]
fn legacy_list_agents_completion_maps_agent_path_to_started_marker() {
    let dir = tempfile::tempdir().unwrap();
    let completed = serde_json::to_string(&serde_json::json!({
        "agents": [
            {"agent_name": "/root/sub", "agent_status": {"completed": "sub finished"}},
            {"agent_name": "/root/other", "agent_status": "running"},
        ]
    }))
    .unwrap();
    let helper = write_parent_and_sub(
        &dir,
        &[
            started_item("sub-1", "/root/sub", "spawn-1"),
            serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "toolCall",
                    "id": "call-list",
                    "tool": "list_agents",
                    "namespace": "collaboration",
                    "status": "completed",
                    "arguments": {},
                    "result": completed,
                }
            }),
        ],
    );
    let harness = import(dir.path(), &helper_in(&helper));
    let projection = HistoryProjection::from_ops(harness.ops.ops.clone());

    let completion = source_stream(&dir, "rollout-parent.jsonl")
        .op_from_position(SourcePosition::raw(3))
        .unwrap();
    let terminal = source_stream(&dir, "rollout-sub.jsonl")
        .op_from_position(SourcePosition::raw(2))
        .unwrap();
    assert_eq!(
        relationship_edges(&projection, NoteRelationship::ReconnectsTo),
        [(completion, terminal)].into(),
        "only the explicit child completion reconnects at its exact physical occurrence"
    );
    assert!(provider_facts(&harness.ops.ops)
        .iter()
        .any(|(op, evidence)| matches!(&evidence.fact,
        ProviderFact::CodexLifecycle(meta) if evidence.source == completion
            && !matches!(meta.event, CodexLifecycleEvent::Spawn { .. })
            && op.scope == ScopeRef::Session(derive_session_id("parent-1")))));
}

#[test]
fn legacy_list_agents_missing_or_ambiguous_evidence_means_no_edge() {
    // Missing evidence: the completed agent_name does not match any started
    // marker's agentPath in the parent thread → no edge.
    let dir = tempfile::tempdir().unwrap();
    let completed_unknown = serde_json::to_string(&serde_json::json!({
        "agents": [{"agent_name": "/root/unknown", "agent_status": {"completed": "?"}}]
    }))
    .unwrap();
    let helper = write_parent_and_sub(
        &dir,
        &[
            started_item("sub-1", "/root/sub", "spawn-1"),
            serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "toolCall",
                    "id": "call-list",
                    "tool": "list_agents",
                    "namespace": "collaboration",
                    "status": "completed",
                    "arguments": {},
                    "result": completed_unknown,
                }
            }),
        ],
    );
    let harness = import(dir.path(), &helper_in(&helper));
    let projection = HistoryProjection::from_ops(harness.ops.ops.clone());
    assert!(
        relationship_edges(&projection, NoteRelationship::ReconnectsTo).is_empty(),
        "a completed agent_name with no matching started marker must not edge"
    );

    // Ambiguous evidence: two started markers share the same agentPath, so the
    // completed agent_name cannot be resolved to one child → no edge.
    let dir2 = tempfile::tempdir().unwrap();
    let completed_json = serde_json::to_string(&serde_json::json!({
        "agents": [{"agent_name": "/root/sub", "agent_status": {"completed": "a"}}]
    }))
    .unwrap();
    write_rollout(
        dir2.path(),
        "rollout-parent.jsonl",
        &[
            session_meta_line("parent-1", "s"),
            event_line("P_STARTED"),
            event_line("P_LIST"),
        ],
    );
    write_rollout(
        dir2.path(),
        "rollout-sub.jsonl",
        &[session_meta_line("sub-1", "s"), event_line("SUB_WORK")],
    );
    let parent_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({"threadId": "parent-1"})),
        ),
        line_record(
            2,
            vec![
                started_item("sub-1", "/root/sub", "spawn-1"),
                started_item("sub-2", "/root/sub", "spawn-2"),
            ],
            None,
        ),
        line_record(
            3,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {
                    "kind": "toolCall",
                    "id": "call-list",
                    "tool": "list_agents",
                    "namespace": "collaboration",
                    "status": "completed",
                    "arguments": {},
                    "result": completed_json,
                }
            })],
            None,
        ),
    ]);
    let sub_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(serde_json::json!({"threadId": "sub-1", "parentThreadId": "parent-1"})),
        ),
        line_record(2, Vec::new(), None),
    ]);
    let helper2 = write_dispatching_helper(
        dir2.path(),
        "dispatch-helper.sh",
        &[
            ("rollout-parent.jsonl", &parent_projection),
            ("rollout-sub.jsonl", &sub_projection),
        ],
    );
    let harness2 = import(dir2.path(), &helper_in(&helper2));
    let projection2 = HistoryProjection::from_ops(harness2.ops.ops);
    assert!(
        relationship_edges(&projection2, NoteRelationship::ReconnectsTo).is_empty(),
        "ambiguous agentPath evidence must not fabricate an edge"
    );
}

#[test]
fn legacy_activation_survives_duplicate_current_representation_and_replay() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_parent_and_sub(
        &dir,
        &[
            started_item("sub-1", "/root/sub", "spawn-1"),
            serde_json::json!({"turnId": "turn-1", "item": {
                "kind": "collabToolCall", "id": "current-spawn", "tool": "spawnAgent",
                "senderThreadId": "parent-1", "receiverThreadIds": ["sub-1", "sub-1", "", null]
            }}),
        ],
    );
    let first = import(dir.path(), &helper_in(&helper));
    let replay = import(dir.path(), &helper_in(&helper));
    assert_eq!(
        first.ops.ops, replay.ops.ops,
        "fact IDs and bytes survive replay"
    );
    let activation = source_stream(&dir, "rollout-parent.jsonl")
        .op_from_position(SourcePosition::raw(2))
        .unwrap();
    let facts = provider_facts(&first.ops.ops);
    assert_eq!(
        facts
            .iter()
            .filter(
                |(_, evidence)| matches!(&evidence.fact, ProviderFact::CodexLifecycle(meta)
        if matches!(meta.event, CodexLifecycleEvent::Spawn { .. }))
            )
            .count(),
        2,
        "both provider representations remain durable; duplicate receivers add no evidence"
    );
    let projection = HistoryProjection::from_ops(first.ops.ops);
    assert_eq!(
        relationship_edges(&projection, NoteRelationship::SpawnedBy),
        [(sub_stream(&dir), activation)].into()
    );
}

#[test]
fn duplicate_child_sources_leave_spawn_and_completion_unresolved() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_parent_and_sub(
        &dir,
        &[
            started_item("sub-1", "/root/sub", "spawn-1"),
            serde_json::json!({"turnId": "turn-1", "item": {
                "kind": "collabToolCall", "id": "wait", "tool": "wait",
                "agentsStates": {"sub-1": {"status": "completed"}}
            }}),
        ],
    );
    let first = import(dir.path(), &helper_in(&helper));
    let projection = HistoryProjection::from_ops(first.ops.ops);
    assert_eq!(
        relationship_edges(&projection, NoteRelationship::ReconnectsTo).len(),
        1
    );
    let other = dir.path().join("another-source");
    std::fs::create_dir(&other).unwrap();
    let _: u64 = std::fs::copy(
        dir.path().join("rollout-sub.jsonl"),
        other.join("rollout-sub.jsonl"),
    )
    .unwrap();
    let ambiguous = import(dir.path(), &helper_in(&helper));
    let projection = HistoryProjection::from_ops(ambiguous.ops.ops);
    assert!(relationship_edges(&projection, NoteRelationship::SpawnedBy).is_empty());
    assert!(relationship_edges(&projection, NoteRelationship::ReconnectsTo).is_empty());
}
