//! Host protocol: parsing inbound host messages and building the exact request
//! envelopes the production renderer sends.
//!
//! The service protocol speaks JSON with camelCase field names (the wire shape
//! is authoritative; see `crates/editchain-protocol`). Like the production
//! renderer, we read fields defensively with the same defaults JavaScript
//! applies, so fixture and real-service payloads behave identically.

use serde_json::{json, Value};

/// A message delivered by the extension host (or fixture bridge).
#[derive(Debug, Clone)]
pub(crate) struct HostMessage {
    /// `open`, `ready`, `reveal`, or a numeric request id.
    pub(crate) id: Id,
    /// Raw response body; may be absent for plain handshakes.
    pub(crate) body: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Id {
    Open,
    Ready,
    Reveal,
    Request(u64),
    Unknown(String),
}

impl HostMessage {
    /// Parse one host message from its structured-clone JSON form.
    ///
    /// Matches `main.js`'s `msg.id` / `msg.body` access: `id: 'open'` is the
    /// open handshake, `id: 'ready'` starts loading, `id: 'reveal'` replays a
    /// recreated context, and numeric ids correlate service responses.
    pub(crate) fn parse(value: &Value) -> Option<HostMessage> {
        if value.is_null() || !value.is_object() {
            return None;
        }
        let id = match value.get("id") {
            Some(Value::String(s)) if s == "open" => Id::Open,
            Some(Value::String(s)) if s == "ready" => Id::Ready,
            Some(Value::String(s)) if s == "reveal" => Id::Reveal,
            Some(Value::String(s)) => Id::Unknown(s.clone()),
            Some(Value::Number(n)) => n
                .as_u64()
                .map_or_else(|| Id::Unknown(n.to_string()), Id::Request),
            Some(Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_)) | None => {
                return None;
            }
        };
        Some(HostMessage {
            id,
            body: value.get("body").cloned(),
        })
    }
}

/// Unwrapped service response body (`{ Ok: v } | { Error: msg }`, plus the
/// legacy lowercase `{ error: msg }` transport envelope).
#[derive(Debug, Clone)]
pub(crate) enum Unwrapped {
    Ok(Value),
    Err(String),
}

/// Unwrap a service response body exactly like `main.js unwrap()`.
pub(crate) fn unwrap(body: Option<Value>) -> Unwrapped {
    match body {
        None => Unwrapped::Ok(Value::Null),
        Some(body) => {
            if let Some(value) = body.get("Ok") {
                Unwrapped::Ok(value.clone())
            } else if let Some(error) = body.get("Error") {
                Unwrapped::Err(as_string(error))
            } else if let Some(error) = body.get("error") {
                // Legacy lowercase envelope (old extension hosts posted
                // transport exceptions this way).
                Unwrapped::Err(as_string(error))
            } else {
                Unwrapped::Ok(body)
            }
        }
    }
}

fn as_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_)) => other.to_string(),
    }
}

/// The fixed chain filter sent with every GetWindow/FindInHistory request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChainFilter {
    pub(crate) summary_pattern: String,
    pub(crate) kind_pattern: String,
    pub(crate) include_kind_pattern: String,
    pub(crate) hide_undated: bool,
    pub(crate) splice: bool,
    pub(crate) hide_trace: bool,
}

impl ChainFilter {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "summary_pattern": self.summary_pattern,
            "kind_pattern": self.kind_pattern,
            "include_kind_pattern": self.include_kind_pattern,
            "hide_undated": self.hide_undated,
            "splice": self.splice,
            "hide_trace": self.hide_trace,
        })
    }
}

/// Fixed submodule hiding for every window request (matches the production
/// `FIXED_HIDE_SUBMODULES` view flag).
const HIDE_SUBMODULES: bool = true;

/// Build a `GetWindow` request body with the exact production shape.
pub(crate) fn get_window(
    offset: i64,
    limit: i64,
    filter: &ChainFilter,
    include_layout: bool,
) -> Value {
    json!({
        "GetWindow": {
            "offset": offset,
            "limit": limit,
            "hide_submodules": HIDE_SUBMODULES,
            "filter": filter.to_json(),
            "include_layout": include_layout,
        }
    })
}

/// Build a `FindInHistory` request body with the exact production shape.
pub(crate) fn find_in_history(
    query: &str,
    top_k: i64,
    filter: &ChainFilter,
    hide_submodules: bool,
) -> Value {
    json!({
        "FindInHistory": {
            "query": query,
            "top_k": top_k,
            "filters": {},
            "filter": filter.to_json(),
            "hide_submodules": hide_submodules,
        }
    })
}

/// Build a legacy flat-list `Search` request body with the exact production
/// retry shape (`{ query, mode: 'Lexical', top_k: 50, filters: {} }`).
pub(crate) fn search(query: &str, top_k: i64) -> Value {
    json!({
        "Search": {
            "query": query,
            "mode": "Lexical",
            "top_k": top_k,
            "filters": {},
        }
    })
}

/// A host call the state machine wants performed. The DOM shell executes these
/// against the acquired VS Code API; pure tests assert on them directly.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Send {
    /// A correlated service request `{ id, body }`.
    Request { id: u64, body: Value },
    /// Renderer diagnostics / log lines (`{ type: 'log', text }`).
    Log(String),
    /// Status-bar loaded/total counts (`{ type: 'status', loaded, total }`).
    Status { loaded: u64, total: u64 },
    /// Assistive-tech/status text announcement (`{ type: 'statusText', text }`).
    StatusText(String),
    /// Raw-JSON editor activation (`{ type: 'openJson', ... }` — exact
    /// envelope). The wasm shell (`src/lib.rs` `execute_send`) posts the
    /// envelope verbatim; rows.rs builds the git/op identity form.
    OpenJson(Value),
    /// Renderer-instance handshake after installing the host listener.
    WebviewReady(String),
}

/// Debug request log entry (envelope copy), mirroring the harness contract so probes and
/// e2e runners can assert request ordering (window offsets, `hide_trace` flags).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LoggedRequest {
    pub(crate) id: u64,
    pub(crate) body: Value,
}

/// Helpers for reading `HistoryRow` fields with the same defaults `main.js`
/// applies (missing/undefined fields fall back, numbers are coerced).
pub(crate) mod row {
    use super::Value;

    /// Read a string field (missing/undefined -> empty string).
    pub(crate) fn str<'a>(value: &'a Value, key: &str) -> &'a str {
        value.get(key).and_then(Value::as_str).unwrap_or("")
    }

    /// Read a string field as an owned `String`.
    pub(crate) fn owned_str(value: &Value, key: &str) -> String {
        str(value, key).to_owned()
    }

    /// Read a boolean field (missing/undefined -> false).
    pub(crate) fn bool(value: &Value, key: &str) -> bool {
        value.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    /// The row's graph lane (missing/undefined -> 0).
    pub(crate) fn lane(value: &Value) -> u32 {
        value
            .get("lane")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0)
    }

    /// The lanes entering this row from above.
    pub(crate) fn above(value: &Value) -> Vec<u32> {
        lane_list(value.get("above"))
    }

    /// The lanes leaving this row downward.
    pub(crate) fn below(value: &Value) -> Vec<u32> {
        lane_list(value.get("below"))
    }

    /// The directed (child-lane, parent-lane) transition pairs.
    pub(crate) fn transitions(value: &Value) -> Vec<(u32, u32)> {
        match value.get("transitions") {
            Some(Value::Array(pairs)) => pairs
                .iter()
                .filter_map(|pair| {
                    let list = pair.as_array()?;
                    let from = u32::try_from(list.first().and_then(Value::as_u64)?).ok()?;
                    let to = u32::try_from(list.get(1).and_then(Value::as_u64)?).ok()?;
                    Some((from, to))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn lane_list(value: Option<&Value>) -> Vec<u32> {
        match value {
            Some(Value::Array(list)) => list
                .iter()
                .filter_map(|lane| lane.as_u64().and_then(|l| u32::try_from(l).ok()))
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_message_parsing_matches_production_ids() {
        assert_eq!(
            HostMessage::parse(&json!({ "id": "open", "body": { "Ok": { "nodes": 5 } } }))
                .unwrap()
                .id,
            Id::Open
        );
        assert_eq!(
            HostMessage::parse(&json!({ "id": "ready", "body": { "Ok": {} } }))
                .unwrap()
                .id,
            Id::Ready
        );
        assert_eq!(
            HostMessage::parse(&json!({ "id": "reveal" })).unwrap().id,
            Id::Reveal
        );
        assert_eq!(
            HostMessage::parse(&json!({ "id": 7, "body": { "Ok": [] } }))
                .unwrap()
                .id,
            Id::Request(7)
        );
        assert!(HostMessage::parse(&json!(null)).is_none());
        assert!(HostMessage::parse(&json!({ "no-id": true })).is_none());
    }

    #[test]
    fn unwrap_handles_ok_error_and_legacy_envelopes() {
        assert!(matches!(
            unwrap(Some(json!({ "Ok": { "total": 3 } }))),
            Unwrapped::Ok(v) if v.get("total").and_then(Value::as_i64) == Some(3)
        ));
        assert!(
            matches!(unwrap(Some(json!({ "Error": "boom" }))), Unwrapped::Err(e) if e == "boom")
        );
        assert!(
            matches!(unwrap(Some(json!({ "error": "legacy" }))), Unwrapped::Err(e) if e == "legacy")
        );
        // Missing body is treated as a success with a null value (handshake).
        assert!(matches!(unwrap(None), Unwrapped::Ok(v) if v.is_null()));
    }

    #[test]
    fn get_window_envelope_matches_production_shape_exactly() {
        let filter = ChainFilter {
            summary_pattern: String::new(),
            kind_pattern: String::new(),
            include_kind_pattern: String::new(),
            hide_undated: false,
            splice: true,
            hide_trace: true,
        };
        assert_eq!(
            get_window(0, 500, &filter, false),
            json!({
                "GetWindow": {
                    "offset": 0,
                    "limit": 500,
                    "hide_submodules": true,
                    "filter": {
                        "summary_pattern": "",
                        "kind_pattern": "",
                        "include_kind_pattern": "",
                        "hide_undated": false,
                        "splice": true,
                        "hide_trace": true,
                    },
                    "include_layout": false,
                }
            })
        );
        let raw = get_window(42, 100, &filter, true);
        let window = raw.get("GetWindow").expect("GetWindow envelope");
        assert_eq!(window.get("offset").and_then(Value::as_i64), Some(42));
        assert_eq!(window.get("limit").and_then(Value::as_i64), Some(100));
        assert_eq!(
            window.get("include_layout").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            window
                .get("filter")
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn find_in_history_envelope_matches_production_shape() {
        let filter = ChainFilter {
            summary_pattern: String::new(),
            kind_pattern: String::new(),
            include_kind_pattern: String::new(),
            hide_undated: false,
            splice: true,
            hide_trace: true,
        };
        let body = find_in_history("hello", 50, &filter, true);
        let find = body.get("FindInHistory").expect("FindInHistory envelope");
        assert_eq!(find.get("query").and_then(Value::as_str), Some("hello"));
        assert_eq!(find.get("top_k").and_then(Value::as_i64), Some(50));
        assert_eq!(find.get("filters"), Some(&json!({})));
        assert_eq!(
            find.get("hide_submodules").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            find.get("filter")
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn row_field_helpers_apply_production_defaults() {
        let row = json!({
            "node_key": "git:m1",
            "lane": 2,
            "above": [1, 0],
            "below": [2],
            "transitions": [[1, 0], [2, 1]],
            "is_subop": true,
        });
        assert_eq!(row::str(&row, "node_key"), "git:m1");
        assert_eq!(row::str(&row, "missing"), "");
        assert_eq!(row::lane(&row), 2);
        assert_eq!(row::above(&row), vec![1, 0]);
        assert_eq!(row::below(&row), vec![2]);
        assert_eq!(row::transitions(&row), vec![(1, 0), (2, 1)]);
        assert!(row::bool(&row, "is_subop"));
        assert!(!row::bool(&row, "promoted"));
        // Malformed geometry defaults to empty lanes.
        let sparse = json!({ "lane": "7" });
        assert_eq!(row::lane(&sparse), 0);
        assert!(row::above(&sparse).is_empty());
    }
    #[test]
    fn search_envelope_matches_the_production_retry_shape() {
        let body = search("needle", 50);
        let search = body.get("Search").expect("Search envelope");
        assert_eq!(search.get("query").and_then(Value::as_str), Some("needle"));
        assert_eq!(search.get("mode").and_then(Value::as_str), Some("Lexical"));
        assert_eq!(search.get("top_k").and_then(Value::as_i64), Some(50));
        assert_eq!(search.get("filters"), Some(&json!({})));
        assert!(
            search.get("filter").is_none(),
            "legacy Search carries no chain filter"
        );
    }

    #[test]
    fn send_envelope_variants_cover_the_host_contract() {
        // The shell posts openJson / webviewReady controls; keep the variants
        // alive and their envelope shapes explicit.
        let open_json = Send::OpenJson(json!({ "op_id": "op:1" }));
        assert!(matches!(&open_json, Send::OpenJson(body) if body["op_id"] == "op:1"));
        let ready = Send::WebviewReady("instance-abc".to_owned());
        assert!(matches!(&ready, Send::WebviewReady(id) if id == "instance-abc"));
    }
}
