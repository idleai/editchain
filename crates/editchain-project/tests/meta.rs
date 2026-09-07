//! Tests for deterministic semantic metadata (readability taxonomy) and
//! `hide_trace` chain filtering in the projection.

#![expect(
    clippy::indexing_slicing,
    reason = "Tests index into freshly built vectors"
)]
// Crate-level dependency markers (used by Cargo for feature resolution).
use regex as _;
use serde as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, ImportOp, MessageOp, NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind,
    ParentSet, Payload, ScopeRef, SessionId, Tags, ToolOp, ToolStage, TurnId,
};
use editchain_project::filter::ChainFilter;
use editchain_project::taxonomy::{ActivityKind, ChainState, Outcome, RecordRole, Visibility};
use editchain_project::HistoryProjection;

/// 2^53 + 1 — the first integer JavaScript's IEEE-754 doubles round.
const OVER_2_53: u64 = 9_007_199_254_740_993;

/// Build a raw import op carrying one raw JSONL line.
fn raw_import(node: u64, seq: u64, clock_ms: u64, parent: Option<OpId>, raw: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(raw.as_bytes().to_vec()),
            raw_hash: None,
        }),
    }
}

/// Build a session-scoped normalized child op anchored at a raw import op.
fn child(node: u64, seq: u64, parent: OpId, kind: OpKind) -> Op {
    child_with_clock(node, seq, parent, seq.saturating_mul(1_000), kind)
}

/// Build a turn-scoped normalized child op anchored at a raw import op.
fn turn_child(node: u64, seq: u64, parent: OpId, kind: OpKind) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq.saturating_mul(1_000)),
        scope: ScopeRef::Turn(TurnId(OVER_2_53)),
        tags: Tags::MESSAGE,
        kind,
    }
}

/// Build a normalized child op anchored at a raw import op.
fn child_with_clock(node: u64, seq: u64, parent: OpId, clock_ms: u64, kind: OpKind) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::Session(SessionId(1)),
        tags: Tags::MESSAGE,
        kind,
    }
}

fn message_op(text: &str) -> OpKind {
    OpKind::Message(MessageOp {
        content: Payload::Inline(text.as_bytes().to_vec()),
        content_type: Payload::Empty,
    })
}

fn tool_finish_op() -> OpKind {
    OpKind::Tool(ToolOp {
        tool_call_id: Payload::Inline(b"call-1".to_vec()),
        tool_name: Payload::Empty,
        stage: ToolStage::Finish,
        content: Payload::Inline(b"done".to_vec()),
    })
}

fn note_op(relationship: NoteRelationship) -> OpKind {
    OpKind::Note(NoteOp {
        target_ids: Vec::new(),
        relationship,
        content: Payload::Empty,
    })
}

/// The single projected node for one import (plus its children).
fn sole_node(ops: Vec<Op>) -> editchain_project::HistoryNode {
    let projection = HistoryProjection::from_ops(ops);
    let mut nodes = projection.nodes();
    assert_eq!(nodes.len(), 1, "expected exactly one top-level row");
    nodes.remove(0)
}

#[test]
fn empty_response_item_envelope_is_trace_lifecycle() {
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{}}"#,
    );
    let node = sole_node(vec![raw]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Trace);
    assert_eq!(meta.record_role, RecordRole::Lifecycle);
    assert_eq!(meta.activity_kind, ActivityKind::System);
    assert_eq!(meta.outcome, Outcome::Unknown);
}

#[test]
fn id_only_response_item_envelope_is_trace() {
    // An id-only envelope carries no user content — identity fields are never
    // treated as narrative.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","id":"msg_x"}}"#,
    );
    let node = sole_node(vec![raw]);
    assert_eq!(node.visibility(), Visibility::Trace);
}

#[test]
fn response_item_with_narrative_but_no_children_stays_primary() {
    // A decode-fallback response_item that still carries genuine user/agent
    // narrative must not be classified as trace.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"audit the tree"}]}}"#,
    );
    let node = sole_node(vec![raw]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Primary);
    assert_eq!(meta.record_role, RecordRole::Narrative);
    assert_eq!(meta.activity_kind, ActivityKind::Conversation);
}

#[test]
fn response_item_with_message_child_is_primary_narrative() {
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
    );
    let message = child(2, 1, raw.id, message_op("hi"));
    let node = sole_node(vec![raw, message]);
    assert_eq!(node.visibility(), Visibility::Primary);
    assert_eq!(node.record_role(), RecordRole::Narrative);
    assert_eq!(node.activity_kind(), ActivityKind::Conversation);
}

#[test]
fn claude_interrupted_request_gets_muted_chain_state() {
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},"interruptedMessageId":"msg_cancelled"}"#,
    );
    let message = child(2, 1, raw.id, message_op("[Request interrupted by user]"));
    let node = sole_node(vec![raw, message]);
    assert_eq!(node.chain_state(), ChainState::Muted);
    assert_eq!(node.outcome(), Outcome::Unknown);
}

#[test]
fn claude_tool_use_interruption_without_message_id_is_still_muted() {
    // Claude does not consistently include `interruptedMessageId` on the
    // tool-use variant, so the exact typed provider marker is the fallback.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#,
    );
    let message = child(
        2,
        1,
        raw.id,
        message_op("[Request interrupted by user for tool use]"),
    );
    assert_eq!(
        sole_node(vec![raw, message]).chain_state(),
        ChainState::Muted
    );
}

#[test]
fn codex_turn_abort_reason_folds_muted_state_onto_visible_anchor() {
    let anchor = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"partial response"}]}}"#,
    );
    let message = child(2, 1, anchor.id, message_op("partial response"));
    let mut abort = raw_import(
        1,
        2,
        2_000,
        Some(anchor.id),
        r#"{"type":"event_msg","payload":{"type":"turn_aborted","reason":"model_error"}}"#,
    );
    abort.tags |= Tags::META;

    let projection = HistoryProjection::from_ops_with(
        vec![anchor, message, abort],
        editchain_project::ProjectionOptions {
            bundle_metadata: true,
        },
    );
    let mut nodes = projection.nodes();
    assert_eq!(nodes.len(), 1);
    let node = nodes.remove(0);
    assert_eq!(node.summary(), "partial response turn_aborted");
    assert_eq!(node.chain_state(), ChainState::Muted);
}

#[test]
fn interruption_like_prose_does_not_mutate_chain_state() {
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Please explain how request interruption works"}]}}"#,
    );
    let message = child(
        2,
        1,
        raw.id,
        message_op("Please explain how request interruption works"),
    );
    assert_eq!(
        sole_node(vec![raw, message]).chain_state(),
        ChainState::Active
    );
}

#[test]
fn response_item_reasoning_label_prefers_summary_text() {
    // A childless reasoning row keeps the first non-empty summary block as
    // its label instead of the opaque envelope type.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"  "},{"type":"summary_text","text":"Audit the tree layout"}],"content":[{"type":"reasoning","text":"ignored"}]}}"#,
    );
    let node = sole_node(vec![raw]);
    assert_eq!(node.summary(), "Audit the tree layout");
}

#[test]
fn response_item_reasoning_label_falls_back_to_content_text() {
    // A reasoning row without usable summary text falls back to the first
    // content text, then to the payload type.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"reasoning","summary":[],"content":[{"type":"reasoning","text":"fallback content"}]}}"#,
    );
    let node = sole_node(vec![raw]);
    assert_eq!(node.summary(), "fallback content");

    let bare = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"reasoning"}}"#,
    );
    assert_eq!(sole_node(vec![bare]).summary(), "reasoning");
}

#[test]
fn response_item_message_label_prefers_content_text() {
    // A childless message row keeps its content text; without content it shows
    // the payload type rather than the opaque envelope type.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Plan the migration"}]}}"#,
    );
    let node = sole_node(vec![raw]);
    assert_eq!(node.summary(), "Plan the migration");

    let bare = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","id":"msg_x"}}"#,
    );
    assert_eq!(sole_node(vec![bare]).summary(), "message");
}

#[test]
fn response_item_other_payload_types_show_payload_type() {
    // Non-reasoning/message payload types label with `payload.type` instead
    // of the opaque envelope type; an empty payload keeps `response_item`.
    let call = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"function_call","name":"Bash"}}"#,
    );
    assert_eq!(sole_node(vec![call]).summary(), "function_call");

    let empty = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{}}"#,
    );
    assert_eq!(sole_node(vec![empty]).summary(), "response_item");
}

#[test]
fn response_item_label_bounds_unicode_summary_text() {
    // Labels never expose unbounded text: long multibyte summaries are cut to
    // the display limit with an ellipsis and never split a code point.
    let huge = format!(
        r#"{{"type":"response_item","payload":{{"type":"reasoning","summary":[{{"type":"summary_text","text":"{}"}}]}}}}"#,
        "界".repeat(5_000),
    );
    let raw = raw_import(1, 1, 1_000, None, &huge);
    let summary = sole_node(vec![raw]).summary();
    assert_eq!(summary.chars().count(), 1_025);
    assert!(summary.ends_with('…'));
    assert!(summary.starts_with("界".repeat(1_024).as_str()));
}

#[test]
fn duplicate_item_completed_envelope_is_trace_lifecycle() {
    // The completion marker repeats lifecycle state already folded onto the
    // item's first-seen row: it produced no normalized children.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"item_completed","thread_id":"t","turn_id":"turn-1","item":{"type":"UserMessage","id":"msg_1"}}}"#,
    );
    let node = sole_node(vec![raw]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Trace);
    assert_eq!(meta.record_role, RecordRole::Lifecycle);
    assert_eq!(meta.activity_kind, ActivityKind::System);
}

#[test]
fn compacted_checkpoint_stays_primary_plan_content() {
    // Compaction is a user-visible context checkpoint. Its Activity topology
    // is normalized separately; semantic filtering must not erase the row.
    let compacted = raw_import(
        1,
        2,
        2_000,
        None,
        r#"{"type":"compacted","payload":{"message":"","replacement_history":[]}}"#,
    );
    let compacted_id = compacted.id;
    let projection = HistoryProjection::from_ops(vec![compacted]);

    let raw_nodes = projection.nodes();
    let checkpoint = raw_nodes
        .iter()
        .find(|node| node.node_key() == compacted_id.to_string())
        .expect("Raw mode keeps context checkpoint");
    assert_eq!(checkpoint.visibility(), Visibility::Primary);
    assert_eq!(checkpoint.record_role(), RecordRole::Narrative);
    assert_eq!(checkpoint.activity_kind(), ActivityKind::Plan);

    let activity_nodes = projection.filtered_nodes(&ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        true,
    ));
    assert_eq!(activity_nodes.len(), 1);
    assert_eq!(activity_nodes[0].node_key(), compacted_id.to_string());
}

#[test]
fn item_completed_with_tool_result_child_is_primary_result_success() {
    // A tool item first materialized on its completion line carries real content
    // children — it is an action/result row, never trace noise.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"item_completed","thread_id":"t","turn_id":"turn-1","item":{"type":"CommandExecution","id":"call_1","status":"completed","aggregatedOutput":"done"}}}"#,
    );
    let tool = turn_child(2, 1, raw.id, tool_finish_op());
    let node = sole_node(vec![raw, tool]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Primary);
    assert_eq!(meta.record_role, RecordRole::Result);
    assert_eq!(meta.activity_kind, ActivityKind::Execute);
    assert_eq!(meta.outcome, Outcome::Success);
    assert_eq!(meta.turn_id, Some(TurnId(OVER_2_53)));
}

#[test]
fn outcome_failure_comes_from_exit_code() {
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"call_9","exitCode":1,"status":"completed"}}}"#,
    );
    let tool = child(2, 1, raw.id, tool_finish_op());
    let node = sole_node(vec![raw, tool]);
    assert_eq!(node.outcome(), Outcome::Failure);
}

#[test]
fn outcome_failure_comes_from_canonical_codex_exec_result() {
    // Codex custom exec results do not persist an exitCode. Their first typed
    // output block is the durable execution-status envelope.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","output":[{"type":"input_text","text":"Script failed\nWall time 0.0 seconds\nOutput:\n"},{"type":"input_text","text":"Script error:\ncommand rejected"}]}}"#,
    );
    let tool = child(2, 1, raw.id, tool_finish_op());
    let node = sole_node(vec![raw, tool]);
    assert_eq!(node.record_role(), RecordRole::Result);
    assert_eq!(node.outcome(), Outcome::Failure);
}

#[test]
fn outcome_ignores_script_failed_outside_canonical_exec_result() {
    // Display text is not outcome evidence. Even the same words stay unknown
    // unless they occur in the complete typed Codex execution envelope.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","output":[{"type":"input_text","text":"The script failed during an earlier attempt."}]}}"#,
    );
    let tool = child(2, 1, raw.id, tool_finish_op());
    assert_eq!(sole_node(vec![raw, tool]).outcome(), Outcome::Unknown);
}

#[test]
fn outcome_stays_unknown_without_structured_evidence() {
    // A tool call with no status/exitCode/error evidence must not be guessed.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"function_call","name":"Bash","arguments":"{}"}}"#,
    );
    let tool = child(
        2,
        1,
        raw.id,
        OpKind::Tool(ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Empty,
        }),
    );
    let node = sole_node(vec![raw, tool]);
    assert_eq!(node.record_role(), RecordRole::Action);
    assert_eq!(node.outcome(), Outcome::Unknown);
}

#[test]
fn inter_agent_echo_line_is_trace_echo_external() {
    // An inter-agent-shaped line with no raw payload content and no content
    // child stays trace; the raw-narrative guard only rescues rows that
    // actually carry user content.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"inter_agent_communication","payload":{"author":"/root/sub","recipient":"/root"}}"#,
    );
    let note_child = child(2, 1, raw.id, note_op(NoteRelationship::Explains));
    let node = sole_node(vec![raw, note_child]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Trace);
    assert_eq!(meta.record_role, RecordRole::Echo);
    assert_eq!(meta.activity_kind, ActivityKind::External);
}

#[test]
fn childless_agent_message_with_narrative_in_payload_stays_primary() {
    // A decode-fallback/unprojected agent_message that produced no normalized
    // child still carries genuine narrative in its raw payload: the
    // conservative inter-agent gate must not hide it as trace.
    let event_msg = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"I will audit the tree now"}}"#,
    );
    let event_node = sole_node(vec![event_msg]);
    assert_eq!(event_node.visibility(), Visibility::Primary);
    assert_eq!(event_node.record_role(), RecordRole::Narrative);
    assert_eq!(event_node.activity_kind(), ActivityKind::Conversation);

    let response = raw_import(
        3,
        1,
        2_000,
        None,
        r#"{"type":"response_item","payload":{"type":"agent_message","message":"narrative from the bridge decode"}}"#,
    );
    let response_node = sole_node(vec![response]);
    assert_eq!(response_node.visibility(), Visibility::Primary);
    assert_eq!(response_node.record_role(), RecordRole::Narrative);
    assert_eq!(response_node.activity_kind(), ActivityKind::Conversation);
}

#[test]
fn exact_external_tool_prefix_echo_stays_trace_without_children() {
    // The stable marker prefix wins over the raw-narrative guard: an exact
    // external-tool echo with no normalized child at all must still classify
    // as trace.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_call] {\"tool\":\"Bash\",\"command\":\"ls\"}"}}"#,
    );
    let node = sole_node(vec![raw]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Trace);
    assert_eq!(meta.record_role, RecordRole::Echo);
    assert_eq!(meta.activity_kind, ActivityKind::External);
}

#[test]
fn childless_tool_like_response_item_with_object_payload_stays_primary() {
    // Non-empty object tool payloads under arguments/parameters/input are
    // genuine content carriers: a childless envelope must not collapse to
    // empty transport (trace) just because the nested keys are not on the
    // text whitelist.
    let cases = [
        (
            1,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"WebSearch","arguments":{"query":"editchain docs"}}}"#,
        ),
        (
            2,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"Bash","parameters":{"command":"ls -la"}}}"#,
        ),
        (
            3,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"Read","input":{"path":"/tmp/x"}}}"#,
        ),
    ];
    for (node_id, raw) in cases {
        let node = sole_node(vec![raw_import(node_id, 1, node_id * 1_000, None, raw)]);
        assert_eq!(node.visibility(), Visibility::Primary, "case {raw}");
        assert_eq!(node.record_role(), RecordRole::Action, "case {raw}");
        assert_eq!(node.activity_kind(), ActivityKind::Execute, "case {raw}");
    }
}

#[test]
fn childless_response_item_with_empty_structured_carrier_stays_trace() {
    // An empty object/array carrier carries no content signal: id/type-only
    // envelopes remain trace even when they mention a tool-payload key.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"function_call","arguments":{}}}"#,
    );
    let node = sole_node(vec![raw]);
    assert_eq!(node.visibility(), Visibility::Trace);
}

#[test]
fn childless_tool_like_response_item_with_scalar_payload_stays_primary() {
    // Scalar tool-payload carriers are genuine content too: non-empty strings
    // and bool/number values under arguments/input/parameters keep a childless
    // envelope primary, so they are never hidden as empty transport.
    let cases = [
        (
            5,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"Bash","arguments":"ls -la"}}"#,
        ),
        (
            6,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"Read","input":"/tmp/x"}}"#,
        ),
        (
            7,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"Bash","parameters":true}}"#,
        ),
        (
            8,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"Tool","parameters":7}}"#,
        ),
    ];
    for (node_id, raw) in cases {
        let node = sole_node(vec![raw_import(node_id, 1, node_id * 1_000, None, raw)]);
        assert_eq!(node.visibility(), Visibility::Primary, "case {raw}");
        assert_eq!(node.record_role(), RecordRole::Action, "case {raw}");
        assert_eq!(node.activity_kind(), ActivityKind::Execute, "case {raw}");
    }
}

#[test]
fn childless_response_item_with_empty_scalar_carrier_stays_trace() {
    // Null, empty-string, and empty object/array carriers carry no content
    // signal: those envelopes remain trace even when they mention a
    // tool-payload key.
    let cases = [
        (
            5,
            r#"{"type":"response_item","payload":{"type":"function_call","arguments":""}}"#,
        ),
        (
            6,
            r#"{"type":"response_item","payload":{"type":"function_call","parameters":null}}"#,
        ),
        (
            7,
            r#"{"type":"response_item","payload":{"type":"function_call","input":[]}}"#,
        ),
    ];
    for (node_id, raw) in cases {
        let node = sole_node(vec![raw_import(node_id, 1, node_id * 1_000, None, raw)]);
        assert_eq!(node.visibility(), Visibility::Trace, "case {raw}");
    }
}

#[test]
fn inter_agent_line_with_genuine_content_child_stays_primary() {
    // Conservative guard: an inter-agent-shaped line carrying a real content
    // child (e.g. a bridge decode surprise) must not be hidden as trace.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"inter_agent_communication","payload":{"author":"/root/sub","recipient":"/root","content":"status update"}}"#,
    );
    let message = child(2, 1, raw.id, message_op("status update"));
    let node = sole_node(vec![raw, message]);
    assert_eq!(node.visibility(), Visibility::Primary);
    assert_eq!(node.record_role(), RecordRole::Narrative);
}

#[test]
fn event_msg_agent_message_external_tool_call_prefix_is_trace_even_with_message_child() {
    // A legacy `event_msg` `agent_message` whose text starts with the stable
    // external-tool marker is a duplicate echo of the subagent's tool call —
    // the normalized Message child repeats the same text and must not rescue
    // the row from the trace family.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_call] {\"tool\":\"Bash\",\"command\":\"ls\"}"}}"#,
    );
    let message = child(2, 1, raw.id, message_op("[external_agent_tool_call] echo"));
    let node = sole_node(vec![raw, message]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Trace);
    assert_eq!(meta.record_role, RecordRole::Echo);
    assert_eq!(meta.activity_kind, ActivityKind::External);
}

#[test]
fn response_item_assistant_external_tool_result_prefix_is_trace_even_with_message_child() {
    // A `response_item` message row (role assistant) whose first content text
    // starts with the external-tool-result marker echoes the subagent's tool
    // result; the normalized Message child does not rescue it.
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"[external_agent_tool_result] done"}]}}"#,
    );
    let message = child(
        2,
        1,
        raw.id,
        message_op("[external_agent_tool_result] done"),
    );
    let node = sole_node(vec![raw, message]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Trace);
    assert_eq!(meta.record_role, RecordRole::Echo);
    assert_eq!(meta.activity_kind, ActivityKind::External);
}

#[test]
fn generic_agent_prose_without_external_marker_stays_primary() {
    // Generic agent/inter-agent prose never carries the external-tool marker;
    // a normalized Message child keeps these rows primary.
    let event_msg = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"I will audit the tree now"}}"#,
    );
    let event_message = child(2, 1, event_msg.id, message_op("I will audit the tree now"));
    let event_node = sole_node(vec![event_msg, event_message]);
    assert_eq!(event_node.visibility(), Visibility::Primary);
    assert_eq!(event_node.record_role(), RecordRole::Narrative);

    let response = raw_import(
        3,
        1,
        2_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"working on it"}]}}"#,
    );
    let response_message = child(4, 1, response.id, message_op("working on it"));
    let response_node = sole_node(vec![response, response_message]);
    assert_eq!(response_node.visibility(), Visibility::Primary);
    assert_eq!(response_node.record_role(), RecordRole::Narrative);
    assert_eq!(response_node.activity_kind(), ActivityKind::Conversation);
}

#[test]
fn marker_with_colon_metadata_delimiter_is_trace_echo_external() {
    // The safe marker stem followed by a colon metadata delimiter
    // (`[external_agent_tool_call: Bash]`, `[external_agent_tool_result: Read]`)
    // is the same external-agent echo family as the bare bracket form.
    let call = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_call: Bash]\ndescription: audit tree\n[/external_agent_tool_call]"}}"#,
    );
    let call_node = sole_node(vec![call]);
    let call_meta = call_node.record_meta();
    assert_eq!(call_meta.visibility, Visibility::Trace);
    assert_eq!(call_meta.record_role, RecordRole::Echo);
    assert_eq!(call_meta.activity_kind, ActivityKind::External);

    let result = raw_import(
        3,
        1,
        2_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"[external_agent_tool_result: Read]\nread tree\n[/external_agent_tool_result]"}]}}"#,
    );
    let result_node = sole_node(vec![result]);
    let result_meta = result_node.record_meta();
    assert_eq!(result_meta.visibility, Visibility::Trace);
    assert_eq!(result_meta.record_role, RecordRole::Echo);
    assert_eq!(result_meta.activity_kind, ActivityKind::External);
}

#[test]
fn marker_lookalike_without_delimiter_stays_primary() {
    // `[external_agent_tool_callback]` shares the stem's first characters but
    // is not followed by `]` or `:` — it is genuine prose, never a trace
    // marker, on either echo shape.
    let event_msg = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_callback] invoked"}}"#,
    );
    let event_node = sole_node(vec![event_msg]);
    assert_eq!(event_node.visibility(), Visibility::Primary);
    assert_eq!(event_node.record_role(), RecordRole::Narrative);

    let response = raw_import(
        3,
        1,
        2_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"[external_agent_tool_callback] invoked"}]}}"#,
    );
    let response_node = sole_node(vec![response]);
    assert_eq!(response_node.visibility(), Visibility::Primary);
    assert_eq!(response_node.record_role(), RecordRole::Narrative);
    assert_eq!(response_node.activity_kind(), ActivityKind::Conversation);
}

#[test]
fn normal_duplicate_pair_hides_response_item_keeps_event_msg() {
    // A `response_item` message/assistant row whose untruncated message text
    // is exactly duplicated by an `event_msg` agent_message row in the same
    // source chain at the same timestamp is the echo copy: the response_item
    // is Trace/Echo, the event_msg stays Primary/Narrative. "Exact" requires
    // untruncated text on both sides — truncated service-compacted text is
    // excluded from pairing (see `truncated_*` tests below).
    let text = "I will audit the tree now";
    let response = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"I will audit the tree now"}]}}"#,
    );
    let event_msg = raw_import(
        1,
        2,
        1_000,
        Some(response.id),
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"I will audit the tree now"}}"#,
    );
    let response_message = child(3, 1, response.id, message_op(text));
    let projection = HistoryProjection::from_ops(vec![response, event_msg, response_message]);
    let nodes = projection.nodes();

    let response_node = nodes
        .iter()
        .find(|n| n.node_key() == "1:0:1")
        .expect("response_item row");
    let response_meta = response_node.record_meta();
    assert_eq!(response_meta.visibility, Visibility::Trace);
    assert_eq!(response_meta.record_role, RecordRole::Echo);
    assert_eq!(response_meta.activity_kind, ActivityKind::External);

    let event_node = nodes
        .iter()
        .find(|n| n.node_key() == "1:0:2")
        .expect("event_msg row");
    let event_meta = event_node.record_meta();
    assert_eq!(event_meta.visibility, Visibility::Primary);
    assert_eq!(event_meta.record_role, RecordRole::Narrative);
    assert_eq!(event_meta.activity_kind, ActivityKind::Conversation);
}

#[test]
fn unique_response_item_without_event_duplicate_stays_primary() {
    // No matching event_msg row exists: the response_item is genuine narrative
    // and must never be hidden by pairing.
    let response = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"unique assistant narrative"}]}}"#,
    );
    let message = child(2, 1, response.id, message_op("unique assistant narrative"));
    let node = sole_node(vec![response, message]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Primary);
    assert_eq!(meta.record_role, RecordRole::Narrative);
    assert_eq!(meta.activity_kind, ActivityKind::Conversation);
}

#[test]
fn duplicate_pairing_is_order_independent_and_one_to_one() {
    // Reversed input order (event_msg first) still demotes exactly the
    // response_item copy.
    let response = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"reversed pair"}]}}"#,
    );
    let event_msg = raw_import(
        1,
        2,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"reversed pair"}}"#,
    );
    let projection = HistoryProjection::from_ops(vec![event_msg, response]);
    let nodes = projection.nodes();
    let response_node = nodes
        .iter()
        .find(|n| n.node_key() == "1:0:1")
        .expect("response_item row");
    assert_eq!(response_node.record_meta().visibility, Visibility::Trace);
    let event_node = nodes
        .iter()
        .find(|n| n.node_key() == "1:0:2")
        .expect("event_msg row");
    assert_eq!(event_node.record_meta().visibility, Visibility::Primary);

    // Multiple identical messages pair one-to-one: with two event rows and
    // three response copies, exactly two response copies are demoted and the
    // third stays primary (no response_item is hidden without a distinct
    // matching event row).
    let mut ops = Vec::new();
    let mut response_keys = Vec::new();
    let mut event_keys = Vec::new();
    for i in 1..=3u64 {
        let seq = i * 2 - 1;
        let response = raw_import(
            1,
            seq,
            2_000,
            None,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"identical echo"}]}}"#,
        );
        response_keys.push(response.id.to_string());
        ops.push(response);
    }
    for i in 1..=2u64 {
        let event_msg = raw_import(
            1,
            i * 2,
            2_000,
            None,
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"identical echo"}}"#,
        );
        event_keys.push(event_msg.id.to_string());
        ops.push(event_msg);
    }
    let projection = HistoryProjection::from_ops(ops);
    let nodes = projection.nodes();
    let mut trace_keys = Vec::new();
    for node in nodes {
        if node.record_meta().visibility == Visibility::Trace {
            trace_keys.push(node.node_key());
        }
    }
    let paired_response_keys = response_keys
        .iter()
        .filter(|key| trace_keys.contains(key))
        .count();
    assert_eq!(
        paired_response_keys, 2,
        "two of three identical response copies demoted: {trace_keys:?}"
    );
    assert!(
        trace_keys.iter().all(|key| !event_keys.contains(key)),
        "event_msg rows stay visible: {trace_keys:?}"
    );

    // A different timestamp or chain never pairs: the same text must share the
    // exact clock and source chain to count as a duplicate.
    let different_clock_response = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"other timestamp"}]}}"#,
    );
    let different_clock_event = raw_import(
        1,
        2,
        2_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"other timestamp"}}"#,
    );
    let projection = HistoryProjection::from_ops(vec![
        different_clock_response.clone(),
        different_clock_event.clone(),
    ]);
    let nodes = projection.nodes();
    for node in nodes {
        assert_eq!(
            node.record_meta().visibility,
            Visibility::Primary,
            "different timestamps never pair"
        );
    }

    let different_chain_response = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"other chain"}]}}"#,
    );
    let different_chain_event = raw_import(
        2,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"other chain"}}"#,
    );
    let projection =
        HistoryProjection::from_ops(vec![different_chain_response, different_chain_event]);
    let nodes = projection.nodes();
    for node in nodes {
        assert_eq!(
            node.record_meta().visibility,
            Visibility::Primary,
            "different source chains never pair"
        );
    }
}

#[test]
fn user_role_response_item_is_never_demoted_by_event_duplicate() {
    // Pairing targets the assistant copy only; a `role == "user"` response_item
    // with the same text as an event_msg row stays primary narrative even
    // though the texts match.
    let response = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"shared prose"}]}}"#,
    );
    let event_msg = raw_import(
        1,
        2,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"shared prose"}}"#,
    );
    let projection = HistoryProjection::from_ops(vec![response, event_msg]);
    let nodes = projection.nodes();
    for node in nodes {
        assert_eq!(
            node.record_meta().visibility,
            Visibility::Primary,
            "user-role response_item is never a trace echo copy"
        );
        assert_eq!(node.record_meta().record_role, RecordRole::Narrative);
    }
}

#[test]
fn trace_filter_hides_unconditionally_and_splices_chain() {
    // chain: import_a (message) -> trace envelope -> import_c (message).
    let a = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"alpha"}]}}"#,
    );
    let trace = raw_import(
        2,
        1,
        2_000,
        Some(a.id),
        r#"{"type":"response_item","payload":{}}"#,
    );
    let c = raw_import(
        3,
        1,
        3_000,
        Some(trace.id),
        r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"gamma"}]}}"#,
    );
    let ma = child(4, 1, a.id, message_op("alpha"));
    let mc = child(5, 1, c.id, message_op("gamma"));
    let projection = HistoryProjection::from_ops(vec![a.clone(), trace.clone(), c.clone(), ma, mc]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        true,
    );
    let nodes = projection.filtered_nodes(&filter);
    let keys: Vec<String> = nodes
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    assert_eq!(
        keys.len(),
        2,
        "trace row removed, splice reconnects: {keys:?}"
    );
    assert!(
        !keys.contains(&trace.id.to_string()),
        "trace envelope hidden"
    );
    let c_node = nodes
        .iter()
        .find(|n| n.node_key() == c.id.to_string())
        .expect("gamma row kept");
    assert_eq!(
        c_node.parent_keys(&projection.git.links, projection.relationship_notes()),
        vec![a.id.to_string()],
        "spliced parent skips the hidden trace row"
    );
}

#[test]
fn trace_filter_hides_trace_leaves_too() {
    // A lone trace leaf is noise and must not survive as an endpoint anchor,
    // matching `hide_undated` semantics.
    let a = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"alpha"}]}}"#,
    );
    let trace = raw_import(
        2,
        1,
        2_000,
        Some(a.id),
        r#"{"type":"response_item","payload":{}}"#,
    );
    let ma = child(3, 1, a.id, message_op("alpha"));
    let projection = HistoryProjection::from_ops(vec![a.clone(), trace.clone(), ma]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        true,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node_key(), a.id.to_string());
}

#[test]
fn hide_trace_preserves_structural_anchor_rows() {
    // A SubagentOf note anchored on a trace row keeps that row visible so the
    // virtual branch edge survives.
    let spawn = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"spawned"}]}}"#,
    );
    let trace = raw_import(
        2,
        1,
        2_000,
        Some(spawn.id),
        r#"{"type":"response_item","payload":{}}"#,
    );
    let sub = raw_import(
        3,
        1,
        3_000,
        Some(trace.id),
        r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"sub work"}]}}"#,
    );
    let mspawn = child(4, 1, spawn.id, message_op("spawned"));
    let msub = child(5, 1, sub.id, message_op("sub work"));
    let note = Op {
        id: OpId::new(NodeId(9), 0, 2),
        parents: ParentSet::One(trace.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![spawn.id],
            relationship: NoteRelationship::SubagentOf,
            content: Payload::Empty,
        }),
    };
    let projection = HistoryProjection::from_ops(vec![
        spawn.clone(),
        trace.clone(),
        sub.clone(),
        mspawn,
        msub,
        note,
    ]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        true,
        true,
    );
    let nodes = projection.filtered_nodes(&filter);
    let keys: Vec<String> = nodes
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    assert!(
        keys.contains(&trace.id.to_string()),
        "structural anchor preserved under hide_trace: {keys:?}"
    );
    let trace_node = nodes
        .iter()
        .find(|n| n.node_key() == trace.id.to_string())
        .expect("preserved trace anchor row");
    assert_eq!(
        trace_node.parent_keys(&projection.git.links, projection.relationship_notes()),
        vec![spawn.id.to_string()],
        "virtual SubagentOf edge renders from the preserved trace anchor"
    );
    let sub_node = nodes
        .iter()
        .find(|n| n.node_key() == sub.id.to_string())
        .expect("sub row");
    assert_eq!(
        sub_node.parent_keys(&projection.git.links, projection.relationship_notes()),
        vec![trace.id.to_string()],
        "sub row still points at the preserved structural anchor"
    );
}

#[test]
fn standalone_turn_scoped_op_exposes_turn_id() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_000),
        scope: ScopeRef::Turn(TurnId(OVER_2_53)),
        tags: Tags::MESSAGE,
        kind: message_op("turn scoped"),
    };
    let projection = HistoryProjection::from_ops(vec![op]);
    let node = projection.nodes().into_iter().next().expect("row");
    assert_eq!(node.turn_id(), Some(TurnId(OVER_2_53)));
    assert_eq!(node.record_role(), RecordRole::Narrative);
}

#[test]
fn non_turn_rows_have_no_turn_id() {
    let raw = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"hi"}]}}"#,
    );
    let message = child(2, 1, raw.id, message_op("hi"));
    let node = sole_node(vec![raw, message]);
    assert_eq!(node.turn_id(), None);
}

#[test]
fn filter_key_and_is_empty_include_hide_trace() {
    let hide = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        false,
        true,
    );
    let keep = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        false,
        false,
    );
    assert_ne!(
        hide.key(),
        keep.key(),
        "hide_trace must participate in cache keys"
    );
    assert!(!hide.is_empty());
    assert!(keep.is_empty());

    let default_filter = ChainFilter::default();
    assert!(
        !default_filter.is_empty(),
        "default filter still hides undated rows"
    );
    assert!(
        !default_filter.key().hide_trace,
        "ChainFilter::default is the raw baseline; hide_trace is an explicit choice"
    );
    assert!(
        default_filter.key().hide_undated,
        "existing default behavior preserved"
    );
}

#[test]
fn truncated_echo_text_never_participates_in_duplicate_pairing() {
    // The service projection flags service-compacted echo text as truncated
    // (`payload.echo_text_truncated`); a flagged side must never pair, even
    // when the bounded texts compare equal. Family-3 external-marker
    // classification is unaffected (the marker lives at the text start).
    // (a) A truncated event side is never counted, so the matching response
    //     stays primary.
    let response = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"same bounded text"}]}}"#,
    );
    let truncated_event = raw_import(
        1,
        2,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"same bounded text","echo_text_truncated":true}}"#,
    );
    let projection = HistoryProjection::from_ops(vec![response, truncated_event]);
    let nodes = projection.nodes();
    for node in &nodes {
        assert_eq!(
            node.record_meta().visibility,
            Visibility::Primary,
            "truncated event side never demotes the response copy"
        );
    }

    // (b) A truncated response side never consumes a pair slot, so it stays
    //     primary even though an untruncated event row matches the text.
    let truncated_response = raw_import(
        2,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"same bounded text"}],"echo_text_truncated":true}}"#,
    );
    let event = raw_import(
        2,
        2,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"same bounded text"}}"#,
    );
    let projection = HistoryProjection::from_ops(vec![truncated_response, event]);
    let nodes = projection.nodes();
    for node in &nodes {
        assert_eq!(
            node.record_meta().visibility,
            Visibility::Primary,
            "truncated response side is never demoted"
        );
    }

    // (c) Family-3 external-marker classification still works on a truncated
    //     text: the marker prefix survives the display cut.
    let truncated_marker = raw_import(
        3,
        1,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_call: Bash] long description","echo_text_truncated":true}}"#,
    );
    let node = sole_node(vec![truncated_marker]);
    let meta = node.record_meta();
    assert_eq!(meta.visibility, Visibility::Trace);
    assert_eq!(meta.record_role, RecordRole::Echo);
    assert_eq!(meta.activity_kind, ActivityKind::External);
}

#[test]
fn truncated_long_texts_with_shared_prefix_never_pair_after_compaction() {
    // The service compacts two distinct long texts sharing one display-preview
    // prefix to the SAME bounded text; without an explicit signal they would
    // compare equal and the response_item would be wrongly demoted. The
    // truncation flag on both compacted records keeps them unpaired.
    let prefix = "shared-prefix-".repeat(200);
    let response = raw_import(
        1,
        1,
        1_000,
        None,
        &format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{prefix}TAIL-A"}}],"echo_text_truncated":true}}}}"#,
        ),
    );
    let event = raw_import(
        1,
        2,
        1_000,
        None,
        &format!(
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{prefix}TAIL-B","echo_text_truncated":true}}}}"#,
        ),
    );
    let projection = HistoryProjection::from_ops(vec![response, event]);
    let nodes = projection.nodes();
    for node in &nodes {
        assert_eq!(
            node.record_meta().visibility,
            Visibility::Primary,
            "same-prefix truncated texts never pair: {}",
            node.node_key()
        );
    }

    // Untruncated long texts remain exact pairs: the full text still
    // distinguishes the tails, and the matching pair demotes the response.
    let exact_response = raw_import(
        2,
        1,
        1_000,
        None,
        &format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{prefix}TAIL-A"}}]}}}}"#,
        ),
    );
    let exact_event = raw_import(
        2,
        2,
        1_000,
        None,
        &format!(
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{prefix}TAIL-A"}}}}"#,
        ),
    );
    let projection = HistoryProjection::from_ops(vec![exact_response, exact_event]);
    let nodes = projection.nodes();
    let response_node = nodes
        .iter()
        .find(|n| n.node_key() == "2:0:1")
        .expect("exact long pair response row");
    assert_eq!(
        response_node.record_meta().visibility,
        Visibility::Trace,
        "untruncated exact long pair still demotes the response copy"
    );
    let event_node = nodes
        .iter()
        .find(|n| n.node_key() == "2:0:2")
        .expect("exact long pair event row");
    assert_eq!(event_node.record_meta().visibility, Visibility::Primary);
}

#[test]
fn duplicate_pair_demotion_keeps_response_visible_with_unique_tool_child() {
    // Family-4 demotes only a duplicated Message child: the event_msg row is
    // the canonical narrative copy. A response_item that also owns a unique
    // normalized Tool child carries content the event row cannot replace, so
    // the response row must stay visible instead of being hidden as trace.
    let response = raw_import(
        1,
        1,
        1_000,
        None,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"shared tool prose"}]}}"#,
    );
    let event = raw_import(
        1,
        2,
        1_000,
        None,
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"shared tool prose"}}"#,
    );
    let tool = child(3, 1, response.id, tool_finish_op());
    let projection = HistoryProjection::from_ops(vec![response, event, tool]);
    let nodes = projection.nodes();

    let response_node = nodes
        .iter()
        .find(|n| n.node_key() == "1:0:1")
        .expect("response_item row");
    let response_meta = response_node.record_meta();
    assert_eq!(
        response_meta.visibility,
        Visibility::Primary,
        "unique Tool child keeps the paired response row visible"
    );
    assert_eq!(response_meta.record_role, RecordRole::Result);
    assert_eq!(response_meta.activity_kind, ActivityKind::Execute);

    let event_node = nodes
        .iter()
        .find(|n| n.node_key() == "1:0:2")
        .expect("event_msg row");
    let event_meta = event_node.record_meta();
    assert_eq!(event_meta.visibility, Visibility::Primary);
    assert_eq!(event_meta.record_role, RecordRole::Narrative);
    assert_eq!(event_meta.activity_kind, ActivityKind::Conversation);
}
