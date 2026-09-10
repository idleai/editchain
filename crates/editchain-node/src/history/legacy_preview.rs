//! Bounded display recovery for retained provider JSON records.

/// Convert raw JSONL to the bounded semantic subset used by row labeling,
/// classification, and outcome logic.
///
/// The projection classifier and outcome logic read the envelope
/// discriminators plus a small semantic subset: `payload.message` /
/// `payload.content` text (bounded), the first `payload.summary` reasoning
/// summary text (bounded), `payload.role`, `arguments`/`output` previews,
/// bounded command-output carriers (`stdout`/`formatted_output`/aggregate
/// spellings), a bounded structural/content signal for tool-payload carriers
/// (`arguments`/`input`/`parameters`), and structured outcome evidence
/// (`status`, `exitCode`, `errorMessage` at `payload` or `payload.item`
/// level, plus the canonical three-line Codex execution-result header), and
/// Claude's interrupted-request identity/marker. Codex token-accounting
/// records retain only their bounded identity strings, request totals, latest
/// context total, and model context limit. That is enough to validate legacy
/// metadata shapes and render a useful numeric subtitle without retaining the
/// full accounting or rate-limit payload.
/// Large outputs stay bounded to the display preview limits, and blob-backed
/// imports pass through the same bounded preview path, so the full record is
/// never copied into the projection.
#[must_use]
pub(super) fn compact_import_record(bytes: &[u8]) -> Vec<u8> {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        return serde_json::to_vec(&compact_import_value(&value)).unwrap_or_default();
    }

    // Large blob JSON is intentionally read only as a prefix, so a complete
    // serde parse can end at EOF. Discriminators and the semantic subset sit
    // near the envelope start; recover those simple fields without reading
    // the full record.
    let raw = String::from_utf8_lossy(bytes);
    let Some(record_type) = json_string_field(&raw, "type", 0) else {
        return compact_text_bytes(bytes);
    };
    let mut compact = serde_json::Map::new();
    drop(compact.insert(
        "type".to_string(),
        serde_json::Value::String(record_type.to_string()),
    ));
    if record_type == "session_meta" {
        let payload_start = raw.find("\"payload\"").unwrap_or(0);
        let mut payload = serde_json::Map::new();
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "model_provider");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "agent_nickname");
        if !payload.is_empty() {
            drop(compact.insert("payload".to_string(), serde_json::Value::Object(payload)));
        }
    } else if record_type == "token_usage_record" {
        let payload_start = raw.find("\"payload\"").unwrap_or(0);
        let mut payload = serde_json::Map::new();
        for field in [
            "thread_id",
            "turn_id",
            "session_id",
            "root_turn_id",
            "response_id",
        ] {
            let _: bool = copy_preview_string(&raw, payload_start, &mut payload, field);
        }
        for field in ["usage", "turn_token_usage", "thread_token_usage"] {
            if let Some(usage) = json_value_field(&raw, field, payload_start)
                .and_then(|value| compact_token_usage(&value))
            {
                drop(payload.insert(field.to_string(), usage));
            }
        }
        if !payload.is_empty() {
            drop(compact.insert("payload".to_string(), serde_json::Value::Object(payload)));
        }
    } else if let Some(field) = match record_type {
        "custom-title" => Some("customTitle"),
        "ai-title" => Some("aiTitle"),
        "agent-name" => Some("agentName"),
        "session_title" => Some("title"),
        _ => None,
    } {
        let _: bool = copy_preview_string(&raw, 0, &mut compact, field);
    } else if record_type == "user" {
        let _: bool = copy_preview_string(&raw, 0, &mut compact, "interruptedMessageId");
        let _: bool = copy_preview_string(&raw, 0, &mut compact, "text");
    } else if record_type == "assistant" {
        let message_start = raw.find("\"message\"").unwrap_or(0);
        let mut message = serde_json::Map::new();
        let truncated = copy_preview_string(&raw, message_start, &mut message, "id");
        if truncated {
            drop(message.remove("id"));
        }
        if !message.is_empty() {
            drop(compact.insert("message".to_string(), serde_json::Value::Object(message)));
        }
    } else if record_type == "event_msg" || record_type == "response_item" {
        let payload_start = raw.find("\"payload\"").unwrap_or(0);
        let mut payload = serde_json::Map::new();
        // Whether the echo message text (`payload.message` on an
        // `event_msg`/`agent_message`, or the first `payload.content` text on
        // a `response_item`/`message`) was truncated by the preview read limit
        // or the display budget. Truncated text must never participate in
        // exact duplicate pairing, so the classifier is told explicitly
        // instead of guessing from an ellipsis.
        let mut echo_text_truncated = false;
        let event_type = json_string_field(&raw, "type", payload_start);
        if let Some(event_type) = event_type {
            drop(payload.insert(
                "type".to_string(),
                serde_json::Value::String(event_type.to_string()),
            ));
        }
        if event_type == Some("token_count") {
            if let Some(info) = json_value_field(&raw, "info", payload_start) {
                copy_token_count_info(&info, &mut payload);
            }
        }
        echo_text_truncated |= copy_preview_string(&raw, payload_start, &mut payload, "message");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "role");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "status");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "errorMessage");
        // Recover payload-level outcome evidence the same way the full-parse
        // path does (a truncated prefix may cut the record before its
        // `payload.item` block entirely).
        if let Some(code) = json_number_field(&raw, "exitCode", payload_start) {
            drop(payload.insert(
                "exitCode".to_string(),
                serde_json::Value::Number(code.into()),
            ));
        }
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "arguments");
        copy_preview_structured(&raw, payload_start, &mut payload, "arguments");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "input");
        copy_preview_structured(&raw, payload_start, &mut payload, "input");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "parameters");
        copy_preview_structured(&raw, payload_start, &mut payload, "parameters");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "output");
        for field in COMMAND_OUTPUT_FIELDS {
            let _: bool = copy_preview_string(&raw, payload_start, &mut payload, field);
        }
        copy_codex_exec_output_header_from_prefix(&raw, payload_start, &mut payload);
        if let Some(content_start) = raw
            .get(payload_start..)
            .and_then(|tail| tail.find("\"content\""))
        {
            let content_abs = payload_start.saturating_add(content_start);
            if let Some((text, cut_by_read_limit)) =
                json_string_field_preview(&raw, "text", content_abs)
            {
                let decoded = decode_json_string_preview(text);
                let (compact, cut_by_char_limit) = compact_text_with_signal(&decoded);
                echo_text_truncated |= cut_by_read_limit || cut_by_char_limit;
                drop(payload.insert(
                    "content".to_string(),
                    serde_json::json!([{ "type": "input_text", "text": compact }]),
                ));
            }
        }
        if let Some(summary_start) = raw
            .get(payload_start..)
            .and_then(|tail| tail.find("\"summary\""))
        {
            let summary_abs = payload_start.saturating_add(summary_start);
            if let Some(text) = json_string_field(&raw, "text", summary_abs) {
                let decoded = decode_json_string_preview(text);
                if !decoded.trim().is_empty() {
                    drop(payload.insert(
                        "summary".to_string(),
                        serde_json::json!([{ "type": "summary_text", "text": compact_text(&decoded) }]),
                    ));
                }
            }
        }
        if let Some(item_start) = raw
            .get(payload_start..)
            .and_then(|tail| tail.find("\"item\""))
        {
            let item_abs = payload_start.saturating_add(item_start);
            let mut item = serde_json::Map::new();
            let _: bool = copy_preview_string(&raw, item_abs, &mut item, "type");
            let _: bool = copy_preview_string(&raw, item_abs, &mut item, "status");
            let _: bool = copy_preview_string(&raw, item_abs, &mut item, "errorMessage");
            for field in COMMAND_OUTPUT_FIELDS {
                let _: bool = copy_preview_string(&raw, item_abs, &mut item, field);
            }
            if let Some(code) = json_number_field(&raw, "exitCode", item_abs) {
                drop(item.insert(
                    "exitCode".to_string(),
                    serde_json::Value::Number(code.into()),
                ));
            }
            if !item.is_empty() {
                drop(payload.insert("item".to_string(), serde_json::Value::Object(item)));
            }
        }
        if echo_text_truncated {
            drop(payload.insert(
                "echo_text_truncated".to_string(),
                serde_json::Value::Bool(true),
            ));
        }
        if !payload.is_empty() {
            drop(compact.insert("payload".to_string(), serde_json::Value::Object(payload)));
        }
    }
    serde_json::to_vec(&serde_json::Value::Object(compact)).unwrap_or_default()
}

/// Bounded semantic subset of one fully-parsed raw import record.
#[must_use]
fn compact_import_value(value: &serde_json::Value) -> serde_json::Value {
    let record_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut compact = serde_json::Map::new();
    drop(compact.insert(
        "type".to_string(),
        serde_json::Value::String(record_type.to_string()),
    ));
    match record_type {
        "attachment" => {
            drop(compact.insert(
                "attachment".to_string(),
                value.get("attachment").cloned().unwrap_or_default(),
            ));
        }
        "user" => {
            drop(compact.insert(
                "text".to_string(),
                serde_json::Value::String(first_nested_json_text(value).unwrap_or_default()),
            ));
            let _: bool = copy_bounded_field(value, &mut compact, "interruptedMessageId");
        }
        "assistant" => {
            let mut message = serde_json::Map::new();
            let truncated = value
                .get("message")
                .is_some_and(|source| copy_bounded_field(source, &mut message, "id"));
            if truncated {
                drop(message.remove("id"));
            }
            if !message.is_empty() {
                drop(compact.insert("message".to_string(), serde_json::Value::Object(message)));
            }
        }
        "custom-title" => {
            let _: bool = copy_bounded_field(value, &mut compact, "customTitle");
        }
        "ai-title" => {
            let _: bool = copy_bounded_field(value, &mut compact, "aiTitle");
        }
        "agent-name" => {
            let _: bool = copy_bounded_field(value, &mut compact, "agentName");
        }
        "session_title" => {
            let _: bool = copy_bounded_field(value, &mut compact, "title");
        }
        _ => {}
    }
    if let Some(payload) = value.get("payload") {
        let mut compact_payload = serde_json::Map::new();
        // Whether the echo message text (`payload.message` on an
        // `event_msg`/`agent_message`, or the first `payload.content` text on
        // a `response_item`/`message`) was truncated by the display budget.
        // Truncated text must never participate in exact duplicate pairing,
        // so the classifier is told explicitly instead of guessing from the
        // ellipsis.
        let mut echo_text_truncated = false;
        if record_type == "session_meta" {
            let _: bool = copy_bounded_field(payload, &mut compact_payload, "model_provider");
            let _: bool = copy_bounded_field(payload, &mut compact_payload, "agent_nickname");
        }
        copy_token_accounting_payload(record_type, payload, &mut compact_payload);
        copy_string_field(payload, &mut compact_payload, "type");
        copy_string_field(payload, &mut compact_payload, "role");
        echo_text_truncated |= copy_bounded_field(payload, &mut compact_payload, "message");
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "arguments");
        copy_structured_payload_field(payload, &mut compact_payload, "arguments");
        copy_structured_payload_field(payload, &mut compact_payload, "input");
        copy_structured_payload_field(payload, &mut compact_payload, "parameters");
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "output");
        for field in COMMAND_OUTPUT_FIELDS {
            let _: bool = copy_bounded_field(payload, &mut compact_payload, field);
        }
        copy_codex_exec_output_header(payload, &mut compact_payload);
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "status");
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "errorMessage");
        copy_i64_field(payload, &mut compact_payload, "exitCode");
        if let Some(content) = payload.get("content") {
            let (compact, truncated) = compact_content(content);
            echo_text_truncated |= truncated;
            drop(compact_payload.insert("content".to_string(), compact));
        }
        if let Some(summary) = payload.get("summary") {
            let compact = compact_summary(summary);
            match compact.as_array() {
                Some(items) if !items.is_empty() => {
                    drop(compact_payload.insert("summary".to_string(), compact));
                }
                _ => {}
            }
        }
        if let Some(item) = payload.get("item") {
            let mut compact_item = serde_json::Map::new();
            copy_string_field(item, &mut compact_item, "type");
            let _: bool = copy_bounded_field(item, &mut compact_item, "status");
            let _: bool = copy_bounded_field(item, &mut compact_item, "errorMessage");
            for field in COMMAND_OUTPUT_FIELDS {
                let _: bool = copy_bounded_field(item, &mut compact_item, field);
            }
            copy_i64_field(item, &mut compact_item, "exitCode");
            if !compact_item.is_empty() {
                drop(
                    compact_payload
                        .insert("item".to_string(), serde_json::Value::Object(compact_item)),
                );
            }
        }
        if echo_text_truncated {
            drop(compact_payload.insert(
                "echo_text_truncated".to_string(),
                serde_json::Value::Bool(true),
            ));
        }
        if !compact_payload.is_empty() {
            drop(compact.insert(
                "payload".to_string(),
                serde_json::Value::Object(compact_payload),
            ));
        }
    }
    serde_json::Value::Object(compact)
}

/// Provider spellings that can carry the readable result of a completed
/// command. Every retained value passes through the bounded text-preview path.
const COMMAND_OUTPUT_FIELDS: [&str; 5] = [
    "stdout",
    "formatted_output",
    "formattedOutput",
    "aggregated_output",
    "aggregatedOutput",
];

/// Copy one JSON string field verbatim into a compact payload object.
fn copy_string_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    if let Some(value) = source.get(key).and_then(serde_json::Value::as_str) {
        drop(out.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        ));
    }
}

/// Copy one JSON text field, bounded to the display preview limit.
///
/// Returns whether the copied text was truncated by the display budget, so
/// callers can flag echo message text that must not participate in exact
/// duplicate pairing.
fn copy_bounded_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> bool {
    let Some(value) = source.get(key).and_then(serde_json::Value::as_str) else {
        return false;
    };
    if value.trim().is_empty() {
        return false;
    }
    let (compact, truncated) = compact_text_with_signal(value);
    drop(out.insert(key.to_string(), serde_json::Value::String(compact)));
    truncated
}

/// Retain only the canonical three-line status header from a fully parsed
/// Codex custom-exec output array.
///
/// The normalized Tool child keeps the complete bounded result preview. This
/// small raw-envelope copy exists solely so projection outcome classification
/// does not lose its structured evidence during service compaction.
fn copy_codex_exec_output_header(
    payload: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if payload.get("type").and_then(serde_json::Value::as_str) != Some("custom_tool_call_output") {
        return;
    }
    let Some(first) = payload
        .get("output")
        .and_then(serde_json::Value::as_array)
        .and_then(|blocks| blocks.first())
    else {
        return;
    };
    if first.get("type").and_then(serde_json::Value::as_str) != Some("input_text") {
        return;
    }
    let Some(header) = first
        .get("text")
        .and_then(serde_json::Value::as_str)
        .and_then(codex_exec_output_header)
    else {
        return;
    };
    drop(out.insert(
        "output".to_string(),
        serde_json::json!([{ "type": "input_text", "text": header }]),
    ));
}

/// Recover the same canonical header when a blob preview ends before the raw
/// JSON record closes and therefore cannot be fully parsed.
fn copy_codex_exec_output_header_from_prefix(
    raw: &str,
    payload_start: usize,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if out.get("type").and_then(serde_json::Value::as_str) != Some("custom_tool_call_output") {
        return;
    }
    let Some(output_rel) = raw
        .get(payload_start..)
        .and_then(|tail| tail.find("\"output\""))
    else {
        return;
    };
    let output_start = payload_start.saturating_add(output_rel);
    let Some((encoded, _)) = json_string_field_preview(raw, "text", output_start) else {
        return;
    };
    let decoded = decode_json_string_preview(encoded);
    let Some(header) = codex_exec_output_header(&decoded) else {
        return;
    };
    drop(out.insert(
        "output".to_string(),
        serde_json::json!([{ "type": "input_text", "text": header }]),
    ));
}

/// Validate and bound Codex's machine-generated execution-result header.
#[must_use]
fn codex_exec_output_header(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let status = lines.next()?;
    if !matches!(status, "Script completed" | "Script failed") {
        return None;
    }
    let wall_time = lines.next()?;
    if !wall_time.starts_with("Wall time ")
        || !wall_time.ends_with(" seconds")
        || lines.next() != Some("Output:")
    {
        return None;
    }
    Some(format!("{status}\n{wall_time}\nOutput:"))
}

/// Copy one structured tool-payload carrier (`arguments`/`input`/
/// `parameters`) into a compact payload, bounded to the display preview
/// limits. A carrier holding a non-empty object/array, non-empty string, or
/// scalar boolean/number keeps a bounded content signal so a childless
/// tool-like envelope is not misread as empty transport after compaction.
fn copy_structured_payload_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    let Some(value) = source.get(key) else {
        return;
    };
    if !is_meaningful_carrier_value(value) {
        return;
    }
    drop(out.insert(key.to_string(), compact_structured(value)));
}

/// Maximum nesting depth retained in a structured tool-payload carrier
/// preview. Deeper input is pruned so pathological nesting cannot recurse
/// without bound.
pub(super) const STRUCTURED_CARRIER_MAX_DEPTH: usize = 16;

/// Maximum total entries retained across the whole structured tool-payload
/// carrier preview. The budget is shared globally (not per object/array), so
/// the retained output is deterministically bounded.
pub(super) const STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT: usize = 64;

/// Bounded copy of a structured tool-payload carrier.
///
/// Retains a bounded structural/content signal for the classifier: strings
/// are cut to the display preview limit, object keys are cut the same way
/// (Unicode-scalar safe, with an ellipsis on cut), nesting is pruned at
/// `STRUCTURED_CARRIER_MAX_DEPTH`, and the total number of retained object
/// keys plus array items never exceeds `STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT`
/// across the whole carrier.
#[must_use]
pub(super) fn compact_structured(value: &serde_json::Value) -> serde_json::Value {
    let mut budget = STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT;
    compact_structured_bounded(value, 0, &mut budget)
}

/// Depth- and budget-bounded recursive step of [`compact_structured`].
#[must_use]
fn compact_structured_bounded(
    value: &serde_json::Value,
    depth: usize,
    budget: &mut usize,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) if depth < STRUCTURED_CARRIER_MAX_DEPTH => {
            let mut compact = serde_json::Map::new();
            for (key, child) in map {
                if *budget == 0 {
                    break;
                }
                // Bound the retained key text the same way as string values;
                // otherwise a few arbitrarily huge keys would keep unbounded
                // raw payload despite the entry budget. Keep the first key
                // when truncation makes two distinct original keys collide,
                // so no retained entry is silently overwritten.
                let bounded_key = compact_text(key);
                if compact.contains_key(&bounded_key) {
                    continue;
                }
                *budget = budget.saturating_sub(1);
                drop(compact.insert(
                    bounded_key,
                    compact_structured_bounded(child, depth.saturating_add(1), budget),
                ));
            }
            serde_json::Value::Object(compact)
        }
        serde_json::Value::Array(items) if depth < STRUCTURED_CARRIER_MAX_DEPTH => {
            let mut compact = Vec::new();
            for item in items {
                if *budget == 0 {
                    break;
                }
                *budget = budget.saturating_sub(1);
                compact.push(compact_structured_bounded(
                    item,
                    depth.saturating_add(1),
                    budget,
                ));
            }
            serde_json::Value::Array(compact)
        }
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => serde_json::Value::Null,
        serde_json::Value::String(text) => serde_json::Value::String(compact_text(text)),
        other @ (serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)) => other.clone(),
    }
}

/// Whether a JSON value carries a meaningful tool-payload signal.
///
/// Non-empty objects/arrays, non-empty strings, and scalar booleans/numbers
/// all carry signal; null, empty strings, and empty objects/arrays do not.
fn is_meaningful_carrier_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => !map.is_empty(),
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::Null => false,
    }
}

/// Retain only numeric fields needed to label Codex token-accounting rows.
fn copy_token_accounting_payload(
    record_type: &str,
    payload: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if record_type == "token_usage_record" {
        for field in [
            "thread_id",
            "turn_id",
            "session_id",
            "root_turn_id",
            "response_id",
        ] {
            let _: bool = copy_bounded_field(payload, out, field);
        }
        for field in ["usage", "turn_token_usage", "thread_token_usage"] {
            copy_token_usage_field(payload, out, field);
        }
    } else if record_type == "event_msg"
        && payload.get("type").and_then(serde_json::Value::as_str) == Some("token_count")
    {
        if let Some(info) = payload.get("info") {
            copy_token_count_info(info, out);
        }
    }
}

/// Copy one usage object while discarding every field except `total_tokens`.
fn copy_token_usage_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) {
    if let Some(usage) = source.get(field).and_then(compact_token_usage) {
        drop(out.insert(field.to_string(), usage));
    }
}

/// Compact a token-usage object to its total while retaining an empty object
/// marker for legacy schema recognition.
fn compact_token_usage(value: &serde_json::Value) -> Option<serde_json::Value> {
    if !value.is_object() {
        return None;
    }
    let mut compact = serde_json::Map::new();
    copy_u64_field(value, &mut compact, "total_tokens");
    Some(serde_json::Value::Object(compact))
}

/// Copy the latest active-context total and context limit from a token event.
fn copy_token_count_info(
    info: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if !info.is_object() {
        return;
    }
    let mut compact = serde_json::Map::new();
    copy_token_usage_field(info, &mut compact, "last_token_usage");
    copy_token_usage_field(info, &mut compact, "total_token_usage");
    copy_u64_field(info, &mut compact, "model_context_window");
    if !compact.is_empty() {
        drop(out.insert("info".to_string(), serde_json::Value::Object(compact)));
    }
}

/// Copy one non-negative JSON integer field verbatim.
fn copy_u64_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    if let Some(value) = source.get(key).and_then(serde_json::Value::as_u64) {
        drop(out.insert(key.to_string(), serde_json::Value::Number(value.into())));
    }
}

/// Copy one JSON integer field verbatim.
fn copy_i64_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    if let Some(value) = source.get(key).and_then(serde_json::Value::as_i64) {
        drop(out.insert(key.to_string(), serde_json::Value::Number(value.into())));
    }
}

/// Bounded copy of a `payload.content` array for the classifier.
///
/// Keeps only the first text-bearing item (bounded), which is all the prefix
/// classifier and content-presence checks read; the rest of the content stays
/// deferred to the durable record. The returned flag reports whether the
/// retained text was truncated by the display budget, so callers can flag
/// response-item echo message text that must not participate in exact
/// duplicate pairing.
#[must_use]
fn compact_content(content: &serde_json::Value) -> (serde_json::Value, bool) {
    let Some(items) = content.as_array() else {
        return (serde_json::Value::Array(Vec::new()), false);
    };
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let mut compact_item = serde_json::Map::new();
        if let Some(kind) = obj.get("type").and_then(serde_json::Value::as_str) {
            drop(compact_item.insert(
                "type".to_string(),
                serde_json::Value::String(kind.to_string()),
            ));
        }
        let mut truncated = false;
        for key in ["text", "input_text", "output_text"] {
            if let Some(text) = obj.get(key).and_then(serde_json::Value::as_str) {
                let (compact, cut) = compact_text_with_signal(text);
                truncated |= cut;
                drop(compact_item.insert(key.to_string(), serde_json::Value::String(compact)));
            }
        }
        if compact_item.contains_key("text")
            || compact_item.contains_key("input_text")
            || compact_item.contains_key("output_text")
        {
            return (
                serde_json::Value::Array(vec![serde_json::Value::Object(compact_item)]),
                truncated,
            );
        }
    }
    (serde_json::Value::Array(Vec::new()), false)
}

/// Bounded copy of a `payload.summary` array for response-item labeling.
///
/// Reasoning response items carry a `summary` array of summary-text blocks;
/// keeps only the first text-bearing item (bounded), which is all the
/// response-item label logic reads. The rest of the summary stays deferred
/// to the durable record.
#[must_use]
fn compact_summary(summary: &serde_json::Value) -> serde_json::Value {
    let Some(items) = summary.as_array() else {
        return serde_json::Value::Array(Vec::new());
    };
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let mut compact_item = serde_json::Map::new();
        if let Some(kind) = obj.get("type").and_then(serde_json::Value::as_str) {
            drop(compact_item.insert(
                "type".to_string(),
                serde_json::Value::String(kind.to_string()),
            ));
        }
        if let Some(text) = obj.get("text").and_then(serde_json::Value::as_str) {
            if !text.trim().is_empty() {
                drop(compact_item.insert(
                    "text".to_string(),
                    serde_json::Value::String(compact_text(text)),
                ));
            }
        }
        if compact_item.contains_key("text") {
            return serde_json::Value::Array(vec![serde_json::Value::Object(compact_item)]);
        }
    }
    serde_json::Value::Array(Vec::new())
}

/// Copy one simple string field recovered from a truncated JSON prefix,
/// bounded to the display preview limit.
///
/// Returns whether the copied value was truncated by the preview read limit
/// or the display budget, so callers can flag echo message text that must not
/// participate in exact duplicate pairing.
fn copy_preview_string(
    raw: &str,
    start: usize,
    out: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> bool {
    let Some((value, cut_by_read_limit)) = json_string_field_preview(raw, field, start) else {
        return false;
    };
    let decoded = decode_json_string_preview(value);
    if decoded.trim().is_empty() {
        return false;
    }
    let (compact, cut_by_char_limit) = compact_text_with_signal(&decoded);
    drop(out.insert(field.to_string(), serde_json::Value::String(compact)));
    cut_by_read_limit || cut_by_char_limit
}

/// Decode the JSON escapes in one recovered string fragment.
///
/// Prefix recovery sees the bytes *inside* the source JSON quotes. Wrapping a
/// fragment in a fresh pair of quotes lets serde decode complete escapes (for
/// example `\n` and `\"`) so display summaries match the fully parsed path.
/// A preview can end in an incomplete escape; in that case parsing fails and
/// the raw fragment is retained while the caller's read-limit flag preserves
/// the conservative truncation semantics.
fn decode_json_string_preview(fragment: &str) -> String {
    let mut wrapped = String::with_capacity(fragment.len().saturating_add(2));
    wrapped.push('"');
    wrapped.push_str(fragment);
    wrapped.push('"');
    serde_json::from_str::<String>(&wrapped).unwrap_or_else(|_| fragment.to_string())
}

/// Copy one structured tool-payload carrier recovered from a truncated JSON
/// prefix, bounded to whatever lies within the preview prefix. A carrier that
/// starts but does not close before the cutoff keeps the incomplete-carrier
/// sentinel so a large childless tool call is not misread as empty transport.
fn copy_preview_structured(
    raw: &str,
    start: usize,
    out: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) {
    let Some(value) = json_value_field(raw, field, start) else {
        return;
    };
    if !is_meaningful_carrier_value(&value) {
        return;
    }
    drop(out.insert(field.to_string(), compact_structured(&value)));
}

/// Sentinel retained for a structured tool-payload carrier that begins inside
/// the bounded preview but closes after the read cutoff.
#[must_use]
fn incomplete_carrier_sentinel() -> serde_json::Value {
    serde_json::json!({ "truncated": true })
}

/// Extract one JSON object/array/scalar field value from a truncated prefix.
///
/// An object/array must close within the bounded prefix to parse; one cut off
/// by the preview read limit yields the incomplete-carrier sentinel instead
/// of nothing, so a large childless tool call still carries a content signal.
/// Numbers and booleans are recovered as bounded scalar tokens; null and
/// absent fields yield values the caller's meaningfulness check drops.
fn json_value_field(raw: &str, field: &str, start: usize) -> Option<serde_json::Value> {
    let tail = raw.get(start..)?;
    let needle = format!("\"{field}\"");
    let field_offset = tail.find(&needle)?.saturating_add(needle.len());
    let after_field = tail.get(field_offset..)?;
    let colon = after_field.find(':')?;
    let value = after_field.get(colon.saturating_add(1)..)?.trim_start();
    match value.chars().next()? {
        '{' | '[' => {
            let mut depth: i64 = 0;
            let mut in_string = false;
            let mut escaped = false;
            for (idx, ch) in value.char_indices() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        in_string = false;
                    }
                    continue;
                }
                match ch {
                    '"' => in_string = true,
                    '{' | '[' => depth = depth.saturating_add(1),
                    '}' | ']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            let end = idx.saturating_add(ch.len_utf8());
                            return serde_json::from_str(value.get(..end)?).ok();
                        }
                    }
                    _ => {}
                }
            }
            Some(incomplete_carrier_sentinel())
        }
        _ => {
            let end = value
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '+' | '.')))
                .unwrap_or(value.len());
            serde_json::from_str(value.get(..end)?).ok()
        }
    }
}

/// Find the first non-empty nested JSON `text` field.
fn first_nested_json_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(serde_json::Value::as_str) {
                if !text.trim().is_empty() {
                    return Some(compact_text(text));
                }
            }
            map.values().find_map(first_nested_json_text)
        }
        serde_json::Value::Array(values) => values.iter().find_map(first_nested_json_text),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => None,
    }
}

/// Extract one simple JSON string field from a prefix, reporting whether the
/// value was cut before its closing quote by the preview read limit.
pub(super) fn json_string_field_preview<'a>(
    raw: &'a str,
    field: &str,
    start: usize,
) -> Option<(&'a str, bool)> {
    let tail = raw.get(start..)?;
    let needle = format!("\"{field}\"");
    let field_offset = tail.find(&needle)?.saturating_add(needle.len());
    let after_field = tail.get(field_offset..)?;
    let colon = after_field.find(':')?;
    let value = after_field.get(colon.saturating_add(1)..)?.trim_start();
    let quoted = value.strip_prefix('"')?;
    // A large blob is read only up to the preview limit, so a long value may
    // be cut before its closing quote; the bounded remainder is still the
    // value's prefix (the marker lives at the start).
    // A quote closes the JSON string only when the immediately preceding run
    // of backslashes has even length. A plain `find('"')` mistakes `\"` for
    // the closing delimiter and can turn a read-limit-cut echo into an
    // apparently complete value, defeating the truncation safety flag.
    let mut odd_backslash_run = false;
    for (offset, byte) in quoted.bytes().enumerate() {
        if byte == b'\\' {
            odd_backslash_run = !odd_backslash_run;
            continue;
        }
        if byte == b'"' && !odd_backslash_run {
            return Some((quoted.get(..offset)?, false));
        }
        odd_backslash_run = false;
    }
    Some((quoted, true))
}

/// Extract one simple JSON string field from a prefix.
pub(super) fn json_string_field<'a>(raw: &'a str, field: &str, start: usize) -> Option<&'a str> {
    json_string_field_preview(raw, field, start).map(|(value, _)| value)
}

/// Extract one simple JSON integer field from a prefix.
fn json_number_field(raw: &str, field: &str, start: usize) -> Option<i64> {
    let tail = raw.get(start..)?;
    let needle = format!("\"{field}\"");
    let field_offset = tail.find(&needle)?.saturating_add(needle.len());
    let after_field = tail.get(field_offset..)?;
    let colon = after_field.find(':')?;
    let value = after_field.get(colon.saturating_add(1)..)?.trim_start();
    let end = value
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(value.len());
    value.get(..end)?.parse().ok()
}

/// Maximum characters retained in any projection payload.
pub(super) const DISPLAY_PREVIEW_CHAR_LIMIT: usize = 1024;

/// Bound arbitrary bytes as lossy UTF-8 display text.
pub(super) fn compact_text_bytes(bytes: &[u8]) -> Vec<u8> {
    compact_text(&String::from_utf8_lossy(bytes)).into_bytes()
}

/// Bound display text by Unicode scalar count, appending an ellipsis on cut,
/// reporting whether the text was truncated.
#[must_use]
fn compact_text_with_signal(text: &str) -> (String, bool) {
    let mut chars = text.chars();
    let mut compact: String = chars.by_ref().take(DISPLAY_PREVIEW_CHAR_LIMIT).collect();
    let truncated = chars.next().is_some();
    if truncated {
        compact.push('…');
    }
    (compact, truncated)
}

/// Bound display text by Unicode scalar count, appending an ellipsis on cut.
#[must_use]
pub(super) fn compact_text(text: &str) -> String {
    compact_text_with_signal(text).0
}
