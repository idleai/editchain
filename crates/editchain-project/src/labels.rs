//! Bounded display labels and retained provider-content interpretation.

use editchain_core::op::NoteRelationship;
use editchain_core::{Op, Payload};
use std::sync::Arc;

/// Produce a short summary for an `EditChain` operation.
#[must_use]
pub(super) fn op_summary(op: &Op) -> String {
    use editchain_core::OpKind;
    match &op.kind {
        OpKind::Message(m) => message_summary(&payload_text(&m.content)),
        // A tool_result (stage Finish, empty tool_name) carries its result in
        // `content`; show a pretty-printed, truncated preview of it rather than
        // the empty tool_name.
        OpKind::Tool(t)
            if matches!(t.stage, editchain_core::op::ToolStage::Finish)
                && payload_text(&t.tool_name).is_empty() =>
        {
            tool_result_summary(&payload_text(&t.content))
        }
        OpKind::Tool(t) => payload_text(&t.tool_name),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::File(f) => format!("file:{}", f.path.0),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(cs) => String::from_utf8_lossy(&cs.name).to_string(),
        OpKind::Actor(a) => payload_text(&a.label),
        OpKind::Import(i) => payload_text(&i.raw_ref),
        OpKind::GitCommit(c) => payload_text(&c.message),
        OpKind::GitLink(l) => format!("git:{}", l.target_oid),
        OpKind::Unknown(u) => format!("unknown kind={}", u.kind_discriminant),
    }
}

/// Produce a pretty-printed, truncated preview of a tool result's content.
///
/// Tool results are raw text (file contents, JSON, error messages). This:
/// 1. strips leading line-number prefixes (`N\t`) that Claude Code adds to
///    file reads;
/// 2. collapses to the first non-empty line;
/// 3. truncates to ~90 chars with an ellipsis.
///
/// The result is a single-line preview suitable for the main-pane summary cell.
#[must_use]
pub(super) fn tool_result_summary(content: &str) -> String {
    const MAX: usize = 1024;
    // Strip leading `<digits>\t` line-number prefixes from every line so both
    // plain text and JSON blobs are readable.
    let stripped: String = content
        .lines()
        .map(|l| {
            // Strip a leading `<digits>\t` line-number prefix ONLY when the
            // digits are followed by a tab — otherwise a JSON line that happens
            // to start with a digit (e.g. an array element `5,`) would be mangled.
            let trimmed = l.trim_start();
            let after_digits = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
            after_digits.strip_prefix('\t').map_or(l, |rest| rest)
        })
        .collect::<Vec<_>>()
        .join("\n");
    // If the result is a JSON blob (e.g. a debugger status dump), pull a short
    // label from it instead of dumping the whole JSON.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&stripped) {
        if let Some(label) = json_status_label(&value) {
            return label;
        }
    }
    let mut line = stripped
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    if line.chars().count() > MAX {
        let mut cut = line.chars().take(MAX).collect::<String>();
        cut.push('…');
        line = cut;
    }
    line
}

/// Resolve the readable payload for one completed command row.
///
/// Current Codex command-completion envelopes retain the command and its
/// output in separate fields, while the normalized `CommandOp` historically
/// joined both into `content`. Prefer the raw output carrier so a Content cell
/// titled “Command output” starts with actual output instead of `$ <command>`.
/// The normalized content remains a compatibility fallback for standalone or
/// older provider-neutral command records.
pub(super) fn command_output_summary(op: &Op, command: &editchain_core::op::CommandOp) -> String {
    raw_command_output(op).map_or_else(
        || truncate_line(&payload_text(&command.content)),
        |output| {
            let preview = truncate_line(&output);
            if preview.is_empty() {
                "No output".to_owned()
            } else {
                preview
            }
        },
    )
}

/// Extract a command-output preview from a supported raw provider envelope.
///
/// The service keeps these fields bounded when it prepares a render
/// projection, so this path works for both direct projections and prepared
/// snapshots. `stdout` is the least decorated source; formatted and aggregate
/// spellings cover Codex schema generations and provider bridges.
fn raw_command_output(op: &Op) -> Option<String> {
    let editchain_core::OpKind::Import(import) = &op.kind else {
        return None;
    };
    let Payload::Inline(raw) = &import.raw_ref else {
        return None;
    };
    let value = serde_json::from_slice::<serde_json::Value>(raw).ok()?;
    let payload = value.get("payload");
    let item = payload.and_then(|payload| payload.get("item"));
    if let Some(output) = item.and_then(command_output_field) {
        return Some(output);
    }
    if let Some(output) = payload.and_then(command_output_field) {
        return Some(output);
    }
    if let Some(output) = command_output_field(&value) {
        return Some(output);
    }
    let recognized = [item, payload, Some(&value)]
        .into_iter()
        .flatten()
        .any(is_command_output_container);
    recognized.then(String::new)
}

/// Select the first non-empty command output field in display-preference
/// order from one JSON object.
fn command_output_field(value: &serde_json::Value) -> Option<String> {
    COMMAND_OUTPUT_FIELD_NAMES
        .iter()
        .filter_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
        .find(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

/// Whether an object is known to represent a command result even when every
/// output carrier is present-but-empty (a successful silent command).
fn is_command_output_container(value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| matches!(kind, "CommandExecution" | "commandExecution"))
        || COMMAND_OUTPUT_FIELD_NAMES
            .iter()
            .any(|key| value.get(*key).is_some())
}

const COMMAND_OUTPUT_FIELD_NAMES: [&str; 5] = [
    "stdout",
    "formatted_output",
    "formattedOutput",
    "aggregated_output",
    "aggregatedOutput",
];

/// Derive one concise argument preview for a normalized tool invocation.
///
/// The raw provider envelope is preferred because current Codex custom-tool
/// records keep their JavaScript invocation there while their normalized
/// `ToolOp` has empty content. The normalized content remains a provider-neutral
/// fallback and is the primary path for Claude tool-use records.
pub(super) fn tool_invocation_detail(op: &Op, tool: &editchain_core::op::ToolOp) -> String {
    raw_tool_invocation_detail(op, &payload_text(&tool.tool_name))
        .or_else(|| normalized_tool_invocation_detail(tool))
        .unwrap_or_default()
}

/// Read invocation arguments from a supported raw provider envelope.
fn raw_tool_invocation_detail(op: &Op, tool_name: &str) -> Option<String> {
    let editchain_core::OpKind::Import(import) = &op.kind else {
        return None;
    };
    let Payload::Inline(raw) = &import.raw_ref else {
        return None;
    };
    let value = serde_json::from_slice::<serde_json::Value>(raw).ok()?;
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("response_item") => value.get("payload").and_then(tool_payload_detail),
        Some("assistant") => claude_tool_input(&value, tool_name).and_then(tool_argument_summary),
        Some(_) | None => None,
    }
}

/// Select the first meaningful argument carrier from a Codex tool payload.
fn tool_payload_detail(payload: &serde_json::Value) -> Option<String> {
    ["arguments", "input", "parameters"]
        .iter()
        .filter_map(|key| payload.get(*key))
        .find_map(tool_argument_summary)
}

/// Select the matching Claude `tool_use` input block.
fn claude_tool_input<'a>(
    value: &'a serde_json::Value,
    tool_name: &str,
) -> Option<&'a serde_json::Value> {
    let content = value.get("message")?.get("content")?.as_array()?;
    content
        .iter()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use"))
        .find(|block| {
            tool_name.is_empty()
                || block.get("name").and_then(serde_json::Value::as_str) == Some(tool_name)
        })
        .and_then(|block| block.get("input"))
}

/// Derive an argument preview from the normalized provider-neutral tool op.
pub(super) fn normalized_tool_invocation_detail(
    tool: &editchain_core::op::ToolOp,
) -> Option<String> {
    let content = payload_text(&tool.content);
    if content.trim().is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
        return tool_argument_summary(&value);
    }
    matches!(tool.stage, editchain_core::op::ToolStage::Start)
        .then(|| compact_tool_text(&content))
        .filter(|detail| !detail.is_empty())
}

/// Produce a readable preview from JSON/scalar tool arguments.
fn tool_argument_summary(value: &serde_json::Value) -> Option<String> {
    tool_argument_summary_at_depth(value, 0)
}

/// Bounded recursive implementation for string-encoded JSON arguments.
fn tool_argument_summary_at_depth(value: &serde_json::Value, depth: usize) -> Option<String> {
    if depth > 2 {
        return None;
    }
    match value {
        serde_json::Value::String(text) => {
            if let Some(summary) = custom_tool_script_summary(text) {
                return Some(summary);
            }
            if let Ok(nested) = serde_json::from_str::<serde_json::Value>(text) {
                if !matches!(nested, serde_json::Value::String(_)) {
                    return tool_argument_summary_at_depth(&nested, depth.saturating_add(1));
                }
            }
            let compact = compact_tool_text(text);
            (!compact.is_empty()).then_some(compact)
        }
        serde_json::Value::Object(map) => {
            for key in [
                "cmd",
                "command",
                "query",
                "pattern",
                "path",
                "file_path",
                "url",
                "prompt",
                "description",
                "task_name",
                "target",
            ] {
                if let Some(summary) = map.get(key).and_then(|field| {
                    tool_argument_summary_at_depth(field, depth.saturating_add(1))
                }) {
                    return Some(summary);
                }
            }
            (map.len() == 1)
                .then(|| map.values().next())
                .flatten()
                .and_then(|field| tool_argument_summary_at_depth(field, depth.saturating_add(1)))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|item| tool_argument_summary_at_depth(item, depth.saturating_add(1))),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Null => None,
    }
}

/// Summarize the nested tool invoked by a current Codex custom-tool script.
fn custom_tool_script_summary(source: &str) -> Option<String> {
    let nested_tool = nested_tool_name(source)?;
    let detail = [
        "cmd",
        "command",
        "query",
        "q",
        "pattern",
        "path",
        "file_path",
        "url",
        "prompt",
    ]
    .iter()
    .find_map(|key| javascript_string_property(source, key))
    .map(|value| compact_tool_text(&value))
    .filter(|value| !value.is_empty());
    if nested_tool == "exec_command" {
        return detail.or(Some(nested_tool));
    }
    Some(detail.map_or(nested_tool.clone(), |detail| {
        format!("{nested_tool} · {detail}")
    }))
}

/// Extract the first `tools.<name>(...)` callee from a custom-tool script.
fn nested_tool_name(source: &str) -> Option<String> {
    let start = source.find("tools.")?.saturating_add("tools.".len());
    let after_marker = source.get(start..)?;
    let name = after_marker
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .next()
        .unwrap_or("");
    (!name.is_empty()).then(|| name.to_string())
}

/// Parse one generated JavaScript `key: "value"` property without executing
/// provider-authored code.
fn javascript_string_property(source: &str, key: &str) -> Option<String> {
    for (index, _) in source.match_indices(key) {
        let before = source
            .get(..index)
            .and_then(|prefix| prefix.chars().next_back());
        if before.is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
        }) {
            continue;
        }
        let Some(after_key) = source.get(index.saturating_add(key.len())..) else {
            continue;
        };
        let Some(encoded) = after_key.trim_start().strip_prefix(':') else {
            continue;
        };
        let encoded = encoded.trim_start();
        let mut values =
            serde_json::Deserializer::from_str(encoded).into_iter::<serde_json::Value>();
        if let Some(Ok(serde_json::Value::String(value))) = values.next() {
            return Some(value);
        }
    }
    None
}

/// Collapse whitespace and bound an invocation preview for one content cell.
fn compact_tool_text(value: &str) -> String {
    truncate_line(&value.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Resolve a collapsed File row's display path.
///
/// The core [`FileOp`] stores only a hashed `PathId`; the provider-neutral
/// path text is carried by an explicit `Explains` note targeting the file op
/// (the Codex importer emits one per file item). Rows without such an
/// annotation — e.g. Claude attachment rows, which carry no file path — fall
/// through to the raw-record label, preserving existing Claude behavior.
#[must_use]
pub(super) fn annotated_file_path(file_op: &Op, children: &[&Op]) -> Option<String> {
    for child in children {
        if let editchain_core::OpKind::Note(note) = &child.kind {
            if note.relationship == NoteRelationship::Explains
                && note.target_ids.contains(&file_op.id)
            {
                let text = payload_text(&note.content);
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }
    None
}

/// Produce a meaningful display label for a raw import record.
///
/// The raw import's `raw_ref` is the original JSONL line. When it parses as
/// JSON, derive a human-readable label from the record's `type` and structured
/// fields (e.g. attachment filename, queued command prompt). Falls back to the
/// raw text when it isn't parseable JSON.
#[must_use]
pub(super) fn raw_import_label(import: &editchain_core::op::ImportOp) -> String {
    let raw = match &import.raw_ref {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    };
    if raw.is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return raw;
    };
    if let Some(summary) = token_accounting_summary(&value) {
        return summary;
    }
    let record_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match record_type {
        // Claude emits one assistant JSONL record per response content block.
        // When an exact same-response fragment is expandable under its response
        // parent, retain the block's semantic label instead of showing the
        // transport-level word `assistant`.
        "assistant" => value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(serde_json::Value::as_array)
            .and_then(|content| content.first())
            .map_or_else(
                || record_type.to_string(),
                |block| match block.get("type").and_then(serde_json::Value::as_str) {
                    Some("tool_use") => block
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .filter(|name| !name.is_empty())
                        .map_or_else(|| "tool".to_string(), |name| format!("tool: {name}")),
                    Some("text") => block
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                        .map_or_else(|| "assistant".to_string(), truncate_line),
                    Some("thinking") => "thinking".to_string(),
                    Some(kind) if !kind.is_empty() => kind.to_string(),
                    Some(_) | None => "assistant".to_string(),
                },
            ),
        // Codex event envelopes put the meaningful lifecycle discriminator in
        // `payload.type`; showing it avoids a wall of indistinguishable
        // `event_msg` labels when a leading/unbundled record is visible.
        "event_msg" => value
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(serde_json::Value::as_str)
            .filter(|event_type| !event_type.is_empty())
            .unwrap_or(record_type)
            .to_string(),
        // Codex response items put the meaningful label in `payload.type` and
        // its content: reasoning items expose a `summary` array of
        // summary-text blocks, message items carry a `content` array. Showing
        // the payload type (or its text) avoids a wall of indistinguishable
        // `response_item` labels when a leading/unbundled record is visible.
        "response_item" => {
            let Some(payload) = value.get("payload") else {
                return record_type.to_string();
            };
            let payload_type = payload
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            match payload_type {
                "reasoning" => {
                    if let Some(text) = first_response_summary_text(payload) {
                        return truncate_line(&text);
                    }
                    if let Some(text) = first_response_content_text(payload) {
                        return truncate_line(&text);
                    }
                    "reasoning".to_string()
                }
                "message" => {
                    if let Some(text) = first_response_content_text(payload) {
                        return truncate_line(&text);
                    }
                    "message".to_string()
                }
                _ if !payload_type.is_empty() => payload_type.to_string(),
                _ => record_type.to_string(),
            }
        }
        // Attachment records carry a structured `attachment` object.
        "attachment" => {
            let att = value.get("attachment");
            let att_type = att
                .and_then(|a| a.get("type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let filename = att
                .and_then(|a| a.get("filename"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("");
            let path = att
                .and_then(|a| a.get("path"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let prompt = att
                .and_then(|a| a.get("prompt"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            match att_type {
                "file"
                | "edited_text_file"
                | "opened_file_in_ide"
                | "already_read_file"
                | "selected_lines_in_ide"
                    if !filename.is_empty() =>
                {
                    format!("{att_type}: {filename}")
                }
                "directory" if !path.is_empty() => format!("directory: {path}"),
                "queued_command" if !prompt.is_empty() => {
                    format!("queued command: {}", truncate_line(prompt))
                }
                _ if !att_type.is_empty() => format!("attachment: {att_type}"),
                _ => "attachment".to_string(),
            }
        }
        // User records with nested content (e.g. debugger status JSON).
        "user" => {
            if let Some(text) = nested_user_text(&value) {
                // If the extracted text is itself a JSON blob (e.g. a debugger
                // session-status dump), pull a short label from it instead of
                // dumping the whole JSON.
                if let Ok(inner) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(label) = json_status_label(&inner) {
                        return label;
                    }
                }
                truncate_line(&text)
            } else {
                raw
            }
        }
        _ if !record_type.is_empty() => record_type.to_string(),
        _ => raw,
    }
}

/// Derive the compact numeric subtitle for a Codex token-accounting record.
///
/// Current `token_count` events compare the latest active context with the
/// model context window (`used / limit`). Legacy `token_usage_record` entries
/// have no corresponding limit, so they show the request total alone. Returns
/// `None` for every other import shape and for token records without a usable
/// total.
#[must_use]
pub fn import_token_accounting_summary(raw: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<serde_json::Value>(raw).ok()?;
    token_accounting_summary(&value)
}

/// Resolve token-accounting values from a parsed import envelope.
fn token_accounting_summary(value: &serde_json::Value) -> Option<String> {
    let kind = token_accounting_kind(value)?;
    let payload = value.get("payload")?;
    let (used, limit) = match kind {
        "token_count" => {
            let info = payload.get("info")?;
            let used = nested_u64(info, &["last_token_usage", "total_tokens"])
                .or_else(|| nested_u64(info, &["total_token_usage", "total_tokens"]))?;
            let limit = info
                .get("model_context_window")
                .and_then(serde_json::Value::as_u64)
                .filter(|limit| *limit > 0);
            (used, limit)
        }
        "token_usage_record" => {
            let used = nested_u64(payload, &["usage", "total_tokens"])
                .or_else(|| nested_u64(payload, &["turn_token_usage", "total_tokens"]))
                .or_else(|| nested_u64(payload, &["thread_token_usage", "total_tokens"]))?;
            let limit = payload
                .get("model_context_window")
                .and_then(serde_json::Value::as_u64)
                .filter(|limit| *limit > 0);
            (used, limit)
        }
        _ => return None,
    };
    let used = format_token_count(used);
    Some(limit.map_or(used.clone(), |limit| {
        format!("{used} / {}", format_token_count(limit))
    }))
}

/// Return the normalized token-accounting kind carried by an import envelope.
fn token_accounting_kind(value: &serde_json::Value) -> Option<&'static str> {
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("token_usage_record") => Some("token_usage_record"),
        Some("event_msg")
            if value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(serde_json::Value::as_str)
                == Some("token_count") =>
        {
            Some("token_count")
        }
        _ => None,
    }
}

/// Read one unsigned integer through a short fixed JSON object path.
fn nested_u64(value: &serde_json::Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for field in path {
        current = current.get(*field)?;
    }
    current.as_u64()
}

/// Format a token count with stable ASCII thousands separators.
fn format_token_count(value: u64) -> String {
    let digits = value.to_string();
    let mut reversed = String::new();
    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(digit);
    }
    reversed.chars().rev().collect()
}

/// Extract the first non-empty summary text from a response-item payload.
///
/// Reasoning items carry a `summary` array of `summary-text` blocks (and the
/// compacted projection keeps the same shape); a plain string summary is
/// accepted too. Returns `None` when there is no text, so callers can fall
/// back to content text.
#[must_use]
fn first_response_summary_text(payload: &serde_json::Value) -> Option<String> {
    let summary = payload.get("summary")?;
    let items = match summary {
        serde_json::Value::Array(items) => items,
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            return Some(text.clone());
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Object(_) => return None,
    };
    for item in items {
        match item {
            serde_json::Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(serde_json::Value::as_str) {
                    if !text.trim().is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
            serde_json::Value::String(text) if !text.trim().is_empty() => {
                return Some(text.clone());
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::String(_) => {}
        }
    }
    None
}

/// Extract the first non-empty content text from a response-item payload.
///
/// Content blocks are `{type, text|input_text|output_text}` records; the
/// compacted projection keeps the same shape. Returns `None` when there is
/// no text.
#[must_use]
fn first_response_content_text(payload: &serde_json::Value) -> Option<String> {
    let content = payload.get("content")?;
    let items = match content {
        serde_json::Value::Array(items) => items,
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            return Some(text.clone());
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Object(_) => return None,
    };
    for item in items {
        match item {
            serde_json::Value::Object(map) => {
                for key in ["text", "input_text", "output_text"] {
                    if let Some(text) = map.get(key).and_then(serde_json::Value::as_str) {
                        if !text.trim().is_empty() {
                            return Some(text.to_string());
                        }
                    }
                }
            }
            serde_json::Value::String(text) if !text.trim().is_empty() => {
                return Some(text.clone());
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::String(_) => {}
        }
    }
    None
}

/// Extract text from a user record's possibly-nested content blocks.
///
/// Some user records nest text under `message.content[].content[].text` (e.g.
/// debugger status messages). Flatten any `text` blocks found at any depth.
#[must_use]
fn nested_user_text(value: &serde_json::Value) -> Option<String> {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(serde_json::Value::as_str) {
                    if !text.trim().is_empty() {
                        out.push(text.to_string());
                    }
                }
                for val in map.values() {
                    walk(val, out);
                }
            }
            serde_json::Value::Array(arr) => {
                for val in arr {
                    walk(val, out);
                }
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    let mut texts = Vec::new();
    walk(value, &mut texts);
    texts.first().cloned()
}

/// Produce a short label for a JSON status blob (e.g. a debugger session dump).
///
/// Prefers a `configurationName` or `name` field; falls back to a compact
/// `{key: value, ...}` summary of the top-level fields. Returns `None` when the
/// value has no useful scalar fields.
#[must_use]
fn json_status_label(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    if let Some(name) = obj
        .get("configurationName")
        .or_else(|| obj.get("name"))
        .and_then(serde_json::Value::as_str)
    {
        if !name.is_empty() {
            return Some(truncate_line(name));
        }
    }
    // Fall back to a compact summary of scalar fields.
    let mut parts = Vec::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            if !s.is_empty() {
                parts.push(format!("{k}={}", truncate_line(s)));
            }
        } else if let Some(n) = v.as_i64() {
            parts.push(format!("{k}={n}"));
        } else if let Some(b) = v.as_bool() {
            parts.push(format!("{k}={b}"));
        }
        if parts.len() >= 3 {
            break;
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// Produce a display summary for a message's content.
///
/// If the content is itself a JSON blob (e.g. a debugger session-status dump),
/// pull a short label from it instead of dumping the whole JSON. Otherwise
/// truncate the plain text.
#[must_use]
pub(super) fn message_summary(content: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(label) = json_status_label(&value) {
            return label;
        }
    }
    truncate_line(content)
}

/// Truncate a string to ~1024 chars with an ellipsis.
#[must_use]
pub(super) fn truncate_line(s: &str) -> String {
    const MAX: usize = 1024;
    let trimmed = s.trim();
    if trimmed.chars().count() > MAX {
        let mut cut = trimmed.chars().take(MAX).collect::<String>();
        cut.push('…');
        cut
    } else {
        trimmed.to_string()
    }
}

/// Build a display summary for a collapsed import from its own content plus its
/// bundled tool-result content.
///
/// The row's own summary is combined with each sub-op's meaningful content
/// (tool-result previews), joined with spaces, and truncated to ~1024 chars.
/// Metadata is intentionally child-only: copying its label into the parent
/// makes accounting records look like invocation arguments. A tool row with
/// concrete invocation detail also keeps its result child-only; generic tool
/// names may still borrow a result preview. If the row has no own content and
/// no tool-result content, falls back to `(no summary)`.
#[must_use]
pub(super) fn combined_summary(row_summary: &str, sub_ops: &[Arc<Op>]) -> String {
    const MAX: usize = 1024;
    let mut parts: Vec<String> = Vec::new();
    let own = row_summary.trim();
    if !own.is_empty() && own != "(no summary)" {
        parts.push(own.to_string());
    }
    // A concrete invocation is already the parent's best description. Its
    // result remains available as an expanded child and must not crowd or
    // duplicate the command in the parent cell.
    if tool_summary_has_invocation_detail(own) {
        return truncate_line(own);
    }
    for op in sub_ops {
        if let Some(content) = sub_op_content(op) {
            if !content.is_empty() {
                parts.push(content);
            }
        }
    }
    if parts.is_empty() {
        return "(no summary)".to_string();
    }
    let joined = parts.join(" ");
    if joined.chars().count() > MAX {
        let mut cut = joined.chars().take(MAX).collect::<String>();
        cut.push('…');
        cut
    } else {
        joined
    }
}

/// Whether a `tool: <name> <arguments>` summary contains concrete invocation
/// detail beyond the tool's name.
fn tool_summary_has_invocation_detail(summary: &str) -> bool {
    let Some(tool_summary) = summary.strip_prefix("tool:") else {
        return false;
    };
    let mut parts = tool_summary.split_whitespace();
    parts.next().is_some() && parts.next().is_some()
}

/// Extract meaningful display content from a bundled sub-op.
///
/// Tool-result sub-ops (Tool, Finish) contribute their content preview. Import
/// sub-ops are metadata disclosed as child rows and never contribute parent-row
/// content. Returns `None` for sub-ops with no useful text.
#[must_use]
pub(super) fn sub_op_content(op: &Op) -> Option<String> {
    match &op.kind {
        editchain_core::OpKind::Tool(t)
            if matches!(t.stage, editchain_core::op::ToolStage::Finish) =>
        {
            let preview = tool_result_summary(&payload_text(&t.content));
            if preview.is_empty() {
                None
            } else {
                Some(preview)
            }
        }
        editchain_core::OpKind::Import(_)
        | editchain_core::OpKind::ChainStart(_)
        | editchain_core::OpKind::Actor(_)
        | editchain_core::OpKind::Message(_)
        | editchain_core::OpKind::Tool(_)
        | editchain_core::OpKind::Command(_)
        | editchain_core::OpKind::File(_)
        | editchain_core::OpKind::Reflection(_)
        | editchain_core::OpKind::Note(_)
        | editchain_core::OpKind::Error(_)
        | editchain_core::OpKind::GitCommit(_)
        | editchain_core::OpKind::GitLink(_)
        | editchain_core::OpKind::Unknown(_) => None,
    }
}

/// Determine the dominant child kind for a collapsed import op.
///
/// Prefers message, then tool, then command — matching the summary derivation.
/// Childless token-accounting imports retain their concrete kind for Content
/// titles; every other childless record falls back to `"import"`.
#[must_use]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only message/tool/command children determine the dominant kind; all other kinds fall through"
)]
pub(super) fn collapsed_import_kind(op: &Op, children: Option<&Vec<&Op>>) -> String {
    use editchain_core::OpKind;
    if let Some(record) = crate::human::work_record(op) {
        return match record.kind {
            editchain_core::human::HumanWorkKind::Edit => "file",
            editchain_core::human::HumanWorkKind::Read => "read",
            editchain_core::human::HumanWorkKind::Exposure => "exposure",
            editchain_core::human::HumanWorkKind::Gap => "error",
        }
        .to_string();
    }
    if let Some(children) = children {
        for child in children {
            match &child.kind {
                OpKind::Message(_) => return "message".to_string(),
                OpKind::Tool(_) => return "tool".to_string(),
                OpKind::Command(_) => return "command".to_string(),
                _ => {}
            }
        }
    }
    match &op.kind {
        OpKind::Import(import) => match &import.raw_ref {
            Payload::Inline(raw) => serde_json::from_slice::<serde_json::Value>(raw)
                .ok()
                .as_ref()
                .and_then(token_accounting_kind)
                .unwrap_or("import")
                .to_string(),
            Payload::Empty | Payload::Blob(_) => "import".to_string(),
        },
        _ => "import".to_string(),
    }
}

/// Determine the author label for a collapsed import op from its children's
/// tags.
///
/// The raw import op's tags only carry `IMPORT`; the role (`HUMAN` / `AGENT`)
/// lives on the normalized children. Prefers `human`, then `agent`, and falls
/// back to `system` when no child carries a role tag.
#[must_use]
pub(super) fn collapsed_import_author(op: &Op, children: Option<&Vec<&Op>>) -> String {
    use editchain_core::Tags;
    if crate::human::work_record(op).is_some() {
        return "human".to_string();
    }
    if let Some(children) = children {
        for child in children {
            if child.tags.matches_any(Tags::HUMAN) {
                return "human".to_string();
            }
        }
        for child in children {
            if child.tags.matches_any(Tags::AGENT) {
                return "agent".to_string();
            }
        }
    }
    "system".to_string()
}

/// Extract text from a payload, or empty string.
#[must_use]
pub(super) fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}
