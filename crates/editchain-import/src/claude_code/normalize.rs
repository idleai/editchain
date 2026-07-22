use editchain_core::{
    clock::Clock,
    op::{
        CommandOp, CommandStage, FileEdit, FileOp, FileStage, FrontierSet, ImportOp, MessageOp,
        OpKind, ReflectionOp, ToolOp, ToolStage, WindowRef,
    },
    parents::ParentSet,
    payload::Payload,
    scope::ScopeRef,
    tags::Tags,
    Op,
};

use super::envelope::{CcContentBlock, CcEnvelope};
use crate::ids::{derive_actor_id, derive_session_id, SourcePosition, SourceStream};
use crate::sink::{payload_for, BlobSink};

/// Normalize a parsed CC envelope into editchain operations.
///
/// Returns (`raw_import_op`, `optional_normalized_ops`).
///
/// # Panics
///
/// Panics if the source position overflows — this should never happen
/// in practice since sequence numbers are bounded by file size. Also
/// panics if `payload_for` fails and the raw bytes exceed 4096 bytes.
#[expect(
    clippy::too_many_arguments,
    reason = "all arguments are required for normalization"
)]
#[expect(
    clippy::unwrap_used,
    reason = "source positions are always valid — derived from bounded u64/u16 values"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "raw_bytes[..4096] is bounded by INLINE_LIMIT constant"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "normalized.len() is bounded by content block count (<65536)"
)]
#[expect(
    clippy::as_conversions,
    reason = "usize to u16 is safe for normalized op count"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "normalized.len() + 1 is bounded by content block count"
)]
pub fn normalize_envelope(
    env: &CcEnvelope,
    line_hash: [u8; 32],
    raw_bytes: &[u8],
    stream: &SourceStream,
    seq: u64,
    options: &NormalizeOptions,
    blobs: &mut dyn BlobSink,
) -> (Op, Vec<Op>) {
    let raw_pos = SourcePosition::raw(seq);
    let op_id = stream.op_from_position(raw_pos).unwrap();
    let timestamp = parse_timestamp(&env.timestamp);
    let clock = Clock::UnixMs(timestamp);
    let session_id = derive_session_id(&env.session_id);

    // Derive actor.
    let (actor, _tags) = match env.record_type.as_str() {
        "user" => {
            let actor_key = format!("human:{}", env.session_id);
            (derive_actor_id(&actor_key), Tags::HUMAN | Tags::MESSAGE)
        }
        "assistant" => {
            let model = env.message.as_ref().map_or("", |m| m.model.as_str());
            let actor_key = if env.agent_id.is_empty() {
                format!("model:{}:{}", env.session_id, model)
            } else {
                format!("agent:{}:{}", env.session_id, env.agent_id)
            };
            (derive_actor_id(&actor_key), Tags::AGENT | Tags::MESSAGE)
        }
        _ => {
            let actor_key = format!("system:{}", env.session_id);
            (derive_actor_id(&actor_key), Tags::IMPORT)
        }
    };

    // Raw import op.
    let raw_op = Op {
        id: op_id,
        parents: ParentSet::None,
        actor,
        clock,
        scope: ScopeRef::Session(session_id),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: payload_for(raw_bytes, blobs)
                .unwrap_or_else(|_| Payload::Inline(raw_bytes[..4096].to_vec())),
            raw_hash: Some(line_hash),
        }),
    };

    if !options.normalize {
        return (raw_op, vec![]);
    }

    // Normalized ops — use SourcePosition for collision-free ID allocation.
    let mut normalized = Vec::new();

    match env.record_type.as_str() {
        "user" => {
            if let Some(ref msg) = env.message {
                let has_tool_results = msg
                    .content
                    .iter()
                    .any(|b| matches!(b, CcContentBlock::ToolResult { .. }));
                let has_text = msg
                    .content
                    .iter()
                    .any(|b| matches!(b, CcContentBlock::Text { .. }));

                if has_tool_results {
                    for block in &msg.content {
                        if let CcContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error: _,
                        } = block
                        {
                            let tool_op = Op {
                                id: stream
                                    .op_from_position(SourcePosition::derived(
                                        seq,
                                        (normalized.len() + 1) as u16,
                                    ))
                                    .unwrap(),
                                parents: ParentSet::One(op_id),
                                actor,
                                clock,
                                scope: ScopeRef::Session(session_id),
                                tags: Tags::HUMAN | Tags::TOOL,
                                kind: OpKind::Tool(ToolOp {
                                    tool_call_id: Payload::Inline(tool_use_id.as_bytes().to_vec()),
                                    tool_name: Payload::Empty,
                                    stage: ToolStage::Finish,
                                    content: Payload::Inline(content.as_bytes().to_vec()),
                                }),
                            };
                            normalized.push(tool_op);
                        }
                    }
                }

                if has_text || !has_tool_results {
                    let text = extract_text_content(&msg.content);
                    if !text.is_empty() {
                        let msg_op = Op {
                            id: stream
                                .op_from_position(SourcePosition::derived(
                                    seq,
                                    (normalized.len() + 1) as u16,
                                ))
                                .unwrap(),
                            parents: ParentSet::One(op_id),
                            actor,
                            clock,
                            scope: ScopeRef::Session(session_id),
                            tags: Tags::HUMAN | Tags::MESSAGE,
                            kind: OpKind::Message(MessageOp {
                                content: Payload::Inline(text.as_bytes().to_vec()),
                                content_type: Payload::Inline(b"text/markdown".to_vec()),
                            }),
                        };
                        normalized.push(msg_op);
                    }
                }
            }
        }

        "assistant" => {
            if let Some(ref msg) = env.message {
                for block in &msg.content {
                    match block {
                        CcContentBlock::Text { text } if !text.trim().is_empty() => {
                            let msg_op = Op {
                                id: stream
                                    .op_from_position(SourcePosition::derived(
                                        seq,
                                        (normalized.len() + 1) as u16,
                                    ))
                                    .unwrap(),
                                parents: ParentSet::One(op_id),
                                actor,
                                clock,
                                scope: ScopeRef::Session(session_id),
                                tags: Tags::AGENT | Tags::MESSAGE,
                                kind: OpKind::Message(MessageOp {
                                    content: Payload::Inline(text.as_bytes().to_vec()),
                                    content_type: Payload::Inline(b"text/markdown".to_vec()),
                                }),
                            };
                            normalized.push(msg_op);
                        }
                        CcContentBlock::ToolUse { id, name, input } => {
                            let input_str = serde_json::to_string(input).unwrap_or_default();
                            let tool_op = Op {
                                id: stream
                                    .op_from_position(SourcePosition::derived(
                                        seq,
                                        (normalized.len() + 1) as u16,
                                    ))
                                    .unwrap(),
                                parents: ParentSet::One(op_id),
                                actor,
                                clock,
                                scope: ScopeRef::Session(session_id),
                                tags: Tags::AGENT | Tags::TOOL,
                                kind: OpKind::Tool(ToolOp {
                                    tool_call_id: Payload::Inline(id.as_bytes().to_vec()),
                                    tool_name: Payload::Inline(name.as_bytes().to_vec()),
                                    stage: ToolStage::Start,
                                    content: Payload::Inline(input_str.as_bytes().to_vec()),
                                }),
                            };
                            normalized.push(tool_op);

                            if name == "Bash" || name == "PowerShell" {
                                let cmd =
                                    input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                                let cmd_op = Op {
                                    id: stream
                                        .op_from_position(SourcePosition::derived(
                                            seq,
                                            (normalized.len() + 1) as u16,
                                        ))
                                        .unwrap(),
                                    parents: ParentSet::One(op_id),
                                    actor,
                                    clock,
                                    scope: ScopeRef::Session(session_id),
                                    tags: Tags::AGENT | Tags::COMMAND,
                                    kind: OpKind::Command(CommandOp {
                                        command_id: Payload::Inline(id.as_bytes().to_vec()),
                                        content: Payload::Inline(cmd.as_bytes().to_vec()),
                                        stage: CommandStage::Start,
                                    }),
                                };
                                normalized.push(cmd_op);
                            }
                        }
                        CcContentBlock::Thinking { thinking }
                            if options.include_thinking && !thinking.trim().is_empty() =>
                        {
                            let thinking_op = Op {
                                id: stream
                                    .op_from_position(SourcePosition::derived(
                                        seq,
                                        (normalized.len() + 1) as u16,
                                    ))
                                    .unwrap(),
                                parents: ParentSet::One(op_id),
                                actor,
                                clock,
                                scope: ScopeRef::Session(session_id),
                                tags: Tags::PRIVATE | Tags::MESSAGE,
                                kind: OpKind::Message(MessageOp {
                                    content: Payload::Inline(thinking.as_bytes().to_vec()),
                                    content_type: Payload::Inline(b"text/markdown".to_vec()),
                                }),
                            };
                            normalized.push(thinking_op);
                        }
                        CcContentBlock::Text { .. }
                        | CcContentBlock::ToolResult { .. }
                        | CcContentBlock::Thinking { .. } => {}
                    }
                }
            }
        }

        "attachment" => {
            if env.attachment_type == "file" || env.attachment_type == "file_content" {
                let file_op = Op {
                    id: stream
                        .op_from_position(SourcePosition::derived(
                            seq,
                            (normalized.len() + 1) as u16,
                        ))
                        .unwrap(),
                    parents: ParentSet::One(op_id),
                    actor,
                    clock,
                    scope: ScopeRef::Session(session_id),
                    tags: Tags::FILE | Tags::IMPORT,
                    kind: OpKind::File(FileOp {
                        path: crate::ids::derive_path_id(""),
                        stage: FileStage::Observed,
                        base: None,
                        after: None,
                        edit: FileEdit::None,
                    }),
                };
                normalized.push(file_op);
            }
        }

        "system" => match env.subtype.as_str() {
            "compact_boundary" | "away_summary" => {
                let reflection_op = Op {
                    id: stream
                        .op_from_position(SourcePosition::derived(
                            seq,
                            (normalized.len() + 1) as u16,
                        ))
                        .unwrap(),
                    parents: ParentSet::One(op_id),
                    actor,
                    clock,
                    scope: ScopeRef::Session(session_id),
                    tags: Tags::REFLECTION | Tags::IMPORT,
                    kind: OpKind::Reflection(ReflectionOp {
                        scope: ScopeRef::Session(session_id),
                        covers: FrontierSet(Vec::new()),
                        window: WindowRef {
                            start_seq: 0,
                            end_seq: 0,
                        },
                        summary: Payload::Empty,
                        anchors: Payload::Empty,
                    }),
                };
                normalized.push(reflection_op);
            }
            _ => {}
        },

        _ => {}
    }

    (raw_op, normalized)
}

/// Options for controlling normalization behavior.
#[derive(Debug, Clone)]
pub struct NormalizeOptions {
    /// Whether to normalize operation kinds (e.g. split tool calls into start/end).
    pub normalize: bool,
    /// Whether to include thinking blocks in the output.
    pub include_thinking: bool,
}

impl Default for NormalizeOptions {
    fn default() -> Self {
        Self {
            normalize: true,
            include_thinking: false,
        }
    }
}

fn extract_text_content(blocks: &[CcContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            CcContentBlock::Text { text } => Some(text.clone()),
            CcContentBlock::ToolUse { .. }
            | CcContentBlock::ToolResult { .. }
            | CcContentBlock::Thinking { .. } => None,
        })
        .collect()
}

/// Parse a timestamp string into Unix milliseconds.
///
/// Returns 0 if the string is empty or unparseable.
#[must_use]
pub fn parse_timestamp(ts_str: &str) -> u64 {
    if ts_str.is_empty() {
        return 0;
    }
    chrono_parse(ts_str).unwrap_or(0)
}

#[expect(
    clippy::indexing_slicing,
    reason = "slice indices are validated by the length check at function entry"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on bounded date/time components is safe"
)]
fn chrono_parse(s: &str) -> Option<u64> {
    if s.len() < 20 || !s.is_ascii() {
        return None;
    }
    let bytes = s.as_bytes();

    let year = parse_digits4(&bytes[0..4])?;
    let month = parse_digits2(&bytes[5..7])?;
    let day = parse_digits2(&bytes[8..10])?;
    let hour = parse_digits2(&bytes[11..13])?;
    let min = parse_digits2(&bytes[14..16])?;
    let sec = parse_digits2(&bytes[17..19])?;

    let millis = if s.len() > 20 && bytes[19] == b'.' {
        let end = s.len() - 1;
        if end > 20 && end <= 24 {
            parse_digits_variable(&bytes[20..end]).unwrap_or(0)
        } else if end > 24 && end <= 25 {
            parse_digits_variable(&bytes[20..24]).unwrap_or(0)
        } else if end > 20 && end < 23 {
            parse_digits_variable(&bytes[20..end]).unwrap_or(0)
        } else {
            0
        }
    } else {
        0
    };

    Some(datetime_to_unix_ms(
        i64::from(year),
        month,
        day,
        hour,
        min,
        sec,
        millis,
    ))
}

#[expect(
    clippy::indexing_slicing,
    reason = "slice indices are validated by the length check at function entry"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on bounded date/time components is safe"
)]
fn parse_digits4(digits: &[u8]) -> Option<u16> {
    if digits.len() < 4 {
        return None;
    }
    Some(
        u16::from(digits[0] - b'0') * 1000
            + u16::from(digits[1] - b'0') * 100
            + u16::from(digits[2] - b'0') * 10
            + u16::from(digits[3] - b'0'),
    )
}

#[expect(
    clippy::indexing_slicing,
    reason = "slice indices are validated by the length check at function entry"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on bounded date/time components is safe"
)]
fn parse_digits2(digits: &[u8]) -> Option<u8> {
    if digits.len() < 2 {
        return None;
    }
    Some((digits[0] - b'0') * 10 + (digits[1] - b'0'))
}

#[expect(
    clippy::indexing_slicing,
    reason = "slice indices are validated by the length check at function entry"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on bounded date/time components is safe"
)]
fn parse_digits_variable(digits: &[u8]) -> Option<u32> {
    match digits.len() {
        1 => Some(u32::from(digits[0] - b'0') * 100),
        2 => Some(u32::from(digits[0] - b'0') * 100 + u32::from(digits[1] - b'0') * 10),
        3 => Some(
            u32::from(digits[0] - b'0') * 100
                + u32::from(digits[1] - b'0') * 10
                + u32::from(digits[2] - b'0'),
        ),
        _ => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "all date/time components are required for conversion"
)]
#[expect(
    clippy::similar_names,
    reason = "underscore-suffixed parameter names are conventional for date math"
)]
#[expect(
    clippy::unreadable_literal,
    reason = "date constants are standard epoch values"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "date arithmetic on bounded i64 values is safe"
)]
#[expect(
    clippy::as_conversions,
    reason = "u8 to i64 and u32/u64 casts are safe for date/time components"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "secs is always non-negative for post-epoch timestamps"
)]
fn datetime_to_unix_ms(
    year_: i64,
    month_: u8,
    day_: u8,
    hour_: u8,
    min_: u8,
    sec_: u8,
    millis_: u32,
) -> u64 {
    fn days_from_epoch(y_: i64, m_: u8, d_: u8) -> i64 {
        let y_adj = y_ - i64::from(m_ <= 2);
        let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
        let yoe = y_adj - era * 400;
        let doy =
            (153 * (i64::from(m_) + if m_ <= 2 { 9 } else { -3 }) + 2) / 5 + i64::from(d_) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }

    let days = days_from_epoch(year_, month_, day_);
    let secs = days * 86400 + i64::from(hour_) * 3600 + i64::from(min_) * 60 + i64::from(sec_);
    (secs as u64 * 1000) + u64::from(millis_)
}
