use serde_json::Value;

/// A loose envelope around a Claude Code JSONL record.
///
/// Only captures fields needed for import routing.
/// Unknown fields are silently ignored (forward-compat).
#[derive(Debug, Clone, Default)]
pub struct CcEnvelope {
    /// The record type (e.g. "user", "assistant", "system").
    pub record_type: String,
    /// UUID of this record.
    pub uuid: String,
    /// UUID of the parent record.
    pub parent_uuid: String,
    /// ISO 8601 timestamp string.
    pub timestamp: String,
    /// Session UUID this record belongs to.
    pub session_id: String,
    /// Optional parsed message content.
    pub message: Option<CcMessage>,
    /// Whether this is a sidechain record.
    pub is_sidechain: bool,
    /// Whether this is a meta record.
    pub is_meta: bool,
    /// Working directory at the time of the record.
    pub cwd: String,
    /// Git branch at the time of the record.
    pub git_branch: String,
    /// Permission mode (e.g. "default", "bypass").
    pub permission_mode: String,
    /// Name of the agent (if applicable).
    pub agent_name: String,
    /// ID of the agent (if applicable).
    pub agent_id: String,
    /// Type of user (e.g. "human", "agent").
    pub user_type: String,
    /// Version string of Claude Code.
    pub version: String,
    /// Entrypoint (e.g. "cli", "api").
    pub entrypoint: String,
    /// Kind of session (e.g. "normal", "resume").
    pub session_kind: String,
    /// Subtype of record (e.g. `"compact_boundary"`).
    pub subtype: String,
    /// UUID of the leaf in the conversation tree.
    pub leaf_uuid: String,
    /// Whether this is a compact summary record.
    pub is_compact_summary: bool,
    /// UUID of the logical parent in the conversation tree.
    pub logical_parent_uuid: String,
    /// Type of attachment (if applicable).
    pub attachment_type: String,
}

/// A message within a Claude Code record.
#[derive(Debug, Clone)]
pub struct CcMessage {
    /// Role of the message author (e.g. "user", "assistant").
    pub role: String,
    /// Model identifier (e.g. "claude-sonnet-4-20250514").
    pub model: String,
    /// Reason the model stopped generating.
    pub stop_reason: Option<String>,
    /// Content blocks in this message.
    pub content: Vec<CcContentBlock>,
}

/// A content block within a message.
#[derive(Debug, Clone)]
pub enum CcContentBlock {
    /// Plain text content.
    Text {
        /// The text content.
        text: String,
    },
    /// A tool use invocation by the assistant.
    ToolUse {
        /// Unique ID for this tool call.
        id: String,
        /// Name of the tool being called.
        name: String,
        /// JSON input arguments for the tool.
        input: Value,
    },
    /// A tool result returned to the assistant.
    ToolResult {
        /// ID of the `tool_use` this result corresponds to.
        tool_use_id: String,
        /// Result content as a string.
        content: String,
        /// Whether the tool returned an error.
        is_error: bool,
    },
    /// Model thinking/reasoning content.
    Thinking {
        /// The thinking text content.
        thinking: String,
    },
}

/// Parse a single JSONL line into a `CcEnvelope`.
///
/// Returns None for malformed lines or lines without a type field.
#[must_use]
pub fn parse_envelope(line: &[u8]) -> Option<CcEnvelope> {
    let raw: Value = serde_json::from_slice(line).ok()?;

    let record_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if record_type.is_empty() {
        return None;
    }

    let env = CcEnvelope {
        record_type: record_type.to_string(),
        uuid: raw
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        parent_uuid: raw
            .get("parentUuid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        timestamp: raw
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        session_id: raw
            .get("sessionId")
            .or_else(|| raw.get("session_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        message: parse_message(raw.get("message")),
        is_sidechain: raw
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        is_meta: raw.get("isMeta").and_then(Value::as_bool).unwrap_or(false),
        cwd: raw
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        git_branch: raw
            .get("gitBranch")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        permission_mode: raw
            .get("permissionMode")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        agent_name: raw
            .get("agentName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        agent_id: raw
            .get("agentId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        user_type: raw
            .get("userType")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        version: raw
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        entrypoint: raw
            .get("entrypoint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        session_kind: raw
            .get("sessionKind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        subtype: raw
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        leaf_uuid: raw
            .get("leafUuid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        is_compact_summary: raw
            .get("isCompactSummary")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        logical_parent_uuid: raw
            .get("logicalParentUuid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        attachment_type: raw
            .get("attachment")
            .and_then(|a| a.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    };

    Some(env)
}

fn parse_message(val: Option<&Value>) -> Option<CcMessage> {
    let msg = val?;
    let role = msg
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if role.is_empty() {
        return None;
    }

    let model = msg
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let stop_reason = msg
        .get("stop_reason")
        .and_then(Value::as_str)
        .map(String::from);

    let content = match msg.get("content") {
        Some(Value::Array(blocks)) => blocks.iter().filter_map(parse_content_block).collect(),
        Some(Value::String(text)) => {
            vec![CcContentBlock::Text { text: text.clone() }]
        }
        _ => Vec::new(),
    };

    Some(CcMessage {
        role,
        model,
        stop_reason,
        content,
    })
}

fn parse_content_block(block: &Value) -> Option<CcContentBlock> {
    let block_type = block.get("type").and_then(|v| v.as_str())?;

    match block_type {
        "text" => {
            let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Some(CcContentBlock::Text {
                text: text.to_string(),
            })
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
            let tool_use_id = block
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = block.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(CcContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error,
            })
        }
        "thinking" => {
            let thinking = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
            Some(CcContentBlock::Thinking {
                thinking: thinking.to_string(),
            })
        }
        _ => None,
    }
}
