//! Adapter for the historical JSON presentation goldens. Production rows use
//! the shared protocol decoder; these fixtures also cover older sparse shapes.

use serde_json::{json, Value};

use super::RowInput;
use crate::app::host::row as legacy;

impl RowInput {
    pub(crate) fn from_legacy(value: &Value) -> Self {
        let mut fields = value.as_object().cloned().unwrap_or_default();
        for (key, default) in [
            ("summary", json!("")),
            ("timestamp_ms", json!(0)),
            ("group", json!("")),
            ("node_key", json!("")),
            ("parents", json!([])),
            ("is_submodule", json!(false)),
        ] {
            let _: &mut Value = fields.entry(key.to_owned()).or_insert(default);
        }
        for (field, defaults) in [
            ("work_unit", vec![("id", json!("")), ("count", json!(0))]),
            (
                "activity_bundle",
                vec![("kind", json!("unknown")), ("member_count", json!(0))],
            ),
        ] {
            if let Some(nested) = fields.get_mut(field).and_then(Value::as_object_mut) {
                for (key, default) in defaults {
                    let _: &mut Value = nested.entry(key.to_owned()).or_insert(default);
                }
            }
        }
        for (field, key) in [("parent_relations", "parent"), ("sub_ops", "op_id")] {
            if let Some(items) = fields.get_mut(field).and_then(Value::as_array_mut) {
                for item in items {
                    if let Some(item) = item.as_object_mut() {
                        let _: &mut Value = item.entry(key.to_owned()).or_insert(json!(""));
                    }
                }
            }
        }
        if !fields.contains_key("hierarchy_depth") {
            drop(fields.insert(
                "hierarchy_depth".to_owned(),
                json!(u8::from(legacy::bool(value, "is_subop"))),
            ));
        }
        let source: editchain_protocol::HistoryRow =
            serde_json::from_value(Value::Object(fields)).expect("valid historical fixture");
        let mut row = RowInput::from(source);
        row.record_role = legacy::owned_str(value, "record_role");
        row.activity_kind = legacy::owned_str(value, "activity_kind");
        row.outcome = legacy::owned_str(value, "outcome");
        if let Some(unit) = &mut row.work_unit {
            unit.count = value
                .get("work_unit")
                .and_then(|unit| unit.get("count"))
                .and_then(Value::as_u64);
        }
        if let Some(summary) = &mut row.session_summary {
            summary.count = value
                .get("session_summary")
                .and_then(|summary| summary.get("count"))
                .and_then(Value::as_u64);
        }
        row.bundle_count = value
            .get("activity_bundle")
            .and_then(|bundle| bundle.get("member_count"))
            .and_then(Value::as_u64);
        row.resolve_content();
        row
    }
}

#[test]
fn decoded_rows_resolve_legacy_content_without_changing_source_identities() {
    let summary = "tool: functions.exec {\"output\": \"ready\"}";
    let source: editchain_protocol::HistoryRow = serde_json::from_value(json!({
        "summary": summary, "timestamp_ms": 0, "group": "session:1",
        "node_key": "18446744073709551615:2:3", "op_id": "18446744073709551615:2:3",
        "parents": [], "is_submodule": false, "kind": "tool",
        "record_role": "result", "activity_kind": "execute", "outcome": "success",
        "sub_ops": [{ "op_id": "1:2:4", "kind": "file", "summary": "src/main.rs" }]
    }))
    .unwrap();
    let row = RowInput::from(source);
    assert_eq!(row.source.summary, summary);
    assert_eq!(row.display_summary, "ready");
    assert_eq!(row.tool_label.as_deref(), Some("functions.exec"));
    assert_eq!(row.source.sub_ops.first().unwrap().kind, "file");
    let spec =
        crate::app::rows::RowSpec::from_row(&row, &crate::app::rows::RowContext::for_row(1, false));
    assert_eq!(spec.display_summary, "ready");
    assert_eq!(
        spec.open_json,
        Some(json!({
            "type": "openJson", "op_id": "18446744073709551615:2:3"
        }))
    );
    assert!(spec.disclosure.is_some());
}

#[test]
fn typed_content_preserves_authored_text_and_does_not_parse_summary_prefixes() {
    let authored = "{\"text\": \"an authored JSON example\"}";
    let source: editchain_protocol::HistoryRow = serde_json::from_value(json!({
        "summary": "tool: obsolete_name old payload", "timestamp_ms": 0,
        "group": "session:1", "node_key": "1:2:3", "op_id": "1:2:3",
        "parents": [], "is_submodule": false, "kind": "tool",
        "record_role": "action", "activity_kind": "execute",
        "content": {
            "tool_label": {"text": "工具 with spaces", "complete": true},
            "authored_summary": {"text": authored, "complete": true},
            "output_preview": {"text": "separate output", "complete": false}
        }
    }))
    .unwrap();
    let row = RowInput::from(source);
    let spec =
        crate::app::rows::RowSpec::from_row(&row, &crate::app::rows::RowContext::for_row(0, false));
    assert_eq!(spec.display_summary, authored);
    assert_eq!(spec.summary_source, authored);
    assert_eq!(
        spec.content.top.unwrap().heading.unwrap().title,
        "工具 with spaces"
    );
    assert!(
        row.source
            .content
            .as_ref()
            .unwrap()
            .authored_summary
            .as_ref()
            .unwrap()
            .complete
    );
    assert_eq!(row.source.summary, "tool: obsolete_name old payload");
}
