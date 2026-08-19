//! Tests for subagent branch/reconnect relationship-note emission.

#![expect(
    clippy::indexing_slicing,
    clippy::too_many_arguments,
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "Test fixtures use fixed small vectors; indices are known in bounds and the panic/fall-through arms are deliberate in tests"
)]

use blake3 as _;
use editchain_core as _;
use editchain_project as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

use editchain_core::{
    op::{NoteOp, NoteRelationship, OpKind, ToolOp, ToolStage},
    payload::Payload,
    ActorId, Clock, NodeId, Op, OpId, ParentSet, ScopeRef, SessionId, Tags,
};
use editchain_import::ids::derive_session_id;
use editchain_import::subagent::{emit_subagent_notes_from, SubagentMeta};

/// A realistic parent session UUID; ops scope is derived from it.
const PARENT_UUID: &str = "016bbaad-23ec-425d-b53d-3f212a11ce4b";

/// The numeric session scope used by ops in the parent session.
fn parent_sid() -> u64 {
    derive_session_id(PARENT_UUID).0
}

/// Build a Tool op (Agent call / TaskOutput result) with the given call id.
fn tool_op(
    node: u64,
    seq: u64,
    session: u64,
    name: &str,
    call_id: &str,
    stage: ToolStage,
    content: &str,
) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(session)),
        tags: Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(call_id.as_bytes().to_vec()),
            tool_name: Payload::Inline(name.as_bytes().to_vec()),
            stage,
            content: Payload::Inline(content.as_bytes().to_vec()),
        }),
    }
}

/// Build a generic message op (used for subagent chain members).
///
/// Subagent ops are authored by actor `agent:{parent}:{subagent}`.
fn msg_op(node: u64, seq: u64, session: u64) -> Op {
    let actor_key = format!("agent:{PARENT_UUID}:sub-1");
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: editchain_import::ids::derive_actor_id(&actor_key),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(session)),
        tags: Tags::MESSAGE,
        kind: OpKind::Message(editchain_core::MessageOp {
            content: Payload::Inline(b"msg".to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// The parent session's `Agent` tool_use that spawns the subagent.
fn agent_call(session: u64, call_id: &str) -> Op {
    tool_op(1, 100, session, "Agent", call_id, ToolStage::Start, "{}")
}

/// The parent session's `TaskOutput` result reporting completion for a subagent.
///
/// Mirrors real normalization: `tool_name` is empty (results don't preserve the
/// tool name), and the content carries `<task_id>` equal to the subagent's
/// session id (agent id).
fn task_output_success(session: u64, agent_id: &str) -> Op {
    let content =
        format!("<task_id>{agent_id}</task_id>\n<status>completed</status>\n<output>done</output>");
    tool_op(1, 200, session, "", agent_id, ToolStage::Finish, &content)
}

/// The parent session's `TaskStop`/late-check result reporting a subagent that
/// already finished.
///
/// Mirrors the real "not running (status: completed)" shape: a `tool_result`
/// (empty `tool_name`, `stage: Finish`) whose content reports the task id as no
/// longer running because it completed.
fn task_stop_completed(session: u64, agent_id: &str) -> Op {
    let content = format!(
        "<tool_use_error>Task {agent_id} is not running (status: completed)</tool_use_error>"
    );
    tool_op(1, 200, session, "", agent_id, ToolStage::Finish, &content)
}

/// Extract the relationship notes from an emitted note vec.
fn relationships(notes: &[Op]) -> Vec<NoteRelationship> {
    notes
        .iter()
        .filter_map(|op| match &op.kind {
            OpKind::Note(n) => Some(n.relationship),
            _ => None,
        })
        .collect()
}

#[test]
fn subagent_branches_from_parent_agent_call() {
    // Parent session (node 1): Agent call at seq 100. Subagent chain (node 2):
    // first op at seq 10. After linking, a SubagentOf note is emitted whose
    // causal parent is sub-first and whose target is the Agent call.
    let ops = vec![
        agent_call(parent_sid(), "call_abc"),
        msg_op(2, 10, parent_sid()), // subagent first op
        msg_op(2, 11, parent_sid()), // subagent last op
    ];
    let meta = vec![SubagentMeta {
        subagent_session_id: "sub-1".to_string(),
        parent_session_id: PARENT_UUID.to_string(),
        tool_use_id: "call_abc".to_string(),
    }];
    let notes = emit_subagent_notes_from(&ops, &meta);
    // Only the branch note is emitted (no TaskOutput result present).
    assert_eq!(notes.len(), 1);
    assert_eq!(relationships(&notes), vec![NoteRelationship::SubagentOf]);

    // The note's causal parent is sub-first; its target is the Agent call.
    let note = &notes[0];
    let NoteOp { target_ids, .. } = match &note.kind {
        OpKind::Note(n) => n,
        _ => panic!("expected a Note op"),
    };
    assert_eq!(note.parents.iter().collect::<Vec<_>>(), vec![&ops[1].id]);
    assert_eq!(target_ids, &vec![ops[0].id]);
}

#[test]
fn subagent_reconnects_to_task_output_success() {
    // Parent session (node 1): Agent call + TaskOutput success. Subagent chain
    // (node 2). After linking:
    // - a SubagentOf note (sub-first -> Agent call)
    // - a ReconnectsTo note (TaskOutput -> sub-last)
    let ops = vec![
        agent_call(parent_sid(), "call_abc"),
        msg_op(2, 10, parent_sid()), // sub-first
        msg_op(2, 11, parent_sid()), // sub-last
        task_output_success(parent_sid(), "sub-1"),
    ];
    let meta = vec![SubagentMeta {
        subagent_session_id: "sub-1".to_string(),
        parent_session_id: PARENT_UUID.to_string(),
        tool_use_id: "call_abc".to_string(),
    }];
    let notes = emit_subagent_notes_from(&ops, &meta);
    assert_eq!(notes.len(), 2);
}

#[test]
fn subagent_without_success_stays_branched_no_reconnect() {
    // Parent has an Agent call but NO TaskOutput success result. The subagent
    // should still branch from the Agent call but NOT reconnect.
    let ops = vec![
        agent_call(parent_sid(), "call_abc"),
        msg_op(2, 10, parent_sid()), // sub-first
        msg_op(2, 11, parent_sid()), // sub-last
    ];
    let meta = vec![SubagentMeta {
        subagent_session_id: "sub-1".to_string(),
        parent_session_id: PARENT_UUID.to_string(),
        tool_use_id: "call_abc".to_string(),
    }];
    let notes = emit_subagent_notes_from(&ops, &meta);
    assert_eq!(notes.len(), 1);
    assert_eq!(relationships(&notes), vec![NoteRelationship::SubagentOf]);
}

#[test]
fn subagent_reconnects_via_task_stop_not_running() {
    // Parent session (node 1): Agent call + a TaskStop/late-check result that
    // reports the subagent as already completed ("not running (status:
    // completed)"). The subagent should branch AND reconnect.
    let ops = vec![
        agent_call(parent_sid(), "call_abc"),
        msg_op(2, 10, parent_sid()), // sub-first
        msg_op(2, 11, parent_sid()), // sub-last
        task_stop_completed(parent_sid(), "sub-1"),
    ];
    let meta = vec![SubagentMeta {
        subagent_session_id: "sub-1".to_string(),
        parent_session_id: PARENT_UUID.to_string(),
        tool_use_id: "call_abc".to_string(),
    }];
    let notes = emit_subagent_notes_from(&ops, &meta);
    assert_eq!(notes.len(), 2);
}

#[test]
fn input_ops_not_mutated() {
    // Emitting notes must never mutate the input op set.
    let ops = vec![
        agent_call(parent_sid(), "call_abc"),
        msg_op(2, 10, parent_sid()), // sub-first
        msg_op(2, 11, parent_sid()), // sub-last
        task_output_success(parent_sid(), "sub-1"),
    ];
    let before = ops.clone();
    let meta = vec![SubagentMeta {
        subagent_session_id: "sub-1".to_string(),
        parent_session_id: PARENT_UUID.to_string(),
        tool_use_id: "call_abc".to_string(),
    }];
    let _notes = emit_subagent_notes_from(&ops, &meta);
    assert_eq!(ops, before);
}
