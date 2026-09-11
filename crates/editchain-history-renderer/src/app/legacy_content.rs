//! Compatibility recovery for summaries from services without typed content.
//! Run once when a decoded row enters the cache, before presentation.

use super::row_input::RowInput;
use serde_json::Value;
use std::collections::VecDeque;

/// `TOOL_PAYLOAD_TEXT_KEYS` — BFS visit order for tool envelopes.
const TOOL_PAYLOAD_TEXT_KEYS: [&str; 18] = [
    "text",
    "output_text",
    "input_text",
    "message",
    "summary",
    "stdout",
    "formatted_output",
    "formattedOutput",
    "output",
    "content",
    "status",
    "completed",
    "failed",
    "error",
    "cmd",
    "command",
    "query",
    "path",
];

/// `toolishRow` — the row shapes whose payloads get compacted.
fn toolish_row(row: &RowInput) -> bool {
    let record_role = row.record_role.as_str();
    let kind = row.source.kind.as_str();
    let is_system = row.source.is_system;
    let activity_kind = row.activity_kind.as_str();
    let is_action_or_tool =
        record_role == "action" || record_role == "result" || kind == "tool" || kind == "command";
    let operational =
        activity_kind == "execute" || is_system || kind == "tool" || kind == "command";
    is_action_or_tool && operational
}

/// JSON-ish start test (`jsonish` regex) on a payload.
fn jsonish(payload: &str) -> bool {
    let mut chars = payload.chars().filter(|c| !c.is_whitespace());
    match chars.next() {
        Some('[') => matches!(chars.next(), Some('{' | '"' | ']')),
        Some('{') => matches!(chars.next(), Some('"' | '}')),
        Some('"') => true,
        _ => false,
    }
}

/// The nested-envelope JSON-ish start test (no bare-string alternative).
fn nested_jsonish(payload: &str) -> bool {
    let mut chars = payload.chars().filter(|c| !c.is_whitespace());
    match chars.next() {
        Some('[') => matches!(chars.next(), Some('{' | '"' | ']')),
        Some('{') => matches!(chars.next(), Some('"' | '}')),
        _ => false,
    }
}

/// `decodedToolPayloadText` — decode a JSON tool payload with the narrow
/// truncated-summary recovery path.
fn decoded_tool_payload_text(value: &str) -> String {
    let source = value.trim();
    match serde_json::from_str::<Value>(source) {
        Ok(parsed) => {
            // Some adapters serialize a JSON envelope as a JSON string; unwrap
            // at most once (the JS `try` also covers this parse).
            if let Value::String(inner) = &parsed {
                let trimmed = inner.trim();
                if nested_jsonish(trimmed) {
                    match serde_json::from_str::<Value>(trimmed) {
                        Ok(inner_value) => return first_tool_payload_text(&inner_value),
                        Err(_) => return recovery_text(source),
                    }
                }
            }
            first_tool_payload_text(&parsed)
        }
        Err(_) => recovery_text(source),
    }
}

/// BFS over the tool envelope's text-bearing keys with the visit budget.
fn first_tool_payload_text(value: &Value) -> String {
    let mut pending: VecDeque<&Value> = VecDeque::new();
    pending.push_back(value);
    let mut visits = 0usize;
    while let Some(current) = pending.pop_front() {
        visits = visits.saturating_add(1);
        if visits >= 48 {
            break;
        }
        if let Value::String(text) = current {
            if !text.trim().is_empty() {
                return text.clone();
            }
        }
        match current {
            Value::Array(items) => {
                for item in items
                    .iter()
                    .take(48usize.saturating_sub(visits).saturating_sub(pending.len()))
                {
                    pending.push_back(item);
                }
            }
            Value::Object(map) => {
                for key in TOOL_PAYLOAD_TEXT_KEYS {
                    if pending.len().saturating_add(visits) >= 48 {
                        break;
                    }
                    if let Some(item) = map.get(key) {
                        pending.push_back(item);
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    String::new()
}

/// The truncated-summary recovery path (regex + escape recovery in main.js).
fn recovery_text(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        if chars.get(index) == Some(&'"') {
            let mut cursor = index.saturating_add(1);
            // Keyword match: one of TOOL_PAYLOAD_TEXT_KEYS followed by `"`.
            let mut keyword = None;
            for key in TOOL_PAYLOAD_TEXT_KEYS {
                let key_chars: Vec<char> = key.chars().collect();
                let matches = key_chars
                    .iter()
                    .enumerate()
                    .all(|(k, c)| chars.get(cursor.saturating_add(k)) == Some(c));
                if matches && chars.get(cursor.saturating_add(key_chars.len())) == Some(&'"') {
                    keyword = Some(key);
                    break;
                }
            }
            if let Some(keyword) = keyword {
                cursor = cursor
                    .saturating_add(keyword.chars().count())
                    .saturating_add(1);
                while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
                    cursor = cursor.saturating_add(1);
                }
                if chars.get(cursor) == Some(&':') {
                    cursor = cursor.saturating_add(1);
                    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
                        cursor = cursor.saturating_add(1);
                    }
                    if chars.get(cursor) == Some(&'"') {
                        cursor = cursor.saturating_add(1);
                        let mut encoded = String::new();
                        let mut done = false;
                        while cursor < chars.len() {
                            let c = chars.get(cursor).copied().unwrap_or('\0');
                            if c == '\\' {
                                if let Some(&next) = chars.get(cursor.saturating_add(1)) {
                                    encoded.push(c);
                                    encoded.push(next);
                                    cursor = cursor.saturating_add(2);
                                    continue;
                                }
                            } else if c == '"' {
                                done = true;
                                break;
                            }
                            encoded.push(c);
                            cursor = cursor.saturating_add(1);
                        }
                        if done {
                            return unescape_recovered(&encoded);
                        }
                    }
                }
            }
        }
        index = index.saturating_add(1);
    }
    String::new()
}

/// The three ordered escape recoveries (JS `replace` chain).
fn unescape_recovered(encoded: &str) -> String {
    let mut out = encoded
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\r", "\n");
    out = out.replace("\\\"", "\"");
    out.replace("\\\\", "\\")
}

/// `conciseToolText` — reduce multi-line execution envelopes to their first
/// meaningful status or output line.
fn concise_tool_text(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let status = lines.iter().find(|line| is_script_status(line)).copied();
    if status.is_some_and(|line| script_status_kind(line) == Some("completed")) {
        return "Script completed".to_owned();
    }
    if status.is_some_and(|line| script_status_kind(line) == Some("failed")) {
        return "Script failed".to_owned();
    }
    if status.is_some_and(|line| script_status_kind(line) == Some("running")) {
        return "Script running".to_owned();
    }
    match lines.first() {
        Some(first) => first.split_whitespace().collect::<Vec<&str>>().join(" "),
        None => String::new(),
    }
}

/// `/^Script\s+(?:completed|failed|running)\b/i` test.
fn is_script_status(line: &str) -> bool {
    script_status_kind(line).is_some()
}

fn script_status_kind(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    let rest = lower.strip_prefix("script")?;
    let rest = rest.trim_start();
    for (word, kind) in [
        ("completed", "completed"),
        ("failed", "failed"),
        ("running", "running"),
    ] {
        if let Some(after) = rest.strip_prefix(word) {
            // \b — the following char must not be a word char.
            if after.chars().next().is_none_or(|c| !is_word_char(c)) {
                return Some(kind);
            }
        }
    }
    None
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `displaySummaryForRow` — presentation-only compaction of action/result
/// payloads. Human narrative is left untouched for Markdown rendering.
pub(crate) fn display_summary_for_row(row: &RowInput, value: &str) -> String {
    let source = value.to_owned();
    let toolish = toolish_row(row);
    let trimmed = source.trim();
    // Leading inert container tag (`<…>`), as in main.js.
    let container_len = leading_container_tag_len(trimmed);
    let candidate = if let Some(len) = container_len {
        trimmed.get(len..).unwrap_or("").trim().to_owned()
    } else {
        trimmed.to_owned()
    };
    // `tool:` wrapper prefix.
    let wrapper_len = tool_wrapper_len(&candidate);
    let payload = if let Some(len) = wrapper_len {
        candidate.get(len..).unwrap_or("").trim().to_owned()
    } else {
        candidate.clone()
    };
    let jsonish = jsonish(&payload);
    if !toolish && !jsonish {
        return source;
    }
    if toolish && wrapper_len.is_none() && !jsonish {
        return source;
    }
    let decoded = if jsonish {
        decoded_tool_payload_text(&payload)
    } else {
        payload
    };
    let concise = concise_tool_text(&decoded);
    if !concise.is_empty() {
        return concise;
    }
    // A narrative JSON sample with no recognized envelope stays authored
    // content; generic labels are reserved for operational rows.
    if !toolish {
        return source;
    }
    row.empty_tool_summary()
}

/// `/^<[A-Za-z][A-Za-z0-9_.:-]*>\s*/` — one leading inert container tag.
fn leading_container_tag_len(trimmed: &str) -> Option<usize> {
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.first() != Some(&'<') {
        return None;
    }
    let first = chars.get(1).copied()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut cursor = 2usize;
    while let Some(c) = chars.get(cursor).copied() {
        if c == '>' {
            let mut after = cursor.saturating_add(1);
            while chars.get(after).is_some_and(|c| c.is_whitespace()) {
                after = after.saturating_add(1);
            }
            return Some(after);
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')) {
            return None;
        }
        cursor = cursor.saturating_add(1);
    }
    None
}

/// `/^tool:\s*[A-Za-z0-9_.:-]+(?:\s+|$)/i` — consumed-prefix length.
fn tool_wrapper_len(candidate: &str) -> Option<usize> {
    let chars: Vec<char> = candidate.chars().collect();
    let mut lower = chars.iter().map(char::to_ascii_lowercase);
    if !lower.by_ref().take(4).eq("tool".chars()) {
        return None;
    }
    if chars.get(4) != Some(&':') {
        return None;
    }
    let mut cursor = 5usize;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    let mut name_len = 0usize;
    while let Some(c) = chars.get(cursor).copied() {
        if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')) {
            break;
        }
        name_len = name_len.saturating_add(1);
        cursor = cursor.saturating_add(1);
    }
    if name_len == 0 {
        return None;
    }
    let after = chars.get(cursor).copied();
    let at_end = cursor == chars.len();
    let has_space = after.is_some_and(char::is_whitespace);
    if !at_end && !has_space {
        return None;
    }
    let mut consumed = cursor;
    if has_space {
        while chars.get(consumed).is_some_and(|c| c.is_whitespace()) {
            consumed = consumed.saturating_add(1);
        }
    }
    Some(consumed)
}

/// Recover a provider's tool name from the established `tool: NAME …`
/// summary wrapper. The display-summary compactor removes that wrapper from
/// the subtitle, making the name a natural compact title rather than repeated
/// prose.
pub(super) fn tool_name_from_summary(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let candidate = leading_container_tag_len(trimmed)
        .and_then(|len| trimmed.get(len..))
        .unwrap_or(trimmed)
        .trim();
    let wrapper_len = tool_wrapper_len(candidate)?;
    let wrapped = candidate.get(5..wrapper_len)?.trim();
    (!wrapped.is_empty()).then(|| wrapped.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_traversal_keeps_breadth_first_priority_with_a_fixed_budget() {
        let many = serde_json::json!({"content": vec![Value::Null; 100_000], "status": "ready"});
        assert_eq!(first_tool_payload_text(&many), "ready");
        let mut array = vec![Value::Null; 100_000];
        *array.get_mut(45).unwrap() = Value::String("last reachable".to_owned());
        assert_eq!(
            first_tool_payload_text(&Value::Array(array.clone())),
            "last reachable"
        );
        array.swap(45, 46);
        assert!(first_tool_payload_text(&Value::Array(array)).is_empty());
    }
}
