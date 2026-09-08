use editchain_core::clock::Clock;
use editchain_core::op::{
    CommandOp, CommandStage, FileEdit, FileOp, FileStage, FrontierSet, ImportOp, MessageOp, NoteOp,
    NoteRelationship, OpKind, ReflectionOp, ToolOp, ToolStage, WindowRef,
};
use editchain_core::parents::ParentSet;
use editchain_core::payload::Payload;
use editchain_core::scope::ScopeRef;
use editchain_core::tags::Tags;
use editchain_core::{ActorId, Op, OpId, SessionId};
use serde_json::Value;

use super::projection::{CompactedLine, FinalItem, InterAgentLine, ProjectionKind, TurnMeta};
use crate::claude_code::normalize::parse_source_time;
use crate::error::ImportError;
use crate::ids::{
    derive_actor_id, derive_path_id, derive_turn_id, IdError, SourcePosition, SourceStream,
};
use crate::sink::{payload_for, BlobSink};
use std::collections::HashMap;

/// Minimal top-level metadata extracted from a raw Codex JSONL line.
///
/// Only the top-level `type`/`timestamp` and nested `payload.type` discriminator
/// are read; all message/tool/turn content comes from the helper projection,
/// never from raw parsing. The nested discriminator is used only to classify
/// lifecycle-only `event_msg` records as foldable metadata.
#[derive(Debug, Clone, Default)]
pub struct RawLineMeta {
    /// Top-level record type (e.g. `session_meta`, `event_msg`, `response_item`).
    pub raw_type: String,
    /// Top-level `timestamp` string, when present.
    pub timestamp: Option<String>,
    /// Nested `payload.type` discriminator, when present.
    pub event_type: Option<String>,
}

/// Extract the minimal top-level metadata from a raw Codex JSONL line.
///
/// Unparseable or non-object lines yield defaults (`raw_type` empty, no
/// timestamp) — the raw lane preserves them byte-exact regardless.
#[must_use]
pub fn parse_raw_line_meta(data: &[u8]) -> RawLineMeta {
    let value: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
    let obj = value.as_object();
    RawLineMeta {
        raw_type: obj
            .and_then(|o| o.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        timestamp: obj
            .and_then(|o| o.get("timestamp"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        event_type: obj
            .and_then(|o| o.get("payload"))
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
    }
}

/// Extract the owning thread id from a raw `session_meta` line
/// (`payload.id`). Returns `None` for any other line or shape.
#[must_use]
pub fn owning_thread_from_raw_line(data: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(data).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    value
        .get("payload")
        .and_then(|p| p.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

/// Whether a raw line's non-newline bytes are whitespace-only (the bridge emits
/// no projection record for such lines).
#[must_use]
pub fn is_blank_line(data: &[u8]) -> bool {
    let mut bytes = data;
    if let Some(rest) = bytes.strip_suffix(b"\n") {
        bytes = rest;
    }
    if let Some(rest) = bytes.strip_suffix(b"\r") {
        bytes = rest;
    }
    bytes.iter().all(u8::is_ascii_whitespace)
}

/// Extra raw-lane tags for a top-level Codex record type.
#[must_use]
pub fn raw_type_tags(raw_type: &str) -> Tags {
    match raw_type {
        "session_meta"
        | "world_state"
        | "turn_context"
        | "token_usage_record"
        | "inter_agent_communication_metadata" => Tags::META,
        "compacted" => Tags::STRUCTURAL,
        _ => Tags::NONE,
    }
}

/// Extra raw-lane tags derived from the minimal Codex record envelope.
///
/// Terminal lifecycle `event_msg` records contain no independent conversational
/// content. They remain byte-exact raw imports, but carry `META` so the history
/// projection can fold them only through their unique stored source parent.
/// Other event messages (including `task_started` and user/agent messages)
/// remain ordinary rows because they carry independent lifecycle or content.
#[must_use]
pub fn raw_line_tags(meta: &RawLineMeta) -> Tags {
    let mut tags = raw_type_tags(&meta.raw_type);
    if meta.raw_type == "event_msg"
        && matches!(
            meta.event_type.as_deref(),
            Some("task_complete" | "token_count" | "turn_aborted")
        )
    {
        tags |= Tags::META;
    }
    tags
}

/// Deterministic actor key for a raw lane op, derived from the top-level type
/// and the owning thread id only (no conversation semantics).
#[must_use]
pub fn raw_actor_key(raw_type: &str, thread: &str) -> String {
    match raw_type {
        "session_meta"
        | "world_state"
        | "turn_context"
        | "token_usage_record"
        | "inter_agent_communication_metadata"
        | "compacted" => format!("system:{thread}"),
        "" => format!("codex:unknown:{thread}"),
        other => format!("codex:{other}:{thread}"),
    }
}

/// Build the clock and source-time-unknown flag for a raw line timestamp.
#[must_use]
pub fn raw_clock(timestamp: Option<&str>) -> (Clock, bool) {
    match timestamp.and_then(parse_source_time) {
        Some(ts) => (Clock::UnixMs(ts), false),
        None => (Clock::UnixMs(0), true),
    }
}

/// Build one raw `ImportOp` for a physical JSONL line, byte-exact.
///
/// The op is scoped to the owning thread session, chained to `prev_raw_id`
/// (the previous line's raw op, or the last line of a previous cursor batch),
/// and spills to blob storage through `blobs` when the line exceeds the inline
/// limit.
///
/// # Errors
///
/// Returns [`ImportError`] if the source position overflows or the blob sink
/// fails.
#[expect(
    clippy::too_many_arguments,
    reason = "raw ops need line bytes, stream position, identity lanes, scope context, and chain/blob sinks"
)]
pub fn build_raw_op(
    data: &[u8],
    hash: [u8; 32],
    stream: &SourceStream,
    seq: u64,
    thread: &str,
    session_id: SessionId,
    prev_raw_id: Option<OpId>,
    blobs: &mut dyn BlobSink,
) -> Result<Op, ImportError> {
    let op_id = stream.op_from_position(SourcePosition::raw(seq))?;
    let meta = parse_raw_line_meta(data);
    let (clock, source_time_unknown) = raw_clock(meta.timestamp.as_deref());
    let mut tags = Tags::IMPORT | raw_line_tags(&meta);
    if source_time_unknown {
        tags |= Tags::SOURCE_TIME_UNKNOWN;
    }
    let actor = derive_actor_id(&raw_actor_key(&meta.raw_type, thread));
    let parents = match prev_raw_id {
        Some(prev) => ParentSet::One(prev),
        None => ParentSet::None,
    };
    Ok(Op {
        id: op_id,
        parents,
        actor,
        clock,
        scope: ScopeRef::Session(session_id),
        tags,
        kind: OpKind::Import(ImportOp {
            raw_ref: payload_for(data, blobs)?,
            raw_hash: Some(hash),
        }),
    })
}

/// Which raw line a folded item's normalized ops anchor to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAnchor {
    /// Anchor at the item's first-seen ordinal (fresh item in this batch).
    FirstSeen,
    /// Anchor at the item's last-seen ordinal (deterministic update op for an
    /// item first seen before the cursor and changed after it).
    LastSeen,
}

/// Shared normalization context for one physical file.
///
/// Holds the deterministic source stream, the session scope identity, the
/// per-ordinal derived lane counters, and the blob sink so large content
/// fields spill exactly like the raw lane does.
pub struct NormalizeContext<'a> {
    /// Deterministic source stream of the physical file.
    pub stream: &'a SourceStream,
    /// Owning thread id (session scope identity).
    pub thread: &'a str,
    /// Session scope of the owning thread.
    pub session_id: SessionId,
    /// Per-ordinal derived lane counters. Fresh items, update ops, inter-agent
    /// notes, and compaction reflections share lanes at their anchor ordinals,
    /// so op ids never collide within a batch and stay deterministic.
    pub lanes: HashMap<u64, u16>,
    /// Last physical line ordinal of this batch (items anchored beyond it
    /// belong to a trailing partial line and emit on a later run).
    pub batch_end: u64,
    /// Whether private thinking content (reasoning) is materialized.
    pub include_thinking: bool,
    /// Blob sink for payloads above the inline limit.
    pub blobs: &'a mut dyn BlobSink,
}

impl std::fmt::Debug for NormalizeContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NormalizeContext")
            .field("stream", &self.stream)
            .field("thread", &self.thread)
            .field("session_id", &self.session_id)
            .field("lanes", &self.lanes)
            .field("batch_end", &self.batch_end)
            .field("include_thinking", &self.include_thinking)
            .field("blobs", &"<dyn BlobSink>")
            .finish()
    }
}

/// Build the normalized ops for one folded final item.
///
/// The mapping is source-neutral and reads bridge `camelCase` payload fields:
/// message text, reasoning summaries/raw content, command strings and
/// aggregated output, file diffs, tool arguments/results/errors, plan text,
/// and subagent activity identity. Unknown kinds produce no ops and stay
/// raw-only; reasoning summaries and raw chain-of-thought are private and only
/// materialize when `include_thinking` is set.
///
/// A lifecycle item that spans physical lines (e.g. a tool call or command
/// with both arguments and result/output) emits two ops when fresh: a
/// `Start` op anchored at the item's first-seen ordinal and a `Finish` op
/// anchored at its last-seen ordinal. Both take deterministic derived lanes
/// from [`NormalizeContext::lanes`] so ids never collide with sibling items or
/// with update ops at the same ordinals.
///
/// # Errors
///
/// Returns [`ImportError`] if a derived source position overflows (bounded by
/// per-file line counts in practice) or the blob sink rejects large content.
pub fn normalized_ops_for_item(
    item: &FinalItem,
    anchor: ItemAnchor,
    anchor_clock: Clock,
    last_seen_clock: Clock,
    ctx: &mut NormalizeContext<'_>,
) -> Result<Vec<Op>, ImportError> {
    if item.kind == ProjectionKind::Unknown {
        return Ok(Vec::new());
    }
    let kind_tag = item.payload.get("kind").and_then(Value::as_str);
    // Reasoning summaries and raw chain-of-thought are the private thinking
    // lane; only materialized on request.
    if !ctx.include_thinking && kind_tag == Some("reasoning") {
        return Ok(Vec::new());
    }
    // contextCompaction items carry identity only; the compaction message
    // arrives in the per-line `compacted` projection lane.
    if kind_tag == Some("contextCompaction") {
        return Ok(Vec::new());
    }

    let (actor_tag, actor_id) = codex_actor(item.actor.as_str(), ctx.thread);
    let kind_tag_flag = kind_tags(item.kind);
    // Per-turn items persist their turn identity in the op envelope: the scope
    // is the deterministic provider-neutral turn id (thread + bridge turn id).
    // Raw, inter-agent, and compaction lanes stay session-scoped — they are
    // thread-level, not turn-level.
    let scope = ScopeRef::Turn(derive_turn_id(&format!("{}:{}", ctx.thread, item.turn_id)));
    let anchor_ordinal = match anchor {
        ItemAnchor::FirstSeen => item.first_seen,
        ItemAnchor::LastSeen => item.last_seen,
    };
    let raw_op_id = ctx
        .stream
        .op_from_position(SourcePosition::raw(anchor_ordinal))?;
    let mut ops = Vec::new();

    match item.kind {
        ProjectionKind::Message => {
            let lane = take_lane(&mut ctx.lanes, anchor_ordinal)?;
            ops.push(Op {
                id: ctx
                    .stream
                    .op_from_position(SourcePosition::derived(anchor_ordinal, lane))?,
                parents: ParentSet::One(raw_op_id),
                actor: actor_id,
                clock: anchor_clock,
                scope,
                tags: actor_tag | kind_tag_flag,
                kind: OpKind::Message(MessageOp {
                    content: payload_for_value(item.payload.get("text"), ctx.blobs)?,
                    content_type: Payload::Inline(b"text/markdown".to_vec()),
                }),
            });
        }
        ProjectionKind::Tool => {
            let tool_call_id = payload_for_value(item.payload.get("id"), ctx.blobs)?;
            let tool_name = payload_for_value(item.payload.get("tool"), ctx.blobs)?;
            let status = item.payload.get("status").and_then(Value::as_str);
            let stage = tool_stage(status);
            let has_args = item.payload.get("arguments").is_some();
            let has_result =
                item.payload.get("result").is_some() || item.payload.get("errorMessage").is_some();
            // A fresh tool call spanning lines with both arguments and a
            // result splits into Start (args, first-seen) + Finish (result).
            let split = anchor == ItemAnchor::FirstSeen
                && item.last_seen > item.first_seen
                && item.last_seen <= ctx.batch_end
                && has_args
                && has_result;
            if split {
                let start_lane = take_lane(&mut ctx.lanes, item.first_seen)?;
                let finish_lane = take_lane(&mut ctx.lanes, item.last_seen)?;
                let start_raw = ctx
                    .stream
                    .op_from_position(SourcePosition::raw(item.first_seen))?;
                let finish_raw = ctx
                    .stream
                    .op_from_position(SourcePosition::raw(item.last_seen))?;
                ops.push(Op {
                    id: ctx
                        .stream
                        .op_from_position(SourcePosition::derived(item.first_seen, start_lane))?,
                    parents: ParentSet::One(start_raw),
                    actor: actor_id,
                    clock: anchor_clock,
                    scope,
                    tags: actor_tag | kind_tag_flag,
                    kind: OpKind::Tool(ToolOp {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        stage: ToolStage::Start,
                        content: tool_args_payload(&item.payload, ctx.blobs)?,
                    }),
                });
                ops.push(Op {
                    id: ctx
                        .stream
                        .op_from_position(SourcePosition::derived(item.last_seen, finish_lane))?,
                    parents: ParentSet::One(finish_raw),
                    actor: actor_id,
                    clock: last_seen_clock,
                    scope,
                    tags: actor_tag | kind_tag_flag,
                    kind: OpKind::Tool(ToolOp {
                        tool_call_id,
                        // The split lifecycle's Finish is the result row: an
                        // empty tool name matches the shared projection's
                        // tool-result shape, so the row previews the result
                        // content instead of repeating the tool name label.
                        tool_name: Payload::Empty,
                        stage: ToolStage::Finish,
                        content: tool_result_payload(&item.payload, ctx.blobs)?,
                    }),
                });
            } else {
                let lane = take_lane(&mut ctx.lanes, anchor_ordinal)?;
                ops.push(Op {
                    id: ctx
                        .stream
                        .op_from_position(SourcePosition::derived(anchor_ordinal, lane))?,
                    parents: ParentSet::One(raw_op_id),
                    actor: actor_id,
                    clock: anchor_clock,
                    scope,
                    tags: actor_tag | kind_tag_flag,
                    kind: OpKind::Tool(ToolOp {
                        tool_call_id,
                        tool_name,
                        stage,
                        content: tool_content_payload(&item.payload, ctx.blobs)?,
                    }),
                });
            }
        }
        ProjectionKind::Command => {
            let command_id = payload_for_value(item.payload.get("id"), ctx.blobs)?;
            let status = item.payload.get("status").and_then(Value::as_str);
            let stage = command_stage(status);
            let has_command = item
                .payload
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            let has_output = item
                .payload
                .get("aggregatedOutput")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            // A fresh command spanning lines with command + output splits into
            // Start (command, first-seen) + Finish (output, last-seen).
            let split = anchor == ItemAnchor::FirstSeen
                && item.last_seen > item.first_seen
                && item.last_seen <= ctx.batch_end
                && has_command
                && has_output;
            if split {
                let start_lane = take_lane(&mut ctx.lanes, item.first_seen)?;
                let finish_lane = take_lane(&mut ctx.lanes, item.last_seen)?;
                let start_raw = ctx
                    .stream
                    .op_from_position(SourcePosition::raw(item.first_seen))?;
                let finish_raw = ctx
                    .stream
                    .op_from_position(SourcePosition::raw(item.last_seen))?;
                ops.push(Op {
                    id: ctx
                        .stream
                        .op_from_position(SourcePosition::derived(item.first_seen, start_lane))?,
                    parents: ParentSet::One(start_raw),
                    actor: actor_id,
                    clock: anchor_clock,
                    scope,
                    tags: actor_tag | kind_tag_flag,
                    kind: OpKind::Command(CommandOp {
                        command_id: command_id.clone(),
                        content: payload_for_value(item.payload.get("command"), ctx.blobs)?,
                        stage: CommandStage::Start,
                    }),
                });
                ops.push(Op {
                    id: ctx
                        .stream
                        .op_from_position(SourcePosition::derived(item.last_seen, finish_lane))?,
                    parents: ParentSet::One(finish_raw),
                    actor: actor_id,
                    clock: last_seen_clock,
                    scope,
                    tags: actor_tag | kind_tag_flag,
                    kind: OpKind::Command(CommandOp {
                        command_id,
                        content: payload_for_value(
                            item.payload.get("aggregatedOutput"),
                            ctx.blobs,
                        )?,
                        stage: CommandStage::Finish,
                    }),
                });
            } else {
                let lane = take_lane(&mut ctx.lanes, anchor_ordinal)?;
                ops.push(Op {
                    id: ctx
                        .stream
                        .op_from_position(SourcePosition::derived(anchor_ordinal, lane))?,
                    parents: ParentSet::One(raw_op_id),
                    actor: actor_id,
                    clock: anchor_clock,
                    scope,
                    tags: actor_tag | kind_tag_flag,
                    kind: OpKind::Command(CommandOp {
                        command_id,
                        content: command_content_payload(&item.payload, ctx.blobs)?,
                        stage,
                    }),
                });
            }
        }
        ProjectionKind::File => {
            let default_stage = file_stage(item.payload.get("status").and_then(Value::as_str));
            for part in file_change_parts(&item.payload) {
                let lane = take_lane(&mut ctx.lanes, anchor_ordinal)?;
                let file_op_id = ctx
                    .stream
                    .op_from_position(SourcePosition::derived(anchor_ordinal, lane))?;
                ops.push(Op {
                    id: file_op_id,
                    parents: ParentSet::One(raw_op_id),
                    actor: actor_id,
                    clock: anchor_clock,
                    scope,
                    tags: actor_tag | kind_tag_flag,
                    kind: OpKind::File(FileOp {
                        path: derive_path_id(&part.path),
                        stage: if part.deleted {
                            FileStage::Deleted
                        } else {
                            default_stage
                        },
                        base: None,
                        after: None,
                        edit: file_edit(&part.diffs, ctx.blobs)?,
                    }),
                });
                // Persist each provider-neutral path as an explicit annotation
                // targeting its own file op. `FileOp` carries only `PathId`,
                // while one Codex fileChange item can contain several paths.
                let note_lane = take_lane(&mut ctx.lanes, anchor_ordinal)?;
                ops.push(Op {
                    id: ctx
                        .stream
                        .op_from_position(SourcePosition::derived(anchor_ordinal, note_lane))?,
                    parents: ParentSet::One(raw_op_id),
                    actor: actor_id,
                    clock: anchor_clock,
                    scope,
                    tags: Tags::NOTE | Tags::IMPORT,
                    kind: OpKind::Note(NoteOp {
                        target_ids: vec![file_op_id],
                        relationship: NoteRelationship::Explains,
                        content: payload_for(part.path.as_bytes(), ctx.blobs)?,
                    }),
                });
            }
        }
        ProjectionKind::Reflection => {
            let lane = take_lane(&mut ctx.lanes, anchor_ordinal)?;
            let private = kind_tag == Some("reasoning");
            let summary = match kind_tag {
                Some("reasoning") => joined_string_array(&item.payload, "summary", ctx.blobs)?,
                Some("plan") => payload_for_value(item.payload.get("text"), ctx.blobs)?,
                _ => Payload::Empty,
            };
            let anchors = if private {
                joined_string_array(&item.payload, "content", ctx.blobs)?
            } else {
                Payload::Empty
            };
            ops.push(Op {
                id: ctx
                    .stream
                    .op_from_position(SourcePosition::derived(anchor_ordinal, lane))?,
                parents: ParentSet::One(raw_op_id),
                actor: actor_id,
                clock: anchor_clock,
                scope,
                tags: if private {
                    Tags::PRIVATE | Tags::REFLECTION
                } else {
                    actor_tag | kind_tag_flag
                },
                kind: OpKind::Reflection(ReflectionOp {
                    scope,
                    covers: FrontierSet::new(),
                    window: WindowRef {
                        start_seq: 0,
                        end_seq: 0,
                    },
                    summary,
                    anchors,
                }),
            });
        }
        ProjectionKind::Note => {
            let lane = take_lane(&mut ctx.lanes, anchor_ordinal)?;
            let summary = subagent_activity_summary(&item.payload);
            let content = match &summary {
                Some(text) => payload_for(text.as_bytes(), ctx.blobs)?,
                None => Payload::Empty,
            };
            ops.push(Op {
                id: ctx
                    .stream
                    .op_from_position(SourcePosition::derived(anchor_ordinal, lane))?,
                parents: ParentSet::One(raw_op_id),
                actor: actor_id,
                clock: anchor_clock,
                scope,
                tags: actor_tag | kind_tag_flag,
                kind: OpKind::Note(NoteOp {
                    target_ids: Vec::new(),
                    relationship: NoteRelationship::Explains,
                    content,
                }),
            });
        }
        ProjectionKind::Unknown => {}
    }
    Ok(ops)
}

/// Build the normalized note op for one inter-agent communication line.
///
/// Inter-agent content is a source-neutral context note (subagent exchange),
/// anchored at the physical line that carried it with a deterministic derived
/// lane. Author/recipient labels are never used for scope or identity.
///
/// # Errors
///
/// Returns [`ImportError`] if the source position overflows or the blob sink
/// rejects the content.
pub fn normalized_ops_for_inter_agent(
    line: &InterAgentLine,
    clock: Clock,
    ctx: &mut NormalizeContext<'_>,
) -> Result<Vec<Op>, ImportError> {
    if line.content.is_empty() {
        return Ok(Vec::new());
    }
    let derived_lane = take_lane(&mut ctx.lanes, line.source_ordinal)?;
    let raw_op_id = ctx
        .stream
        .op_from_position(SourcePosition::raw(line.source_ordinal))?;
    let actor_id = derive_actor_id(&format!("system:{}", ctx.thread));
    let summary = inter_agent_summary(
        line.author.as_deref(),
        line.recipient.as_deref(),
        &line.content,
    );
    Ok(vec![Op {
        id: ctx
            .stream
            .op_from_position(SourcePosition::derived(line.source_ordinal, derived_lane))?,
        parents: ParentSet::One(raw_op_id),
        actor: actor_id,
        clock,
        scope: ScopeRef::Session(ctx.session_id),
        tags: Tags::NOTE | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::Explains,
            content: payload_for(summary.as_bytes(), ctx.blobs)?,
        }),
    }])
}

/// Build the normalized note op persisting one turn's identity and metadata.
///
/// The note is anchored at the turn's first-seen physical line with a
/// deterministic derived lane and is scoped to the turn's [`ScopeRef::Turn`]
/// identity; its content is a readable provider-neutral summary of the turn
/// lifecycle (id, status, item count). Turn metadata from `changedTurns` is
/// otherwise transient — this op records it explicitly.
///
/// # Errors
///
/// Returns [`ImportError`] if the source position overflows or the blob sink
/// rejects the summary.
pub fn normalized_ops_for_turn(
    turn: &TurnMeta,
    first_ordinal: u64,
    item_count: usize,
    clock: Clock,
    ctx: &mut NormalizeContext<'_>,
) -> Result<Vec<Op>, ImportError> {
    let derived_lane = take_lane(&mut ctx.lanes, first_ordinal)?;
    let raw_op_id = ctx
        .stream
        .op_from_position(SourcePosition::raw(first_ordinal))?;
    let actor_id = derive_actor_id(&format!("system:{}", ctx.thread));
    let summary = turn_summary(turn, item_count);
    Ok(vec![Op {
        id: ctx
            .stream
            .op_from_position(SourcePosition::derived(first_ordinal, derived_lane))?,
        parents: ParentSet::One(raw_op_id),
        actor: actor_id,
        clock,
        scope: ScopeRef::Turn(derive_turn_id(&format!("{}:{}", ctx.thread, turn.turn_id))),
        tags: Tags::NOTE | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::Explains,
            content: payload_for(summary.as_bytes(), ctx.blobs)?,
        }),
    }])
}

/// Build the normalized reflection op for one context-compaction line.
///
/// The compaction summary message is a non-private reflection anchored at the
/// physical line that carried it with a deterministic derived lane.
///
/// # Errors
///
/// Returns [`ImportError`] if the source position overflows or the blob sink
/// rejects the summary.
pub fn normalized_ops_for_compaction(
    line: &CompactedLine,
    clock: Clock,
    ctx: &mut NormalizeContext<'_>,
) -> Result<Vec<Op>, ImportError> {
    if line.message.is_empty() {
        return Ok(Vec::new());
    }
    let derived_lane = take_lane(&mut ctx.lanes, line.source_ordinal)?;
    let raw_op_id = ctx
        .stream
        .op_from_position(SourcePosition::raw(line.source_ordinal))?;
    let actor_id = derive_actor_id(&format!("system:{}", ctx.thread));
    Ok(vec![Op {
        id: ctx
            .stream
            .op_from_position(SourcePosition::derived(line.source_ordinal, derived_lane))?,
        parents: ParentSet::One(raw_op_id),
        actor: actor_id,
        clock,
        scope: ScopeRef::Session(ctx.session_id),
        tags: Tags::REFLECTION | Tags::IMPORT,
        kind: OpKind::Reflection(ReflectionOp {
            scope: ScopeRef::Session(ctx.session_id),
            covers: FrontierSet::new(),
            window: WindowRef {
                start_seq: 0,
                end_seq: 0,
            },
            summary: payload_for(line.message.as_bytes(), ctx.blobs)?,
            anchors: Payload::Empty,
        }),
    }])
}

/// Take the next derived lane at a physical ordinal, deterministically.
///
/// Lane counters are shared across every normalized op anchored at the same
/// ordinal within one batch (fresh items, update ops, per-line notes), so op
/// ids are unique and repeatable.
fn take_lane(lanes: &mut HashMap<u64, u16>, ordinal: u64) -> Result<u16, ImportError> {
    let lane = lanes.entry(ordinal).or_insert(0u16);
    *lane = lane.checked_add(1).ok_or(IdError::Overflow {
        record_ordinal: ordinal,
        derived_ordinal: *lane,
    })?;
    Ok(*lane)
}

/// Serialize a payload field value into a `Payload`, spilling large values.
///
/// Strings are kept as-is; any other JSON value is serialized to compact JSON
/// bytes; missing fields become an empty payload.
fn payload_for_value(
    value: Option<&Value>,
    blobs: &mut dyn BlobSink,
) -> Result<Payload, ImportError> {
    match value {
        Some(Value::String(s)) => payload_for(s.as_bytes(), blobs),
        Some(other) => payload_for(&serde_json::to_vec(other).unwrap_or_default(), blobs),
        None => Ok(Payload::Empty),
    }
}

/// Newline-join a bridge array-of-strings field into one payload.
fn joined_string_array(
    payload: &Value,
    field: &str,
    blobs: &mut dyn BlobSink,
) -> Result<Payload, ImportError> {
    let lines: Vec<&str> = payload
        .get(field)
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if lines.is_empty() {
        return Ok(Payload::Empty);
    }
    payload_for(lines.join("\n").as_bytes(), blobs)
}

/// Join non-empty byte parts with newlines into one payload.
fn join_payload(parts: &[Vec<u8>], blobs: &mut dyn BlobSink) -> Result<Payload, ImportError> {
    if parts.is_empty() {
        return Ok(Payload::Empty);
    }
    payload_for(&parts.join(&b"\n"[..]), blobs)
}

/// Tool op content for the combined single-op shape: arguments, prompt,
/// result output, and error message, newline-joined in that deterministic
/// order.
fn tool_content_payload(payload: &Value, blobs: &mut dyn BlobSink) -> Result<Payload, ImportError> {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    if let Some(args) = payload.get("arguments") {
        parts.push(serde_json::to_vec(args).unwrap_or_default());
    }
    if let Some(prompt) = payload
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        parts.push(prompt.as_bytes().to_vec());
    }
    if let Some(result) = payload.get("result") {
        parts.push(serde_json::to_vec(result).unwrap_or_default());
    }
    if let Some(err) = payload
        .get("errorMessage")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        parts.push(err.as_bytes().to_vec());
    }
    join_payload(&parts, blobs)
}

/// Tool `Start` op content: the serialized arguments only.
fn tool_args_payload(payload: &Value, blobs: &mut dyn BlobSink) -> Result<Payload, ImportError> {
    match payload.get("arguments") {
        Some(args) => payload_for(&serde_json::to_vec(args).unwrap_or_default(), blobs),
        None => Ok(Payload::Empty),
    }
}

/// Tool `Finish` op content: result output and error message, newline-joined.
fn tool_result_payload(payload: &Value, blobs: &mut dyn BlobSink) -> Result<Payload, ImportError> {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    if let Some(result) = payload.get("result") {
        parts.push(serde_json::to_vec(result).unwrap_or_default());
    }
    if let Some(err) = payload
        .get("errorMessage")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        parts.push(err.as_bytes().to_vec());
    }
    join_payload(&parts, blobs)
}

/// Command op content: command string then aggregated output, newline-joined.
fn command_content_payload(
    payload: &Value,
    blobs: &mut dyn BlobSink,
) -> Result<Payload, ImportError> {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    if let Some(cmd) = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        parts.push(cmd.as_bytes().to_vec());
    }
    if let Some(output) = payload
        .get("aggregatedOutput")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        parts.push(output.as_bytes().to_vec());
    }
    join_payload(&parts, blobs)
}

/// One path-specific projection retained from a Codex `fileChange` item.
#[derive(Debug)]
struct FileChangePart {
    path: String,
    diffs: Vec<String>,
    deleted: bool,
}

/// Preserve every changed path instead of collapsing `changes[]` onto its
/// first entry. Repeated entries for one path are folded in source order.
fn file_change_parts(payload: &Value) -> Vec<FileChangePart> {
    let mut parts: Vec<FileChangePart> = Vec::new();
    if let Some(changes) = payload.get("changes").and_then(Value::as_array) {
        for change in changes {
            let Some(path) = change
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
            else {
                continue;
            };
            let index = parts
                .iter()
                .position(|part| part.path == path)
                .unwrap_or_else(|| {
                    parts.push(FileChangePart {
                        path: path.to_string(),
                        diffs: Vec::new(),
                        deleted: false,
                    });
                    parts.len().saturating_sub(1)
                });
            let Some(part) = parts.get_mut(index) else {
                continue;
            };
            if let Some(diff) = change
                .get("diff")
                .and_then(Value::as_str)
                .filter(|diff| !diff.is_empty())
            {
                part.diffs.push(diff.to_string());
            }
            part.deleted |= change
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "delete" | "deleted" | "remove" | "removed"));
        }
    }
    if parts.is_empty() {
        if let Some(path) = payload
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
        {
            parts.push(FileChangePart {
                path: path.to_string(),
                diffs: Vec::new(),
                deleted: false,
            });
        }
    }
    parts
}

/// File edit payload for one path's unified diff text, if present.
fn file_edit(diffs: &[String], blobs: &mut dyn BlobSink) -> Result<FileEdit, ImportError> {
    if diffs.is_empty() {
        return Ok(FileEdit::None);
    }
    Ok(FileEdit::UnifiedDiff(payload_for(
        diffs.join("\n").as_bytes(),
        blobs,
    )?))
}

/// Deterministic provider-neutral summary for a subagent activity item.
///
/// Renders the lifecycle event as readable prose. The real Codex protocol
/// [`SubAgentActivityKind`] has exactly three values — `started`, `interacted`,
/// `interrupted` — so `started` reads as "spawned subagent …" and every other
/// kind is rendered truthfully as `subagent <thread>: <kind>` (never invented
/// completion prose).
#[must_use]
pub fn subagent_activity_summary(payload: &Value) -> Option<String> {
    let kind = payload
        .get("activityKind")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let thread = payload
        .get("agentThreadId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let path = payload
        .get("agentPath")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (kind, thread) {
        (Some(kind), Some(thread)) if kind.eq_ignore_ascii_case("started") => Some(match path {
            Some(path) => format!("spawned subagent {thread} (path {path})"),
            None => format!("spawned subagent {thread}"),
        }),
        (Some(kind), Some(thread)) => Some(match path {
            Some(path) => format!("subagent {thread}: {kind} (path {path})"),
            None => format!("subagent {thread}: {kind}"),
        }),
        (Some(kind), None) => Some(format!("subagent activity: {kind}")),
        (None, Some(thread)) => Some(match path {
            Some(path) => format!("subagent {thread} (path {path})"),
            None => format!("subagent {thread}"),
        }),
        (None, None) => None,
    }
}

/// Completed agent paths from a legacy `list_agents` collaboration tool call.
///
/// Pre-R2 Codex corpora expose subagent completion only through the
/// `collaboration.list_agents` tool output: a JSON-encoded string such as
/// `{"agents":[{"agent_name":"/root/x","agent_status":{"completed":"..."}}]}`.
/// This extracts the `agent_name` of every agent whose `agent_status` object
/// carries a `completed` key (presence is the explicit completion signal; the
/// value is the provider's message and is deliberately ignored here).
///
/// The output may arrive as a plain JSON string (`Value::String`), as the raw
/// text, or as an array of content items each carrying a `text`/`output`
/// string field. Missing or non-object `agents`/`agent_status` shapes yield no
/// evidence — nothing is inferred.
#[must_use]
pub fn completed_agent_paths_from_tool(payload: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    collect_result_texts(payload.get("result"), &mut texts);
    let mut paths = Vec::new();
    for text in texts {
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(agents) = value.get("agents").and_then(Value::as_array) else {
            continue;
        };
        for agent in agents {
            let Some(name) = agent
                .get("agent_name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(status) = agent.get("agent_status").and_then(Value::as_object) else {
                continue;
            };
            if status.contains_key("completed") {
                paths.push(name.to_string());
            }
        }
    }
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// Collect candidate JSON-text values from a tool `result` field.
///
/// Accepts a plain JSON string, an array of content items (`text`/`output`
/// string fields), or nested arrays of them; anything else contributes nothing.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "unknown JSON result shapes are deliberately ignored (forward-compatible)"
)]
fn collect_result_texts(result: Option<&Value>, out: &mut Vec<String>) {
    let Some(result) = result else {
        return;
    };
    match result {
        Value::String(text) if !text.is_empty() => out.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::String(text) if !text.is_empty() => out.push(text.clone()),
                    Value::Object(obj) => {
                        if let Some(text) = obj
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            out.push(text.to_string());
                        } else if let Some(text) = obj
                            .get("output")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            out.push(text.to_string());
                        }
                    }
                    Value::Array(_) => collect_result_texts(Some(item), out),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Deterministic provider-neutral summary for an inter-agent line.
///
/// The author/recipient labels are diagnostic context for the message content;
/// they are never used for scope or identity.
#[must_use]
pub fn inter_agent_summary(author: Option<&str>, recipient: Option<&str>, content: &str) -> String {
    match (author, recipient) {
        (Some(author), Some(recipient)) => format!("{author} → {recipient}: {content}"),
        (Some(author), None) => format!("{author}: {content}"),
        (None, Some(recipient)) => format!("→ {recipient}: {content}"),
        (None, None) => content.to_string(),
    }
}

/// Deterministic summary for a turn metadata record.
#[must_use]
pub fn turn_summary(turn: &TurnMeta, item_count: usize) -> String {
    match (&turn.status, item_count) {
        (Some(status), 0) => format!("{}: {status}", turn.turn_id),
        (Some(status), count) => format!("{}: {status} ({count} items)", turn.turn_id),
        (None, 0) => turn.turn_id.clone(),
        (None, count) => format!("{}: {count} items", turn.turn_id),
    }
}

/// Actor tag and id for a source-neutral actor label.
#[must_use]
fn codex_actor(actor: &str, thread: &str) -> (Tags, ActorId) {
    match actor {
        "user" => (Tags::HUMAN, derive_actor_id(&format!("human:{thread}"))),
        "assistant" => (Tags::AGENT, derive_actor_id(&format!("model:{thread}"))),
        "system" => (Tags::IMPORT, derive_actor_id(&format!("system:{thread}"))),
        other => (Tags::IMPORT, derive_actor_id(&format!("{other}:{thread}"))),
    }
}

/// Kind tag for a projection kind.
#[must_use]
fn kind_tags(kind: ProjectionKind) -> Tags {
    match kind {
        ProjectionKind::Message => Tags::MESSAGE,
        ProjectionKind::Tool => Tags::TOOL,
        ProjectionKind::Command => Tags::COMMAND,
        ProjectionKind::File => Tags::FILE,
        ProjectionKind::Reflection => Tags::REFLECTION,
        ProjectionKind::Note => Tags::NOTE,
        ProjectionKind::Unknown => Tags::IMPORT,
    }
}

/// Map a bridge tool status to a tool stage (defaults to `Finish`).
#[must_use]
fn tool_stage(status: Option<&str>) -> ToolStage {
    match status {
        Some("running" | "in_progress" | "pending" | "inProgress") => ToolStage::Start,
        Some("streaming_delta" | "delta") => ToolStage::Delta,
        _ => ToolStage::Finish,
    }
}

/// Map a bridge command status to a command stage (defaults to `Finish`).
#[must_use]
fn command_stage(status: Option<&str>) -> CommandStage {
    match status {
        Some("running" | "in_progress" | "pending" | "inProgress") => CommandStage::Start,
        Some("streaming_delta" | "output") => CommandStage::Output,
        _ => CommandStage::Finish,
    }
}

/// Map a bridge file-change status to a file stage (defaults to `Observed`).
#[must_use]
fn file_stage(status: Option<&str>) -> FileStage {
    match status {
        Some("proposed") => FileStage::Proposed,
        Some("applied" | "completed") => FileStage::Applied,
        Some("saved") => FileStage::Saved,
        Some("deleted") => FileStage::Deleted,
        _ => FileStage::Observed,
    }
}
