//! Subagent branch/reconnect linking.
//!
//! Claude Code runs subagents as background sessions that branch off the
//! parent's `Agent` tool_use and (on success) reconnect when the parent's
//! `TaskOutput` tool_result reports completion. This module augments operation
//! parents so the unified history graph shows those branch and reconnect edges.
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
//! lane layout require — we add:
//!
//! - **Branch**: `sub-first.parents += Agent-call` (a downward edge from the
//!   subagent's first op to the parent's `Agent` tool_use).
//! - **Reconnect**: `TaskOutput.parents += sub-last` (a downward merge edge from
//!   the parent's `TaskOutput` result down to the subagent's last op). This is
//!   the geometric inverse of "subagent reconnects into the parent": the parent's
//!   completion result visually merges back into the subagent chain that produced
//!   it, which is what reads correctly in a newest-first graph.

use std::collections::HashMap;

use editchain_core::{
    op::{OpKind, ToolOp, ToolStage},
    payload::Payload,
    Op, OpId, ParentSet,
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

/// Link subagent sessions into their parent's chain via branch/reconnect edges.
///
/// Mutates `ops` in place, adding parents:
/// - each subagent's first op gains a parent to the parent session's matching
///   `Agent` `tool_use` (a `ToolOp` with `tool_name == "Agent"`, `stage: Start`,
///   and `tool_call_id` equal to `meta.tool_use_id`);
/// - the parent session's matching `TaskOutput` result (a `ToolOp` with
///   `tool_name == "TaskOutput"`, `stage: Finish`, whose content reports a
///   success status) gains a parent to that subagent's last op.
///
/// Both edges respect [`ParentSet`] capacity (max 2 parents): if a target op
/// already has two parents, the edge is skipped rather than dropped.
#[must_use]
pub fn link_subagents(ops: &mut [Op], meta: &[SubagentMeta]) -> usize {
    if meta.is_empty() {
        return 0;
    }

    // Index ops by session scope so we can find each session's ops quickly.
    // Subagent JSONL records carry the *parent* session id, so both the parent
    // and its subagents share one session scope; we disambiguate by matching on
    // tool_call_id / content rather than scope grouping.
    let mut ops_by_session: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let editchain_core::ScopeRef::Session(sid) = op.scope {
            ops_by_session.entry(sid.0).or_default().push(i);
        }
    }

    let mut linked = 0usize;
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

        // Branch: sub-first.parents += Agent-call. Independent of whether a
        // TaskOutput result exists — a subagent always branches from its spawn.
        if let Some(agent_call_idx) = find_agent_tool_use(ops, parent_indices, &m.tool_use_id) {
            if let Some(agent_call_id) = ops.get(agent_call_idx).map(|op| op.id) {
                if let Some(sub_first_op) = ops.get_mut(sub_first_idx) {
                    if add_parent(sub_first_op, agent_call_id) {
                        linked = linked.saturating_add(1);
                    }
                }
            }
        }

        // Reconnect: completion-result.parents += sub-last. Only when the parent
        // reports this subagent as completed (via TaskOutput polling or a
        // TaskStop/late-check "not running (status: completed)" result).
        if let Some(completion_idx) =
            find_subagent_completion(ops, parent_indices, &m.subagent_session_id)
        {
            if let Some(sub_last_id) = ops.get(sub_last_idx).map(|op| op.id) {
                if let Some(completion_op) = ops.get_mut(completion_idx) {
                    if add_parent(completion_op, sub_last_id) {
                        linked = linked.saturating_add(1);
                    }
                }
            }
        }
    }
    linked
}

/// Resolve a session id string to its numeric scope value by scanning ops.
fn resolve_session_id(ops: &[Op], session_id: &str) -> Option<u64> {
    let target = editchain_core::SessionId(crate::ids::derive_session_id(session_id).0);
    ops.iter().find_map(|op| match op.scope {
        editchain_core::ScopeRef::Session(sid) if sid == target => Some(sid.0),
        editchain_core::ScopeRef::Session(_)
        | editchain_core::ScopeRef::None
        | editchain_core::ScopeRef::Chain(_)
        | editchain_core::ScopeRef::Turn(_)
        | editchain_core::ScopeRef::File(_) => None,
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

/// Add a parent to an op, respecting `ParentSet` capacity (max 2).
///
/// Returns true if the edge was added; false if it was skipped (already present
/// or at capacity).
fn add_parent(op: &mut Op, parent: OpId) -> bool {
    match op.parents {
        ParentSet::None => {
            op.parents = ParentSet::One(parent);
            true
        }
        ParentSet::One(existing) => {
            if existing == parent {
                false
            } else {
                op.parents = ParentSet::Two(existing, parent);
                true
            }
        }
        ParentSet::Two(..) => false,
    }
}

/// Extract text from a payload.
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}
