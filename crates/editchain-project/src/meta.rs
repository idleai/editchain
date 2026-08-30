//! Deterministic semantic metadata for projected history rows.
//!
//! Derives the provider-neutral readability taxonomy (`RecordRole`,
//! `ActivityKind`, `Visibility`, `Outcome`, and turn identity) from the raw
//! and normalized structure of each row — never from display summaries.
//!
//! The slice classifies four measured trace families so the fixed Activity
//! view can hide them unconditionally:
//!
//! 1. **Raw/empty `response_item` envelopes** — a `response_item` line whose
//!    payload carries no user content and that produced no normalized
//!    children. A `response_item` with narrative text or a unique projected
//!    action is never classified as trace.
//! 2. **Duplicate `item_completed` lifecycle envelopes** — an
//!    `item_completed` line (top-level or `payload.type`) that produced no
//!    normalized children: the completion marker repeats lifecycle state
//!    already folded onto the item's first-seen row.
//! 3. **External-agent tool-call/result echo messages** — raw
//!    `inter_agent_communication` lines and `response_item` `agent_message`
//!    payloads, which echo an external agent's activity into the owning
//!    thread. Guarded so a genuine content child (message/tool/command/file/
//!    reflection/error) or raw payload narrative keeps the row primary.
//! 4. **Cross-record `response_item`/`event_msg` duplicate pairs** — a
//!    `response_item` `payload.type == "message"` `role == "assistant"` row
//!    whose message text is exactly duplicated, one-to-one, by an `event_msg`
//!    `payload.type == "agent_message"` row in the same source chain at the
//!    same timestamp. "Exact" requires **untruncated** text on both sides:
//!    the service projection flags service-compacted echo text
//!    (`payload.echo_text_truncated`) and a flagged side never pairs, so two
//!    distinct long texts that share a display-preview prefix are never
//!    conflated. Only the `response_item` copy is demoted; the `event_msg`
//!    row stays visible, a unique `response_item` with no matching event row
//!    is never hidden, and a paired `response_item` that owns unique
//!    non-narrative normalized content (Tool/Command/File/Reflection/Error
//!    child) stays visible because the event row cannot canonically replace
//!    that content.
//!
//! Outcomes are only ever `Success`/`Failure`/`Cancelled` when the raw JSON
//! carries structured evidence (`status`, `errorMessage`, `exitCode`); absent
//! evidence always yields `Outcome::Unknown`.

use std::collections::HashMap;

use crate::taxonomy::{ActivityKind, Outcome, RecordRole, Visibility};
use editchain_core::op::ImportOp;
use editchain_core::payload::Payload;
use editchain_core::{Op, OpKind, ScopeRef, SessionId, Tags, TurnId};
use serde_json::Value;

/// A fork-branch source chain key: `(OpId.node, OpId.boot)`, matching the
/// projection's chain identity so pairing never crosses source streams.
type EchoSourceChainKey = (u64, u32);

/// An exact signature of one side of a duplicate `response_item`/`event_msg`
/// echo pair.
///
/// The signature is derived from structure already available on the raw
/// import op — the source chain, owning session (when scoped), the exact
/// observed timestamp, and the untruncated message text — never from display
/// summaries. "Exact" requires untruncated text: service-compacted echo text
/// (flagged `payload.echo_text_truncated`) never produces a signature, so two
/// distinct long texts sharing a display-preview prefix cannot collide. Both
/// sides of a genuine pair share every field, so the signature is an exact
/// equality key for O(1) hash pairing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EchoPairSignature {
    chain: EchoSourceChainKey,
    session: Option<SessionId>,
    clock_ms: u64,
    text: String,
}

/// Which side of a duplicate `response_item`/`event_msg` echo pair a raw
/// import row represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EchoPairSide {
    /// The `response_item` `payload.type == "message"` `role == "assistant"`
    /// copy — the row that is demoted to trace when an event row matches.
    ResponseItem,
    /// The `event_msg` `payload.type == "agent_message"` row — always kept
    /// visible; it is the canonical narrative copy of the pair.
    EventMsg,
}

/// One-to-one duplicate-pair bookkeeping, built in a single O(n) pass.
///
/// `from_ops` counts the visible `event_msg` side per signature; each
/// `response_item` side is then paired (and demoted) against that count in
/// input order, so multiple identical messages pair deterministically
/// one-to-one and no `response_item` is hidden without a distinct matching
/// event row.
#[derive(Debug, Default)]
pub(crate) struct EchoPairState {
    event_counts: HashMap<EchoPairSignature, usize>,
    response_seen: HashMap<EchoPairSignature, usize>,
}

impl EchoPairState {
    /// Build the pair state from a projection's operations.
    #[must_use]
    pub(crate) fn from_ops(ops: &[Op]) -> Self {
        let mut event_counts: HashMap<EchoPairSignature, usize> = HashMap::new();
        for op in ops {
            if let Some(signature) = echo_pair_signature(op, EchoPairSide::EventMsg) {
                let count = event_counts.entry(signature).or_default();
                *count = count.saturating_add(1);
            }
        }
        Self {
            event_counts,
            response_seen: HashMap::new(),
        }
    }

    /// Whether a `response_item` row is the duplicate copy of a matching
    /// `event_msg` row.
    ///
    /// O(1) amortized per row. Call once per raw import op in input order;
    /// rows that render as top-level rows consume one pair slot each, so the
    /// pairing is deterministic and at most one `response_item` is demoted per
    /// distinct matching event row, regardless of input order or side.
    pub(crate) fn is_paired_response_item(&mut self, op: &Op) -> bool {
        let Some(signature) = echo_pair_signature(op, EchoPairSide::ResponseItem) else {
            return false;
        };
        let Some(&event_total) = self.event_counts.get(&signature) else {
            return false;
        };
        let seen = self.response_seen.entry(signature).or_default();
        let paired = *seen < event_total;
        *seen = seen.saturating_add(1);
        paired
    }
}

/// Derive the exact pair signature for one side of a duplicate echo pair,
/// when the row is a member of that family at all.
///
/// Both sides must carry a real observed timestamp: without one, "same
/// timestamp" pairing is meaningless and rows tagged `SOURCE_TIME_UNKNOWN`
/// (or with no clock) never pair. "Exact" requires untruncated text: a row
/// whose service-compacted echo text was truncated (flagged
/// `payload.echo_text_truncated`) never produces a signature, so distinct
/// long texts that share a display-preview prefix are never conflated.
#[must_use]
fn echo_pair_signature(op: &Op, side: EchoPairSide) -> Option<EchoPairSignature> {
    if op.tags.matches_any(Tags::SOURCE_TIME_UNKNOWN) {
        return None;
    }
    let clock_ms = op.clock.as_u64();
    if clock_ms == 0 {
        return None;
    }
    let value = raw_import_json(op)?;
    let record_type = value.get("type").and_then(Value::as_str);
    let event_type = value
        .get("payload")
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str);
    let (expected_record, expected_event) = match side {
        EchoPairSide::ResponseItem => (Some("response_item"), Some("message")),
        EchoPairSide::EventMsg => (Some("event_msg"), Some("agent_message")),
    };
    if record_type != expected_record || event_type != expected_event {
        return None;
    }
    if echo_text_truncated(&value) {
        return None;
    }
    let text = external_echo_message_text(&value, record_type, event_type)?;
    let session = match op.scope {
        ScopeRef::Session(session) => Some(session),
        ScopeRef::None | ScopeRef::Chain(_) | ScopeRef::Turn(_) | ScopeRef::File(_) => None,
    };
    Some(EchoPairSignature {
        chain: (op.id.node.0, op.id.boot),
        session,
        clock_ms,
        text,
    })
}

/// Deterministic readability metadata for one projected history row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeMeta {
    /// Provider-neutral record role.
    pub record_role: RecordRole,
    /// Provider-neutral activity kind.
    pub activity_kind: ActivityKind,
    /// Render prominence (trace rows are hidden by `hide_trace` filtering).
    pub visibility: Visibility,
    /// Concluded outcome; `Unknown` unless structured evidence exists.
    pub outcome: Outcome,
    /// Owning turn identity, when the row is turn-scoped.
    pub turn_id: Option<TurnId>,
}

impl NodeMeta {
    /// A trace row with the given role/activity.
    const fn trace(record_role: RecordRole, activity_kind: ActivityKind) -> Self {
        Self {
            record_role,
            activity_kind,
            visibility: Visibility::Trace,
            outcome: Outcome::Unknown,
            turn_id: None,
        }
    }

    /// Attach a turn identity (the raw import op itself is session-scoped, but
    /// a bundled turn-scoped child can carry the turn).
    const fn with_turn_id(mut self, turn_id: Option<TurnId>) -> Self {
        self.turn_id = turn_id;
        self
    }
}

/// Metadata for a standalone (non-collapsed) `EditOperation` row.
#[must_use]
pub(crate) fn for_edit_operation(op: &Op) -> NodeMeta {
    use editchain_core::op::{CommandStage, ToolStage};
    let turn_id = turn_id_of_scope(op.scope);
    let (record_role, activity_kind) = match &op.kind {
        OpKind::Message(_) => (RecordRole::Narrative, ActivityKind::Conversation),
        OpKind::Tool(t) => {
            let role = if matches!(t.stage, ToolStage::Finish) {
                RecordRole::Result
            } else {
                RecordRole::Action
            };
            (role, ActivityKind::Execute)
        }
        OpKind::Command(c) => {
            let role = if matches!(c.stage, CommandStage::Finish) {
                RecordRole::Result
            } else {
                RecordRole::Action
            };
            (role, ActivityKind::Execute)
        }
        OpKind::File(_) => (RecordRole::Artifact, ActivityKind::Change),
        OpKind::Reflection(_) => (RecordRole::Narrative, ActivityKind::Plan),
        OpKind::Error(_) => (RecordRole::Result, ActivityKind::Diagnose),
        OpKind::ChainStart(_) | OpKind::Actor(_) | OpKind::Import(_) | OpKind::Note(_) => {
            (RecordRole::Lifecycle, ActivityKind::System)
        }
        OpKind::GitCommit(_) | OpKind::GitLink(_) => {
            (RecordRole::Artifact, ActivityKind::SourceControl)
        }
        OpKind::Unknown(_) => (RecordRole::Unknown, ActivityKind::Unknown),
    };
    let outcome = if matches!(op.kind, OpKind::Error(_)) {
        Outcome::Failure
    } else {
        Outcome::Unknown
    };
    NodeMeta {
        record_role,
        activity_kind,
        visibility: Visibility::Primary,
        outcome,
        turn_id,
    }
}

/// Metadata for a `GitCommit` row.
#[must_use]
pub(crate) const fn for_git_commit() -> NodeMeta {
    NodeMeta {
        record_role: RecordRole::Artifact,
        activity_kind: ActivityKind::SourceControl,
        visibility: Visibility::Primary,
        outcome: Outcome::Unknown,
        turn_id: None,
    }
}

/// Metadata for a collapsed raw-import row, classified from its raw JSONL
/// envelope and its normalized children.
#[must_use]
pub(crate) fn for_collapsed_import(
    op: &Op,
    children: Option<&[&Op]>,
    duplicate_of_event_msg: bool,
) -> NodeMeta {
    let turn_id = children.and_then(|cs| cs.iter().find_map(|c| turn_id_of_scope(c.scope)));
    let Some(value) = raw_import_json(op) else {
        return children_based_meta(children, None, turn_id);
    };
    let record_type = value.get("type").and_then(Value::as_str);
    let event_type = value
        .get("payload")
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str);
    let has_children = children.is_some_and(|cs| !cs.is_empty());

    // Family 3: external-agent tool-call/result echo messages. The raw marker
    // (`[external_agent_tool_call]` / `[external_agent_tool_result]`, with an
    // optional `: <tool>` metadata suffix) is the stable inter-agent
    // discriminator, so rows carrying it classify as trace even when a
    // normalized Message child repeats the echoed text.
    if is_external_tool_echo(&value, record_type, event_type) {
        return NodeMeta::trace(RecordRole::Echo, ActivityKind::External).with_turn_id(turn_id);
    }
    // Family 4: exact cross-record duplicate of an `event_msg` `agent_message`
    // row in the same source chain at the same timestamp (one-to-one paired by
    // the projection's `EchoPairState`, untruncated text only). The
    // `response_item` copy is the echo; the `event_msg` side stays visible as
    // the canonical narrative row. A paired response_item that owns unique
    // non-narrative normalized content (a Tool/Command/File/Reflection/Error
    // child) stays visible because the event row cannot canonically replace
    // that content — only a duplicated Message child is safe to demote.
    if duplicate_of_event_msg && !has_unique_content_child(children) {
        return NodeMeta::trace(RecordRole::Echo, ActivityKind::External).with_turn_id(turn_id);
    }
    // Unmarked inter-agent prose is classified from the envelope shape alone,
    // guarded so a genuine content child or raw payload narrative keeps the
    // row primary.
    if is_inter_agent_line(record_type, event_type)
        && !has_content_child(children)
        && !value.get("payload").is_some_and(json_has_user_content)
    {
        return NodeMeta::trace(RecordRole::Echo, ActivityKind::External).with_turn_id(turn_id);
    }
    // Family 1: raw/empty response_item envelopes. A response_item whose
    // payload carries narrative text or a unique projected action is never
    // classified as trace.
    if record_type == Some("response_item")
        && !has_children
        && !value.get("payload").is_some_and(json_has_user_content)
    {
        return NodeMeta::trace(RecordRole::Lifecycle, ActivityKind::System).with_turn_id(turn_id);
    }
    // Family 2: duplicate item_completed lifecycle envelopes. A completion
    // marker that produced no normalized children repeats lifecycle state
    // already folded onto the item's first-seen row.
    if is_item_completed(record_type, event_type) && !has_children {
        return NodeMeta::trace(RecordRole::Lifecycle, ActivityKind::System).with_turn_id(turn_id);
    }
    children_based_meta(children, Some(&value), turn_id)
}

/// Classify a row from its normalized children, falling back to the raw
/// payload structure when there are no children.
#[must_use]
fn children_based_meta(
    children: Option<&[&Op]>,
    raw: Option<&Value>,
    turn_id: Option<TurnId>,
) -> NodeMeta {
    let Some(children) = children.filter(|cs| !cs.is_empty()) else {
        return raw_payload_meta(raw, turn_id);
    };
    let (record_role, activity_kind, tool_or_command) = {
        use editchain_core::op::{CommandStage, ToolStage};
        let mut record_role = RecordRole::Unknown;
        let mut activity_kind = ActivityKind::Unknown;
        let mut tool_or_command = false;
        for child in children {
            match &child.kind {
                OpKind::Message(_) => {
                    record_role = RecordRole::Narrative;
                    activity_kind = ActivityKind::Conversation;
                    break;
                }
                OpKind::Reflection(_) => {
                    record_role = RecordRole::Narrative;
                    activity_kind = ActivityKind::Plan;
                    break;
                }
                OpKind::Tool(t) => {
                    tool_or_command = true;
                    record_role = if matches!(t.stage, ToolStage::Finish) {
                        RecordRole::Result
                    } else {
                        RecordRole::Action
                    };
                    activity_kind = ActivityKind::Execute;
                    break;
                }
                OpKind::Command(c) => {
                    tool_or_command = true;
                    record_role = if matches!(c.stage, CommandStage::Finish) {
                        RecordRole::Result
                    } else {
                        RecordRole::Action
                    };
                    activity_kind = ActivityKind::Execute;
                    break;
                }
                OpKind::File(_) => {
                    record_role = RecordRole::Artifact;
                    activity_kind = ActivityKind::Change;
                    break;
                }
                OpKind::Error(_) => {
                    record_role = RecordRole::Result;
                    activity_kind = ActivityKind::Diagnose;
                    break;
                }
                OpKind::Note(_) => {
                    record_role = RecordRole::Lifecycle;
                    activity_kind = ActivityKind::System;
                    break;
                }
                OpKind::ChainStart(_)
                | OpKind::Actor(_)
                | OpKind::Import(_)
                | OpKind::GitCommit(_)
                | OpKind::GitLink(_)
                | OpKind::Unknown(_) => {}
            }
        }
        (record_role, activity_kind, tool_or_command)
    };
    let outcome = if tool_or_command {
        raw_status_outcome(raw).unwrap_or(Outcome::Unknown)
    } else {
        Outcome::Unknown
    };
    NodeMeta {
        record_role,
        activity_kind,
        visibility: Visibility::Primary,
        outcome,
        turn_id,
    }
}

/// Whether any child is genuine content (narrative/action/result/artifact)
/// rather than an annotation note. Used as the conservative guard for the
/// external-agent echo family.
#[must_use]
fn has_content_child(children: Option<&[&Op]>) -> bool {
    children.is_some_and(|cs| {
        cs.iter().any(|c| {
            matches!(
                &c.kind,
                OpKind::Message(_)
                    | OpKind::Tool(_)
                    | OpKind::Command(_)
                    | OpKind::File(_)
                    | OpKind::Reflection(_)
                    | OpKind::Error(_)
            )
        })
    })
}

/// Whether any child is unique non-narrative content (action/result/
/// artifact/reflection/diagnosis) that a duplicated `event_msg` row cannot
/// canonically replace.
///
/// A duplicated Message child is safe to demote — the `event_msg`
/// `agent_message` row is the canonical narrative copy — but a
/// Tool/Command/File/Reflection/Error child carries content the event row
/// does not, so the response row must stay visible when it owns such a child.
#[must_use]
fn has_unique_content_child(children: Option<&[&Op]>) -> bool {
    children.is_some_and(|cs| {
        cs.iter().any(|c| {
            matches!(
                &c.kind,
                OpKind::Tool(_)
                    | OpKind::Command(_)
                    | OpKind::File(_)
                    | OpKind::Reflection(_)
                    | OpKind::Error(_)
            )
        })
    })
}

/// Whether a raw line is the external-agent echo lane: a legacy
/// `inter_agent_communication` line or a `response_item` `agent_message`.
#[must_use]
fn is_inter_agent_line(record_type: Option<&str>, event_type: Option<&str>) -> bool {
    record_type == Some("inter_agent_communication")
        || (record_type == Some("response_item") && event_type == Some("agent_message"))
}

/// Stable marker stems for external-agent tool calls/results echoed as
/// messages into the owning thread. A marker is only recognized when the stem
/// is followed by a closing bracket (`[external_agent_tool_call]`) or a colon
/// metadata delimiter (`[external_agent_tool_call: Bash]`), so lookalikes such
/// as `[external_agent_tool_callback]` never match.
const EXTERNAL_AGENT_TOOL_STEMS: [&str; 2] =
    ["[external_agent_tool_call", "[external_agent_tool_result"];

/// Whether `text` starts with a recognized external-agent tool marker.
#[must_use]
fn starts_with_external_tool_marker(text: &str) -> bool {
    EXTERNAL_AGENT_TOOL_STEMS.iter().any(|stem| {
        text.strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with(']') || rest.starts_with(':'))
    })
}

/// Whether the raw envelope is an external-agent tool-call/result echo.
///
/// Two raw shapes carry these markers:
/// - `event_msg` `payload.type == "agent_message"` whose `payload.message`
///   starts with a marker prefix;
/// - `response_item` `payload.type == "message"` with `role == "assistant"`
///   whose first `payload.content` text starts with a marker prefix.
#[must_use]
fn is_external_tool_echo(
    value: &Value,
    record_type: Option<&str>,
    event_type: Option<&str>,
) -> bool {
    let Some(text) = external_echo_message_text(value, record_type, event_type) else {
        return false;
    };
    starts_with_external_tool_marker(&text)
}

/// The bounded message text that may carry an external-agent tool marker.
#[must_use]
fn external_echo_message_text(
    value: &Value,
    record_type: Option<&str>,
    event_type: Option<&str>,
) -> Option<String> {
    let payload = value.get("payload")?;
    match (record_type, event_type) {
        (Some("event_msg"), Some("agent_message")) => payload
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned),
        (Some("response_item"), Some("message")) => {
            if payload.get("role").and_then(Value::as_str) != Some("assistant") {
                return None;
            }
            let content = payload.get("content")?.as_array()?;
            content.iter().find_map(|item| {
                item.get("text")
                    .or_else(|| item.get("input_text"))
                    .or_else(|| item.get("output_text"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToOwned::to_owned)
            })
        }
        _ => None,
    }
}

/// Whether the service projection flagged a record's echo message text as
/// truncated.
///
/// The service compacts raw import JSON to bounded display previews before
/// projection; when the echo text (`payload.message` on an
/// `event_msg`/`agent_message`, or the first `payload.content` text on a
/// `response_item`/`message`) was truncated by the preview read limit or the
/// display budget, the compact JSON carries `payload.echo_text_truncated:
/// true`. Truncated text must never participate in exact duplicate pairing —
/// two distinct long texts sharing a display-preview prefix would otherwise
/// compare equal — while Family-3 external-marker classification still reads
/// the truncated prefix (the marker lives at the text start).
#[must_use]
fn echo_text_truncated(value: &Value) -> bool {
    value
        .get("payload")
        .and_then(|payload| payload.get("echo_text_truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether a raw line is an `item_completed` lifecycle marker.
#[must_use]
fn is_item_completed(record_type: Option<&str>, event_type: Option<&str>) -> bool {
    record_type == Some("item_completed") || event_type == Some("item_completed")
}

/// Parse the raw JSONL of an import op, if it is inline JSON.
#[must_use]
fn raw_import_json(op: &Op) -> Option<Value> {
    let raw = match &op.kind {
        OpKind::Import(ImportOp { raw_ref, .. }) => match raw_ref {
            Payload::Inline(bytes) => bytes,
            Payload::Empty | Payload::Blob(_) => return None,
        },
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::Tool(_)
        | OpKind::Command(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => return None,
    };
    serde_json::from_slice(raw).ok()
}

/// Whether a JSON value carries non-empty text under a content-bearing key.
///
/// Identity/transport fields (`id`, `type`, `timestamp`, `ordinal`,
/// `thread_id`, `turn_id`, `role`, ...) are never treated as content, so an
/// id-only envelope is still an empty envelope. Tool-payload carrier keys
/// (`arguments`/`input`/`parameters`) count as content when they hold a
/// non-empty object/array, non-empty string, or scalar boolean/number even if
/// the nested keys are not on the text whitelist, so a childless tool-like
/// envelope is never hidden as empty transport.
#[must_use]
fn json_has_user_content(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, child)| {
            (is_tool_payload_carrier_key(key) && is_meaningful_carrier_value(child))
                || (is_content_key(key) && json_has_user_content(child))
        }),
        Value::Array(items) => items.iter().any(json_has_user_content),
        Value::String(text) => !text.trim().is_empty(),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Whether a JSON object key is a tool-payload carrier whose meaningful value
/// (non-empty object/array, non-empty string, or scalar boolean/number) is
/// user content even when its nested keys are not on the text whitelist.
#[must_use]
fn is_tool_payload_carrier_key(key: &str) -> bool {
    matches!(key, "arguments" | "input" | "parameters")
}

/// Whether a JSON value carries a meaningful tool-payload signal.
///
/// Non-empty objects/arrays, non-empty strings, and scalar booleans/numbers
/// all carry signal; null, empty strings, and empty objects/arrays do not.
#[must_use]
fn is_meaningful_carrier_value(value: &Value) -> bool {
    match value {
        Value::Object(map) => !map.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::String(text) => !text.trim().is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
        Value::Null => false,
    }
}

/// Whether a JSON object key is a content carrier rather than identity/
/// transport bookkeeping.
#[must_use]
fn is_content_key(key: &str) -> bool {
    matches!(
        key,
        "text"
            | "content"
            | "summary_text"
            | "summary"
            | "output"
            | "input_text"
            | "output_text"
            | "prompt"
            | "arguments"
            | "error"
            | "error_message"
            | "errorMessage"
            | "message"
    )
}

/// Derive a structured outcome from the raw JSON of a tool/command row.
///
/// Returns `None` (unknown) when no structured evidence is present; success is
/// never inferred from absence of evidence.
#[must_use]
fn raw_status_outcome(raw: Option<&Value>) -> Option<Outcome> {
    let value = raw?;
    let error = value
        .pointer("/payload/item/errorMessage")
        .or_else(|| value.pointer("/payload/errorMessage"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if error {
        return Some(Outcome::Failure);
    }
    let exit_code = value
        .pointer("/payload/item/exitCode")
        .or_else(|| value.pointer("/payload/exitCode"))
        .and_then(Value::as_i64);
    if let Some(code) = exit_code {
        return if code == 0 {
            Some(Outcome::Success)
        } else {
            Some(Outcome::Failure)
        };
    }
    let status = value
        .pointer("/payload/item/status")
        .or_else(|| value.pointer("/payload/status"))
        .and_then(Value::as_str);
    match status {
        Some("completed" | "success" | "succeeded" | "ok") => Some(Outcome::Success),
        Some("error" | "failed" | "failure") => Some(Outcome::Failure),
        Some("cancelled" | "canceled" | "aborted") => Some(Outcome::Cancelled),
        _ => None,
    }
}

/// Conservative fallback for a childless import row: classify from the raw
/// payload structure.
#[must_use]
fn raw_payload_meta(raw: Option<&Value>, turn_id: Option<TurnId>) -> NodeMeta {
    let Some(value) = raw else {
        return NodeMeta {
            turn_id,
            ..NodeMeta::default()
        };
    };
    let record_type = value.get("type").and_then(Value::as_str);
    let event_type = value
        .get("payload")
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str);
    let (record_role, activity_kind) = match record_type {
        Some("message" | "assistant" | "user") => {
            (RecordRole::Narrative, ActivityKind::Conversation)
        }
        Some("reasoning" | "compacted") => (RecordRole::Narrative, ActivityKind::Plan),
        Some("function_call" | "custom_tool_call") => (RecordRole::Action, ActivityKind::Execute),
        Some("function_call_output" | "custom_tool_call_output") => {
            (RecordRole::Result, ActivityKind::Execute)
        }
        Some("event_msg" | "response_item") => match event_type {
            Some("message" | "agent_message") => {
                (RecordRole::Narrative, ActivityKind::Conversation)
            }
            Some("reasoning") => (RecordRole::Narrative, ActivityKind::Plan),
            Some("function_call" | "custom_tool_call") => {
                (RecordRole::Action, ActivityKind::Execute)
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                (RecordRole::Result, ActivityKind::Execute)
            }
            Some(
                "task_started"
                | "task_complete"
                | "token_count"
                | "turn_aborted"
                | "thread_settings_applied"
                | "item_completed",
            ) => (RecordRole::Lifecycle, ActivityKind::System),
            _ => (RecordRole::Unknown, ActivityKind::Unknown),
        },
        Some(
            "session_meta"
            | "world_state"
            | "turn_context"
            | "inter_agent_communication_metadata"
            | "item_completed"
            | "attachment"
            | "system"
            | "mode"
            | "permission-mode"
            | "custom-title"
            | "last-prompt"
            | "agent-name"
            | "file-history-snapshot"
            | "queue-operation"
            | "ai-title",
        ) => (RecordRole::Lifecycle, ActivityKind::System),
        _ => (RecordRole::Unknown, ActivityKind::Unknown),
    };
    NodeMeta {
        record_role,
        activity_kind,
        visibility: Visibility::Primary,
        outcome: Outcome::Unknown,
        turn_id,
    }
}

/// The turn identity of a turn-scoped op, if any.
#[must_use]
const fn turn_id_of_scope(scope: ScopeRef) -> Option<TurnId> {
    match scope {
        ScopeRef::Turn(turn) => Some(turn),
        ScopeRef::None | ScopeRef::Chain(_) | ScopeRef::Session(_) | ScopeRef::File(_) => None,
    }
}
