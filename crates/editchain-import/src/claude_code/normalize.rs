use editchain_core::{
    clock::Clock,
    op::*,
    parents::ParentSet,
    payload::Payload,
    scope::ScopeRef,
    tags::Tags,
    Op,
};

use super::envelope::{CcContentBlock, CcEnvelope};
use crate::ids::{derive_actor_id, derive_session_id, SourceStream};

/// Normalize a parsed CC envelope into editchain operations.
///
/// Returns (raw_import_op, optional_normalized_ops).
pub fn normalize_envelope(
    env: &CcEnvelope,
    line_hash: [u8; 32],
    raw_bytes: &[u8],
    stream: &SourceStream,
    seq: u64,
    options: &NormalizeOptions,
) -> (Op, Vec<Op>) {
    let op_id = stream.op_id(seq);
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
            let model = env.message.as_ref().map(|m| m.model.as_str()).unwrap_or("");
            let actor_key = if !env.agent_id.is_empty() {
                format!("agent:{}:{}", env.session_id, env.agent_id)
            } else {
                format!("model:{}:{}", env.session_id, model)
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
            raw_ref: if raw_bytes.len() <= 4096 {
                Payload::Inline(raw_bytes.to_vec())
            } else {
                Payload::Inline(raw_bytes[..4096].to_vec())
            },
            raw_hash: Some(line_hash),
        }),
    };

    if !options.normalize {
        return (raw_op, vec![]);
    }

    // Normalized ops.
    let mut normalized = Vec::new();
    let norm_seq = seq * 1000 + 1;

    match env.record_type.as_str() {
        "user" => {
            if let Some(ref msg) = env.message {
                let has_tool_results = msg.content.iter().any(|b| matches!(b, CcContentBlock::ToolResult { .. }));
                let has_text = msg.content.iter().any(|b| matches!(b, CcContentBlock::Text { .. }));

                if has_tool_results {
                    for block in &msg.content {
                        if let CcContentBlock::ToolResult { tool_use_id, content, is_error: _ } = block {
                            let tool_op = Op {
                                id: stream.op_id(norm_seq + normalized.len() as u64),
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
                            id: stream.op_id(norm_seq + normalized.len() as u64),
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
                                id: stream.op_id(norm_seq + normalized.len() as u64),
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
                                id: stream.op_id(norm_seq + normalized.len() as u64),
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
                                let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                                let cmd_op = Op {
                                    id: stream.op_id(norm_seq + normalized.len() as u64),
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
                                id: stream.op_id(norm_seq + normalized.len() as u64),
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
                        _ => {}
                    }
                }
            }
        }

        "attachment" => {
            if env.attachment_type == "file" || env.attachment_type == "file_content" {
                let file_op = Op {
                    id: stream.op_id(norm_seq + normalized.len() as u64),
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
                    id: stream.op_id(norm_seq + normalized.len() as u64),
                    parents: ParentSet::One(op_id),
                    actor,
                    clock,
                    scope: ScopeRef::Session(session_id),
                    tags: Tags::REFLECTION | Tags::IMPORT,
                    kind: OpKind::Reflection(ReflectionOp {
                        scope: ScopeRef::Session(session_id),
                        covers: FrontierSet(Vec::new()),
                        window: WindowRef { start_seq: 0, end_seq: 0 },
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

#[derive(Debug, Clone)]
pub struct NormalizeOptions {
    pub normalize: bool,
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
            _ => None,
        })
        .collect()
}

fn parse_timestamp(ts_str: &str) -> u64 {
    if ts_str.is_empty() {
        return 0;
    }
    chrono_parse(ts_str).unwrap_or(0)
}

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
        year as i64, month as u8, day as u8, hour as u8, min as u8, sec as u8, millis,
    ))
}

fn parse_digits4(digits: &[u8]) -> Option<u16> {
    if digits.len() < 4 { return None; }
    Some(
        ((digits[0] - b'0') as u16) * 1000
            + ((digits[1] - b'0') as u16) * 100
            + ((digits[2] - b'0') as u16) * 10
            + ((digits[3] - b'0') as u16),
    )
}

fn parse_digits2(digits: &[u8]) -> Option<u8> {
    if digits.len() < 2 { return None; }
    Some(((digits[0] - b'0') * 10 + (digits[1] - b'0')) as u8)
}

fn parse_digits_variable(digits: &[u8]) -> Option<u32> {
    match digits.len() {
        1 => Some(((digits[0] - b'0') as u32) * 100),
        2 => Some(((digits[0] - b'0') as u32) * 100 + ((digits[1] - b'0') as u32) * 10),
        3 => Some(
            ((digits[0] - b'0') as u32) * 100
                + ((digits[1] - b'0') as u32) * 10
                + ((digits[2] - b'0') as u32),
        ),
        _ => None,
    }
}

fn datetime_to_unix_ms(year_: i64, month_: u8, day_: u8, hour_: u8, min_: u8, sec_: u8, millis_: u32) -> u64 {
    fn days_from_epoch(y_: i64, m_: u8, d_: u8) -> i64 {
        let y_adj = y_ - if m_ <= 2 { 1 } else { 0 };
        let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
        let yoe = y_adj - era * 400;
        let doy = (153 * (m_ as i64 + if m_ <= 2 { 9 } else { -3 }) + 2) / 5 + d_ as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }

    let days = days_from_epoch(year_, month_, day_);
    let secs = days * 86400 + hour_ as i64 * 3600 + min_ as i64 * 60 + sec_ as i64;
    (secs as u64 * 1000) + millis_ as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_parsing() {
        let ts = parse_timestamp("2026-07-09T18:56:19.739Z");
        assert!(ts > 1700000000000);
        assert!(ts < 1800000000000);
    }

    #[test]
    fn test_timestamp_no_millis() {
        let ts = parse_timestamp("2026-07-09T18:56:19Z");
        assert!(ts > 1700000000000);
    }

    #[test]
    fn test_empty_timestamp() {
        assert_eq!(parse_timestamp(""), 0);
    }

    #[test]
    fn test_normalize_user_message() {
        use super::super::envelope::{parse_envelope};

        let json = br#"{"type":"user","uuid":"abc","sessionId":"sess-1","timestamp":"2026-07-09T18:56:19.739Z","message":{"role":"user","content":"hello world"}}"#;
        let env = parse_envelope(json).unwrap();
        let stream = SourceStream::new(crate::ids::derive_node_id("/test"), 0);
        let hash = crate::ids::hash_raw(json);

        let (raw, norm) =
            normalize_envelope(&env, hash, json, &stream, 1, &NormalizeOptions::default());

        assert!(matches!(raw.kind, OpKind::Import(_)));
        assert!(raw.tags.matches_any(Tags::IMPORT));

        assert_eq!(norm.len(), 1);

        match &norm[0].kind {
            OpKind::Message(msg) => match &msg.content {
                Payload::Inline(bytes) => assert_eq!(bytes.as_slice(), b"hello world"),
                _ => panic!("expected inline payload"),
            },
            _ => panic!("expected MessageOp"),
        }
    }
}
