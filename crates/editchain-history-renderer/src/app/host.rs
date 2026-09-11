//! Host protocol: parsing inbound host messages and building the exact request
//! envelopes the production renderer sends.
//!
//! Shared protocol types own service payloads. This adapter retains the host's
//! control messages and legacy error-envelope compatibility.

use editchain_protocol::{
    ErrorCode, FindInHistoryRequest, GetWindowRequest, RequestBody, ServiceError, SnapshotId,
};
use serde_json::Value;

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
    Err(ServiceError),
}

/// Unwrap a service response body exactly like `main.js unwrap()`.
pub(crate) fn unwrap(body: Option<Value>) -> Unwrapped {
    match body {
        None => Unwrapped::Ok(Value::Null),
        Some(body) => {
            if let Some(value) = body.get("Ok") {
                Unwrapped::Ok(value.clone())
            } else if let Some(error) = body.get("Error") {
                Unwrapped::Err(decode_error(error))
            } else if let Some(error) = body.get("error") {
                // Legacy lowercase envelope (old extension hosts posted
                // transport exceptions this way).
                Unwrapped::Err(decode_error(error))
            } else {
                Unwrapped::Ok(body)
            }
        }
    }
}

fn decode_error(value: &Value) -> ServiceError {
    serde_json::from_value(value.clone()).unwrap_or_else(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Invalid service error: {error}"),
        )
    })
}

/// Decode the result type selected by the correlated request.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ServiceError> {
    serde_json::from_value(value).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Invalid service response: {error}"),
        )
    })
}

/// Build a `GetWindow` request body with the exact production shape.
pub(crate) fn get_window(
    snapshot_id: &SnapshotId,
    offset: u64,
    limit: u64,
    include_layout: bool,
) -> RequestBody {
    RequestBody::GetWindow(GetWindowRequest {
        snapshot_id: snapshot_id.clone(),
        offset,
        limit,
        include_layout,
    })
}

/// Build a `FindInHistory` request body with the exact production shape.
pub(crate) fn find_in_history(snapshot_id: &SnapshotId, query: &str, top_k: usize) -> RequestBody {
    RequestBody::FindInHistory(FindInHistoryRequest {
        snapshot_id: snapshot_id.clone(),
        query: query.to_owned(),
        top_k,
    })
}

/// A host call the state machine wants performed. The DOM shell executes these
/// against the acquired VS Code API; pure tests assert on them directly.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Send {
    /// Ask the host to open current sources as a fresh snapshot.
    RefreshHistory,
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
    /// Native VS Code diff activation (`{ type: 'openDiff', change: ... }`).
    /// The host asks the service to revalidate and materialize the advertised
    /// file identity before opening virtual before/after documents.
    OpenDiff(Value),
    /// Renderer-instance handshake after installing the host listener.
    WebviewReady(String),
}

/// Debug request log entry (envelope copy), mirroring the harness contract so
/// probes and e2e runners can assert request ordering and window offsets.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LoggedRequest {
    pub(crate) id: u64,
    pub(crate) body: Value,
}

/// Helpers for reading `HistoryRow` fields with the same defaults `main.js`
/// applies (missing/undefined fields fall back, numbers are coerced).
#[cfg(test)]
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

    /// The incoming lane halves owned exclusively by muted edges.
    pub(crate) fn muted_above(value: &Value) -> Vec<u32> {
        lane_list(value.get("muted_above"))
    }

    /// The outgoing lane halves owned exclusively by muted edges.
    pub(crate) fn muted_below(value: &Value) -> Vec<u32> {
        lane_list(value.get("muted_below"))
    }

    /// The directed (child-lane, parent-lane) transition pairs.
    pub(crate) fn transitions(value: &Value) -> Vec<(u32, u32)> {
        transition_list(value.get("transitions"))
    }

    /// The cross-lane transitions owned exclusively by muted edges.
    pub(crate) fn muted_transitions(value: &Value) -> Vec<(u32, u32)> {
        transition_list(value.get("muted_transitions"))
    }

    fn transition_list(value: Option<&Value>) -> Vec<(u32, u32)> {
        match value {
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
            matches!(unwrap(Some(json!({ "Error": "boom" }))), Unwrapped::Err(e) if e.message == "boom")
        );
        assert!(
            matches!(unwrap(Some(json!({ "error": "legacy" }))), Unwrapped::Err(e) if e.message == "legacy")
        );
        assert!(
            matches!(unwrap(Some(json!({"Error": {"code": "stale_snapshot", "message": "Reopen history"}}))),
            Unwrapped::Err(message) if message.code == ErrorCode::StaleSnapshot && message.message == "Reopen history")
        );
        // Missing body is treated as a success with a null value (handshake).
        assert!(matches!(unwrap(None), Unwrapped::Ok(v) if v.is_null()));
    }

    #[test]
    fn get_window_envelope_matches_production_shape_exactly() {
        assert_eq!(
            json!(get_window(&SnapshotId::new("fixture"), 0, 500, false)),
            json!({
                "GetWindow": {
                    "snapshot_id": "fixture",
                    "offset": 0,
                    "limit": 500,
                    "include_layout": false,
                }
            })
        );
        let raw = json!(get_window(&SnapshotId::new("fixture"), 42, 100, true));
        let window = raw.get("GetWindow").expect("GetWindow envelope");
        assert_eq!(window.get("offset").and_then(Value::as_i64), Some(42));
        assert_eq!(window.get("limit").and_then(Value::as_i64), Some(100));
        assert_eq!(
            window.get("include_layout").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(window.as_object().map(serde_json::Map::len), Some(4));
    }

    #[test]
    fn find_in_history_envelope_matches_production_shape() {
        let body = json!(find_in_history(&SnapshotId::new("fixture"), "hello", 50));
        let find = body.get("FindInHistory").expect("FindInHistory envelope");
        assert_eq!(find.get("query").and_then(Value::as_str), Some("hello"));
        assert_eq!(find.get("top_k").and_then(Value::as_i64), Some(50));
        assert_eq!(find.as_object().map(serde_json::Map::len), Some(3));
    }

    #[test]
    fn row_field_helpers_apply_production_defaults() {
        let row = json!({
            "node_key": "git:m1",
            "lane": 2,
            "above": [1, 0],
            "below": [2],
            "transitions": [[1, 0], [2, 1]],
            "muted_above": [1],
            "muted_below": [2],
            "muted_transitions": [[2, 1]],
            "is_subop": true,
        });
        assert_eq!(row::str(&row, "node_key"), "git:m1");
        assert_eq!(row::str(&row, "missing"), "");
        assert_eq!(row::lane(&row), 2);
        assert_eq!(row::above(&row), vec![1, 0]);
        assert_eq!(row::below(&row), vec![2]);
        assert_eq!(row::transitions(&row), vec![(1, 0), (2, 1)]);
        assert_eq!(row::muted_above(&row), vec![1]);
        assert_eq!(row::muted_below(&row), vec![2]);
        assert_eq!(row::muted_transitions(&row), vec![(2, 1)]);
        assert!(row::bool(&row, "is_subop"));
        assert!(!row::bool(&row, "promoted"));
        // Malformed geometry defaults to empty lanes.
        let sparse = json!({ "lane": "7" });
        assert_eq!(row::lane(&sparse), 0);
        assert!(row::above(&sparse).is_empty());
        assert!(row::muted_above(&sparse).is_empty());
        assert!(row::muted_transitions(&sparse).is_empty());
    }
    #[test]
    fn send_envelope_variants_cover_the_host_contract() {
        // The shell posts openJson / webviewReady controls; keep the variants
        // alive and their envelope shapes explicit.
        let open_json = Send::OpenJson(json!({ "op_id": "op:1" }));
        assert!(matches!(&open_json, Send::OpenJson(body) if body["op_id"] == "op:1"));
        let open_diff = Send::OpenDiff(json!({ "change": { "path": "src/lib.rs" } }));
        assert!(matches!(&open_diff, Send::OpenDiff(body)
            if body
                .get("change")
                .and_then(|change| change.get("path"))
                .and_then(Value::as_str)
                == Some("src/lib.rs")));
        let ready = Send::WebviewReady("instance-abc".to_owned());
        assert!(matches!(&ready, Send::WebviewReady(id) if id == "instance-abc"));
    }
}
