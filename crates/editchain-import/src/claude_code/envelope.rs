use serde_json::Value;

/// A loose envelope around a Claude Code JSONL record.
///
/// Only captures fields needed for import routing.
/// Unknown fields are silently ignored (forward-compat).
#[derive(Debug, Clone, Default)]
pub struct CcEnvelope {
    pub record_type: String,
    pub uuid: String,
    pub parent_uuid: String,
    pub timestamp: String,
    pub session_id: String,
    pub message: Option<CcMessage>,
    pub is_sidechain: bool,
    pub is_meta: bool,
    pub cwd: String,
    pub git_branch: String,
    pub permission_mode: String,
    pub agent_name: String,
    pub agent_id: String,
    pub user_type: String,
    pub version: String,
    pub entrypoint: String,
    pub session_kind: String,
    pub subtype: String,
    pub leaf_uuid: String,
    pub is_compact_summary: bool,
    pub logical_parent_uuid: String,
    pub attachment_type: String,
}

/// A message within a Claude Code record.
#[derive(Debug, Clone)]
pub struct CcMessage {
    pub role: String,
    pub model: String,
    pub stop_reason: Option<String>,
    pub content: Vec<CcContentBlock>,
}

/// A content block within a message.
#[derive(Debug, Clone)]
pub enum CcContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    Thinking { thinking: String },
}

/// Parse a single JSONL line into a CcEnvelope.
///
/// Returns None for malformed lines or lines without a type field.
pub fn parse_envelope(line: &[u8]) -> Option<CcEnvelope> {
    let raw: Value = serde_json::from_slice(line).ok()?;

    let record_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if record_type.is_empty() {
        return None;
    }

    let env = CcEnvelope {
        record_type: record_type.to_string(),
        uuid: raw.get("uuid").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        parent_uuid: raw.get("parentUuid").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        timestamp: raw.get("timestamp").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        session_id: raw.get("sessionId").or_else(|| raw.get("session_id")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        message: parse_message(raw.get("message")),
        is_sidechain: raw.get("isSidechain").and_then(|v| v.as_bool()).unwrap_or(false),
        is_meta: raw.get("isMeta").and_then(|v| v.as_bool()).unwrap_or(false),
        cwd: raw.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        git_branch: raw.get("gitBranch").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        permission_mode: raw.get("permissionMode").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        agent_name: raw.get("agentName").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        agent_id: raw.get("agentId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        user_type: raw.get("userType").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        version: raw.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        entrypoint: raw.get("entrypoint").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        session_kind: raw.get("sessionKind").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        subtype: raw.get("subtype").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        leaf_uuid: raw.get("leafUuid").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        is_compact_summary: raw.get("isCompactSummary").and_then(|v| v.as_bool()).unwrap_or(false),
        logical_parent_uuid: raw.get("logicalParentUuid").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        attachment_type: raw.get("attachment").and_then(|a| a.get("type")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
    };

    Some(env)
}

fn parse_message(val: Option<&Value>) -> Option<CcMessage> {
    let msg = val?;
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if role.is_empty() {
        return None;
    }

    let model = msg.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let stop_reason = msg.get("stop_reason").and_then(|v| v.as_str()).map(String::from);

    let content = match msg.get("content") {
        Some(Value::Array(blocks)) => {
            blocks.iter().filter_map(parse_content_block).collect()
        }
        Some(Value::String(text)) => {
            vec![CcContentBlock::Text { text: text.clone() }]
        }
        _ => Vec::new(),
    };

    Some(CcMessage { role, model, stop_reason, content })
}

fn parse_content_block(block: &Value) -> Option<CcContentBlock> {
    let block_type = block.get("type").and_then(|v| v.as_str())?;

    match block_type {
        "text" => {
            let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Some(CcContentBlock::Text { text: text.to_string() })
        }
        "tool_use" => {
            let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let input = block.get("input").cloned().unwrap_or(Value::Null);
            Some(CcContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
            })
        }
        "tool_result" => {
            let tool_use_id = block.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
            let content = block.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let is_error = block.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(CcContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error,
            })
        }
        "thinking" => {
            let thinking = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
            Some(CcContentBlock::Thinking { thinking: thinking.to_string() })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_message() {
        let json = br#"{"type":"user","uuid":"abc-123","timestamp":"2025-01-01T00:00:00Z","message":{"role":"user","content":"hello world"}}"#;
        let env = parse_envelope(json).unwrap();
        assert_eq!(env.record_type, "user");
        assert_eq!(env.uuid, "abc-123");
        assert!(env.message.is_some());
        assert_eq!(env.message.unwrap().role, "user");
    }

    #[test]
    fn parse_assistant_with_tool_use() {
        let json = br#"{"type":"assistant","uuid":"def-456","message":{"role":"assistant","content":[{"type":"text","text":"checking"},{"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"ls"}}]}}"#;
        let env = parse_envelope(json).unwrap();
        assert_eq!(env.record_type, "assistant");
        let msg = env.message.unwrap();
        assert_eq!(msg.content.len(), 2);
        match &msg.content[1] {
            CcContentBlock::ToolUse { id, name, .. } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "Bash");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn parse_malformed_returns_none() {
        assert!(parse_envelope(b"not json").is_none());
    }

    #[test]
    fn parse_empty_type_returns_none() {
        assert!(parse_envelope(b"{}").is_none());
    }

    #[test]
    fn parse_system_entry() {
        let json = br#"{"type":"system","subtype":"turn_duration","uuid":"sys-001","durationMs":1000}"#;
        let env = parse_envelope(json).unwrap();
        assert_eq!(env.record_type, "system");
        assert_eq!(env.subtype, "turn_duration");
    }

    #[test]
    fn parse_background_session_fields() {
        let json = br#"{"type":"user","uuid":"bg-001","sessionId":"bg-session-1","agentId":"agent-1","userType":"external","entrypoint":"sdk-ts","version":"2.1.195","message":{"role":"user","content":"work"}}"#;
        let env = parse_envelope(json).unwrap();
        assert_eq!(env.session_id, "bg-session-1");
        assert_eq!(env.agent_id, "agent-1");
        assert_eq!(env.user_type, "external");
        assert_eq!(env.entrypoint, "sdk-ts");
        assert_eq!(env.version, "2.1.195");
    }
}
