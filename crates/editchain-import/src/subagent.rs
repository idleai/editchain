//! Subagent branch/reconnect linking.
//!
//! Claude Code runs subagents as background sessions that branch off the
//! parent's `Agent` tool_use and (on success) reconnect when the parent's
//! `TaskOutput` tool_result reports completion. This module emits typed
//! relationship notes so the unified history graph shows those branch and
//! reconnect edges without mutating causal parents (SPEC §1.1).
//!
//! The linking is a pure function over the already-normalized op set plus the
//! subagent metadata captured during discovery. It runs as a post-pass after all
//! sessions are imported, before ops are written to the chain store.
//!
//! ## Geometry note
//!
//! Ops are displayed newest-first, so chronological order maps to *descending*
//! rows: `[TaskOutput] [sub-last] ... [sub-first] [Agent-call]`. To keep every
//! edge pointing downward (child above parent) — which the topological sort and
//! lane layout require — we emit:
//!
//! - **Branch**: a [`NoteRelationship::SubagentOf`] note whose causal parent is
//!   the subagent's first op and whose target is the parent's `Agent` tool_use.
//!   The layout reads it as a downward edge from sub-first to Agent-call.
//! - **Reconnect**: a [`NoteRelationship::ReconnectsTo`] note whose causal parent
//!   is the parent's `TaskOutput` result and whose target is the subagent's last
//!   op. The layout reads it as a downward merge edge from TaskOutput to sub-last.

use std::collections::HashMap;

use editchain_core::{
    clock::Clock,
    op::{NoteOp, NoteRelationship, OpKind, ToolOp, ToolStage},
    payload::Payload,
    scope::ScopeRef,
    tags::Tags,
    ActorId, Op, OpId, SessionId,
};

/// Metadata describing one subagent session, captured during discovery.
#[derive(Debug, Clone)]
pub struct SubagentMeta {
    /// The subagent's session id (the `agent-<id>` filename stem).
    pub subagent_session_id: String,
    /// The parent session id (the main session that spawned it).
    pub parent_session_id: String,
    /// The parent's `Agent` `tool_use` id that spawned this subagent.
    pub tool_use_id: String,
}

/// Emit subagent branch/reconnect relationship notes over an op set.
///
/// Does not mutate `ops`. Returns new [`Op`]s:
/// - a [`NoteRelationship::SubagentOf`] note per subagent whose causal parent is
///   the subagent's first op and whose target is the parent's matching `Agent`
///   `tool_use`;
/// - a [`NoteRelationship::ReconnectsTo`] note per completed subagent whose
///   causal parent is the parent's matching `TaskOutput` result and whose target
///   is the subagent's last op.
#[must_use]
pub fn emit_subagent_notes_from(ops: &[Op], meta: &[SubagentMeta]) -> Vec<Op> {
    if meta.is_empty() {
        return Vec::new();
    }

    // Index ops by session scope so we can find each session's ops quickly.
    // Subagent JSONL records carry the *parent* session id, so both the parent
    // and its subagents share one session scope; we disambiguate by matching on
    // tool_call_id / content rather than scope grouping.
    let mut ops_by_session: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let ScopeRef::Session(sid) = op.scope {
            ops_by_session.entry(sid.0).or_default().push(i);
        }
    }

    let mut notes = Vec::new();
    for m in meta {
        // Resolve the parent session id to its numeric scope.
        let Some(parent_sid) = resolve_session_id(ops, &m.parent_session_id) else {
            continue;
        };
        let Some(parent_indices) = ops_by_session.get(&parent_sid) else {
            continue;
        };

        // Find this subagent's first and last op indices. Subagents share the
        // parent's session scope but are authored by a distinct actor
        // (`agent:{parent}:{subagent}`), so we locate them by actor id.
        let Some((sub_first_idx, sub_last_idx)) = find_subagent_extent(
            ops,
            parent_indices,
            &m.parent_session_id,
            &m.subagent_session_id,
        ) else {
            continue;
        };

        // Branch note: SubagentOf, parent = sub-first, target = Agent-call.
        // Independent of whether a TaskOutput result exists — a subagent always
        // branches from its spawn.
        if let Some(agent_call_idx) = find_agent_tool_use(ops, parent_indices, &m.tool_use_id) {
            if let Some(sub_first_op) = ops.get(sub_first_idx) {
                if let Some(agent_call_id) = ops.get(agent_call_idx).map(|op| op.id) {
                    notes.push(subagent_note(
                        sub_first_op,
                        agent_call_id,
                        NoteRelationship::SubagentOf,
                    ));
                }
            }
        }

        // Reconnect note: ReconnectsTo, parent = completion-result, target =
        // sub-last. Only when the parent reports this subagent as completed (via
        // TaskOutput polling or a TaskStop/late-check "not running (status:
        // completed)" result).
        if let Some(completion_idx) =
            find_subagent_completion(ops, parent_indices, &m.subagent_session_id)
        {
            if let Some(sub_last_op) = ops.get(sub_last_idx) {
                if let Some(completion_op) = ops.get(completion_idx) {
                    notes.push(subagent_note(
                        completion_op,
                        sub_last_op.id,
                        NoteRelationship::ReconnectsTo,
                    ));
                }
            }
        }
    }
    notes
}

/// Reserved high derived-ordinal used for subagent relationship note IDs.
///
/// Normalization allocates derived ordinals sequentially starting at 1 per
/// source record; these reserved values sit far above any realistic content-block
/// count so they never collide with normalized children on the same stream.
const SUBAGENT_NOTE_DISC: u16 = 0xFFFD;

/// Build a relationship note whose causal parent is `parent_op` and whose target
/// is `target_id`.
fn subagent_note(parent_op: &Op, target_id: OpId, relationship: NoteRelationship) -> Op {
    // The note's ID derives from the parent op's stream + a reserved high
    // derived-ordinal, so it is deterministic and collision-free with normalized
    // children on the same stream.
    let stream = crate::ids::SourceStream::new(parent_op.id.node, parent_op.id.boot);
    let note_id = stream
        .op_from_position(crate::ids::SourcePosition::derived(
            parent_op.id.seq >> 16,
            SUBAGENT_NOTE_DISC,
        ))
        .unwrap_or(parent_op.id);
    Op {
        id: note_id,
        parents: editchain_core::parents::ParentSet::One(parent_op.id),
        actor: ActorId(0),
        clock: Clock::None,
        scope: parent_op.scope,
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![target_id],
            relationship,
            content: Payload::Empty,
        }),
    }
}

/// Resolve a session id string to its numeric scope value by scanning ops.
fn resolve_session_id(ops: &[Op], session_id: &str) -> Option<u64> {
    let target = SessionId(crate::ids::derive_session_id(session_id).0);
    ops.iter().find_map(|op| match op.scope {
        ScopeRef::Session(sid) if sid == target => Some(sid.0),
        ScopeRef::Session(_)
        | ScopeRef::None
        | ScopeRef::Chain(_)
        | ScopeRef::Turn(_)
        | ScopeRef::File(_) => None,
    })
}

/// Find the index of the parent's `Agent` `tool_use` with the given call id.
///
/// Matches a `ToolOp` whose `tool_name` is `Agent`, stage is `Start`, and whose
/// `tool_call_id` equals `tool_use_id`.
fn find_agent_tool_use(ops: &[Op], indices: &[usize], tool_use_id: &str) -> Option<usize> {
    indices.iter().copied().find(|&i| {
        ops.get(i).is_some_and(|op| {
            matches!(
                &op.kind,
                OpKind::Tool(ToolOp {
                    tool_name,
                    tool_call_id,
                    stage: ToolStage::Start,
                    ..
                }) if payload_text(tool_name) == "Agent"
                    && payload_text(tool_call_id) == tool_use_id
            )
        })
    })
}

/// Find the index of the parent's completion result for this subagent.
///
/// The parent reports a subagent's completion through one of two real-world
/// `tool_result` shapes (both normalized as a `ToolOp` with `stage: Finish` and
/// an empty `tool_name`, since results don't preserve the tool name):
///
/// 1. **`TaskOutput` polling** — the parent polls the task until it finishes:
///    ```text
///    <retrieval_status>success</retrieval_status>
///    <task_id>{agent-id}</task_id>
///    <task_type>local_agent</task_type>
///    <status>completed</status>
///    <output>...</output>
///    ```
/// 2. **`TaskStop` / late check** — the parent stops or re-checks an already
///    finished task, and the result reports it completed:
///    ```text
///    <tool_use_error>Task {agent-id} is not running (status: completed)</tool_use_error>
///    ```
///
/// We match either shape by referencing the subagent's agent id and reporting a
/// completed status. Tolerant of status strings across Claude Code versions — a
/// result is a success unless it explicitly reports a failure/killed/stopped/
/// running status.
fn find_subagent_completion(
    ops: &[Op],
    indices: &[usize],
    subagent_session_id: &str,
) -> Option<usize> {
    indices.iter().copied().find(|&i| {
        ops.get(i).is_some_and(|op| {
            if let OpKind::Tool(ToolOp {
                stage: ToolStage::Finish,
                content,
                ..
            }) = &op.kind
            {
                let text = payload_text(content);
                // Shape 1: TaskOutput polling result.
                let task_output_completed = text
                    .contains(&format!("<task_id>{subagent_session_id}</task_id>"))
                    && text.contains("<status>completed</status>");
                // Shape 2: TaskStop / late-check "not running (status: completed)".
                let stopped_completed = text.contains(&format!(
                    "Task {subagent_session_id} is not running (status: completed)"
                ));
                (task_output_completed || stopped_completed)
                    && !text.contains("<status>failed</status>")
                    && !text.contains("<status>killed</status>")
                    && !text.contains("<status>stopped</status>")
                    && !text.contains("<status>running</status>")
            } else {
                false
            }
        })
    })
}

/// Find a subagent's first and last op indices within a shared session scope.
///
/// Subagents share their parent's session scope but are authored by a distinct
/// actor: assistant records carry `agentId`, producing actor key
/// `agent:{parent_session_id}:{subagent_session_id}`. We locate the subagent's
/// ops by that deterministic actor id, which is robust even when a session
/// contains multiple subagents (each with its own actor).
fn find_subagent_extent(
    ops: &[Op],
    indices: &[usize],
    parent_session_id: &str,
    subagent_session_id: &str,
) -> Option<(usize, usize)> {
    let actor_key = format!("agent:{parent_session_id}:{subagent_session_id}");
    let target_actor = crate::ids::derive_actor_id(&actor_key);
    let mut first = None;
    let mut last = None;
    for &i in indices {
        if let Some(op) = ops.get(i) {
            if op.actor == target_actor {
                let _: &mut usize = first.get_or_insert(i);
                last = Some(i);
            }
        }
    }
    match (first, last) {
        (Some(f), Some(l)) => Some((f, l)),
        _ => None,
    }
}

/// Extract text from a payload.
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}
