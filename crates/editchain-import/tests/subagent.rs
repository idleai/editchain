//! Tests for subagent branch/reconnect linking.

#![expect(
    clippy::indexing_slicing,
    clippy::too_many_arguments,
    clippy::doc_markdown,
    reason = "Test fixtures use fixed small vectors; indices are known in bounds"
)]

use blake3 as _;
use editchain_core as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

use editchain_core::{
    op::{OpKind, ToolOp, ToolStage},
    payload::Payload,
    ActorId, Clock, NodeId, Op, OpId, ParentSet, ScopeRef, SessionId, Tags,
};
use editchain_import::ids::derive_session_id;
use editchain_import::subagent::{link_subagents, SubagentMeta};

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

#[test]
fn subagent_branches_from_parent_agent_call() {
    // Parent session (node 1): Agent call at seq 100. Subagent chain (node 2):
    // first op at seq 10. After linking, sub-first gains a parent to the Agent
    // call.
    let mut ops = vec![
        agent_call(parent_sid(), "call_abc"),
        msg_op(2, 10, parent_sid()), // subagent first op
        msg_op(2, 11, parent_sid()), // subagent last op
    ];
    let meta = vec![SubagentMeta {
        subagent_session_id: "sub-1".to_string(),
        parent_session_id: PARENT_UUID.to_string(),
        tool_use_id: "call_abc".to_string(),
    }];
    let linked = link_subagents(&mut ops, &meta);
    // Only the branch edge is added (no TaskOutput result present).
    assert_eq!(linked, 1);

    // sub-first (index 1) has the Agent call as a parent.
    let sub_first_parents: Vec<_> = ops[1].parents.iter().collect();
    assert!(
        sub_first_parents.iter().any(|p| **p == ops[0].id),
        "sub-first should branch from the Agent call"
    );
}

#[test]
fn subagent_reconnects_to_task_output_success() {
    // Parent session (node 1): Agent call + TaskOutput success. Subagent chain
    // (node 2). After linking:
    // - sub-first.parents includes Agent call
    // - TaskOutput.parents includes sub-last (reconnect)
    let mut ops = vec![
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
    let linked = link_subagents(&mut ops, &meta);
    assert_eq!(linked, 2);

    // sub-first (index 1) has the Agent call as a parent.
    let sub_first_parents: Vec<_> = ops[1].parents.iter().collect();
    assert!(
        sub_first_parents.iter().any(|p| **p == ops[0].id),
        "sub-first should branch from the Agent call"
    );
    // TaskOutput (index 3) has sub-last (index 2) as a parent.
    let task_parents: Vec<_> = ops[3].parents.iter().collect();
    assert!(
        task_parents.iter().any(|p| **p == ops[2].id),
        "TaskOutput should reconnect to the subagent's last op"
    );
}

#[test]
fn subagent_without_success_stays_branched_no_reconnect() {
    // Parent has an Agent call but NO TaskOutput success result. The subagent
    // should still branch from the Agent call but NOT reconnect.
    let mut ops = vec![
        agent_call(parent_sid(), "call_abc"),
        msg_op(2, 10, parent_sid()), // sub-first
        msg_op(2, 11, parent_sid()), // sub-last
    ];
    let meta = vec![SubagentMeta {
        subagent_session_id: "sub-1".to_string(),
        parent_session_id: PARENT_UUID.to_string(),
        tool_use_id: "call_abc".to_string(),
    }];
    let linked = link_subagents(&mut ops, &meta);
    assert_eq!(linked, 1);

    // Branch edge present.
    let sub_first_parents: Vec<_> = ops[1].parents.iter().collect();
    assert!(
        sub_first_parents.iter().any(|p| **p == ops[0].id),
        "sub-first should branch from the Agent call"
    );
}

#[test]
fn subagent_reconnects_via_task_stop_not_running() {
    // Parent session (node 1): Agent call + a TaskStop/late-check result that
    // reports the subagent as already completed ("not running (status:
    // completed)"). The subagent should branch AND reconnect.
    let mut ops = vec![
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
    let linked = link_subagents(&mut ops, &meta);
    assert_eq!(linked, 2);

    // sub-first (index 1) has the Agent call as a parent.
    let sub_first_parents: Vec<_> = ops[1].parents.iter().collect();
    assert!(
        sub_first_parents.iter().any(|p| **p == ops[0].id),
        "sub-first should branch from the Agent call"
    );
    // The TaskStop result (index 3) has sub-last (index 2) as a parent.
    let stop_parents: Vec<_> = ops[3].parents.iter().collect();
    assert!(
        stop_parents.iter().any(|p| **p == ops[2].id),
        "TaskStop result should reconnect to the subagent's last op"
    );
}

#[test]
fn parent_set_capacity_respected() {
    // Sub-first already has two parents; the branch edge must be skipped.
    let mut ops = vec![
        agent_call(parent_sid(), "call_abc"),
        msg_op(2, 10, parent_sid()), // sub-first
        msg_op(2, 11, parent_sid()), // sub-last
        task_output_success(parent_sid(), "sub-1"),
    ];
    // Give sub-first two existing parents.
    ops[1].parents = ParentSet::Two(OpId::new(NodeId(9), 0, 1), OpId::new(NodeId(9), 0, 2));
    let meta = vec![SubagentMeta {
        subagent_session_id: "sub-1".to_string(),
        parent_session_id: PARENT_UUID.to_string(),
        tool_use_id: "call_abc".to_string(),
    }];
    let linked = link_subagents(&mut ops, &meta);
    // Only the reconnect edge is added (branch skipped at capacity).
    assert_eq!(linked, 1);
}
