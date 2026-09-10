//! Codex `editchain-v1` bridge projection parsing, validation, and folding
//! tests (bridge envelope: `line`/`final` records).
#![expect(
    clippy::indexing_slicing,
    reason = "test assertions on known-length vectors"
)]

use blake3 as _;
use editchain_codec as _;
use editchain_core as _;
use editchain_project as _;
use editchain_store as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;
use time as _;
use tokio as _;

use editchain_import::codex::projection::{parse_projection, ProjectionError, ProjectionKind};

/// Build a projection byte string from record fragments.
fn records(parts: &[String]) -> Vec<u8> {
    let mut out = String::new();
    for p in parts {
        out.push_str(p);
        out.push('\n');
    }
    out.into_bytes()
}

/// A `line` record with a decode status and a projection body (may be empty).
fn line(ordinal: u64, decode_status: &str, projection: &str) -> String {
    format!(
        "{{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"s.jsonl\",\"sourceOrdinal\":{ordinal},\"decode\":{{\"status\":\"{decode_status}\",\"kind\":\"eventMsg\"}},\"projection\":{{{projection}}}}}"
    )
}

fn ok_line(ordinal: u64, projection: &str) -> String {
    line(ordinal, "ok", projection)
}

/// A `changedItems` entry.
fn change(turn: &str, item_kind: &str, item_id: &str, extra: &str) -> String {
    format!(
        "{{\"turnId\":\"{turn}\",\"item\":{{\"kind\":\"{item_kind}\",\"id\":\"{item_id}\"{extra}}}}}"
    )
}

fn removed_turns(turn_ids: &[&str]) -> String {
    format!(
        "\"removedTurnIds\":[{}]",
        turn_ids
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// A `final` record at the EOF anchor.
fn final_record(ordinal: u64, thread: &str) -> String {
    format!(
        "{{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"final\",\"sourcePath\":\"s.jsonl\",\"sourceOrdinal\":{ordinal},\"decode\":{{\"status\":\"ok\",\"kind\":\"sessionSummary\"}},\"threadId\":\"{thread}\",\"physicalLineCount\":{ordinal},\"turns\":[]}}"
    )
}

#[test]
fn parse_empty_output_for_empty_file() {
    let projection = parse_projection(b"", 0, None).unwrap();
    assert!(projection.final_items.is_empty());
    assert_eq!(projection.malformed, 0);
    assert_eq!(projection.owning_thread, None);
}

#[test]
fn parse_empty_output_for_nonempty_file_is_protocol_error() {
    let err = parse_projection(b"", 2, Some(2)).unwrap_err();
    assert!(matches!(err, ProjectionError::Protocol(_)));
    assert!(err.to_string().contains("expected 2 line records"));
}

#[test]
fn parse_records_without_changes_produce_no_items() {
    let out = records(&[ok_line(1, ""), ok_line(2, ""), ok_line(3, "")]);
    let projection = parse_projection(&out, 3, Some(3)).unwrap();
    assert!(projection.final_items.is_empty());
    assert_eq!(projection.malformed, 0);
}

#[test]
fn parse_single_upsert() {
    let change = change("turn-1", "agentMessage", "item-a", ",\"text\":\"hi\"");
    let out = records(&[ok_line(1, &format!("\"changedItems\":[{change}]"))]);
    let projection = parse_projection(&out, 1, Some(1)).unwrap();
    assert_eq!(projection.final_items.len(), 1);
    let item = &projection.final_items[0];
    assert_eq!(item.item_id, "item-a");
    assert_eq!(item.turn_id, "turn-1");
    assert_eq!(item.first_seen, 1);
    assert_eq!(item.kind, ProjectionKind::Message);
    assert_eq!(item.actor, "assistant");
    assert_eq!(item.payload["text"], "hi");
}

#[test]
fn blank_line_gaps_are_allowed() {
    // Physical lines 1..=3 with line 2 blank: records at ordinals 1 and 3 only.
    let out = records(&[ok_line(1, ""), ok_line(3, "")]);
    let projection = parse_projection(&out, 3, Some(2)).unwrap();
    assert!(projection.final_items.is_empty());
}

#[test]
fn record_count_mismatch_is_protocol_error() {
    let out = records(&[ok_line(1, ""), ok_line(2, "")]);
    let err = parse_projection(&out, 3, Some(3)).unwrap_err();
    assert!(err
        .to_string()
        .contains("expected 3 line records for 3 non-blank physical lines"));
}

#[test]
fn schema_version_mismatch_is_protocol_error() {
    let out = b"{\"schemaVersion\":\"editchain-v2\",\"recordType\":\"line\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"}}\n";
    let err = parse_projection(out, 1, None).unwrap_err();
    assert!(err.to_string().contains("schema version"));
}

#[test]
fn missing_schema_version_is_protocol_error() {
    let out = b"{\"recordType\":\"line\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"}}\n";
    let err = parse_projection(out, 1, None).unwrap_err();
    assert!(err.to_string().contains("missing schema version"));
}

#[test]
fn missing_ordinal_is_protocol_error() {
    let out = b"{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"decode\":{\"status\":\"ok\"}}\n";
    let err = parse_projection(out, 1, None).unwrap_err();
    assert!(err.to_string().contains("missing ordinal `sourceOrdinal`"));
}

#[test]
fn zero_ordinal_is_out_of_range() {
    let out = b"{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourceOrdinal\":0,\"decode\":{\"status\":\"ok\"}}\n";
    let err = parse_projection(out, 1, None).unwrap_err();
    assert!(err.to_string().contains("out of range 1..=1"));
}

#[test]
fn ordinal_beyond_file_is_out_of_range() {
    let out = b"{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourceOrdinal\":5,\"decode\":{\"status\":\"ok\"}}\n";
    let err = parse_projection(out, 3, None).unwrap_err();
    assert!(err.to_string().contains("out of range 1..=3"));
}

#[test]
fn duplicate_or_decreasing_ordinal_is_out_of_sequence() {
    let out = records(&[ok_line(1, ""), ok_line(3, ""), ok_line(3, "")]);
    let err = parse_projection(&out, 3, None).unwrap_err();
    assert!(err.to_string().contains("out of sequence (last 3)"));
    let out = records(&[ok_line(1, ""), ok_line(3, ""), ok_line(2, "")]);
    let err = parse_projection(&out, 3, None).unwrap_err();
    assert!(err.to_string().contains("out of sequence (last 3)"));
}

#[test]
fn invalid_json_on_stdout_is_protocol_error() {
    let out = b"{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"}}\nnot json\n";
    let err = parse_projection(out, 2, None).unwrap_err();
    assert!(err.to_string().contains("invalid JSON"));
}

#[test]
fn unknown_record_type_is_protocol_error() {
    let out = b"{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"future\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"}}\n";
    let err = parse_projection(out, 1, None).unwrap_err();
    assert!(err.to_string().contains("unknown recordType"));
}

#[test]
fn decode_error_line_is_non_fatal_and_malformed() {
    let out = records(&[line(1, "error", "")]);
    let projection = parse_projection(&out, 1, Some(1)).unwrap();
    assert_eq!(projection.malformed, 1);
    assert!(projection.final_items.is_empty());
}

#[test]
fn unknown_item_kind_is_non_fatal_and_raw_only() {
    let change = change("turn-1", "futureGadget", "gadget-1", "");
    let out = records(&[ok_line(1, &format!("\"changedItems\":[{change}]"))]);
    let projection = parse_projection(&out, 1, Some(1)).unwrap();
    assert!(projection.final_items.is_empty());
    assert_eq!(projection.malformed, 0);
}

#[test]
fn missing_turn_id_is_malformed() {
    let out = b"{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourceOrdinal\":1,\"decode\":{\"status\":\"ok\"},\"projection\":{\"changedItems\":[{\"item\":{\"kind\":\"agentMessage\",\"id\":\"x\",\"text\":\"hi\"}}]}}\n";
    let projection = parse_projection(out, 1, Some(1)).unwrap();
    assert_eq!(projection.malformed, 1);
    assert!(projection.final_items.is_empty());
}

#[test]
fn empty_item_id_is_malformed() {
    let change = change("turn-1", "agentMessage", "", ",\"text\":\"hi\"");
    let out = records(&[ok_line(1, &format!("\"changedItems\":[{change}]"))]);
    let projection = parse_projection(&out, 1, Some(1)).unwrap();
    assert!(projection.final_items.is_empty());
    assert_eq!(projection.malformed, 1);
}

#[test]
fn upsert_and_turn_removal_fold() {
    let a1 = change("turn-1", "userMessage", "a-1", ",\"text\":\"first\"");
    let a2 = change("turn-1", "userMessage", "a-1", ",\"text\":\"second\"");
    let b1 = change("turn-2", "agentMessage", "b-1", ",\"text\":\"hi\"");
    let out = records(&[
        ok_line(1, &format!("\"changedItems\":[{a1}]")),
        ok_line(2, &format!("\"changedItems\":[{a2}]")),
        ok_line(3, &format!("\"changedItems\":[{b1}]")),
        ok_line(4, &removed_turns(&["turn-1"])),
    ]);
    let projection = parse_projection(&out, 4, Some(4)).unwrap();
    assert_eq!(projection.final_items.len(), 1);
    assert_eq!(projection.final_items[0].item_id, "b-1");
    assert_eq!(projection.final_items[0].turn_id, "turn-2");
    assert_eq!(projection.final_items[0].first_seen, 3);
    assert_eq!(projection.final_items[0].actor, "assistant");
}

#[test]
fn repeated_upserts_fold_to_one_item_with_final_content() {
    // Streaming accumulation / item_completed repeats: same (turn, item id),
    // content replaced, first-seen ordinal retained.
    let c1 = change("turn-1", "agentMessage", "msg-1", ",\"text\":\"first\"");
    let c2 = change(
        "turn-1",
        "agentMessage",
        "msg-1",
        ",\"text\":\"first second\"",
    );
    let out = records(&[
        ok_line(2, &format!("\"changedItems\":[{c1}]")),
        ok_line(3, &format!("\"changedItems\":[{c2}]")),
        ok_line(4, &format!("\"changedItems\":[{c2}]")),
    ]);
    let projection = parse_projection(&out, 4, Some(3)).unwrap();
    assert_eq!(projection.final_items.len(), 1);
    assert_eq!(projection.final_items[0].first_seen, 2);
    assert_eq!(projection.final_items[0].payload["text"], "first second");
}

#[test]
fn final_record_captured_thread_id() {
    let out = records(&[ok_line(1, ""), final_record(1, "thread-final")]);
    let projection = parse_projection(&out, 1, Some(1)).unwrap();
    assert_eq!(projection.owning_thread.as_deref(), Some("thread-final"));
}

#[test]
fn final_record_must_close_stream_at_eof_anchor() {
    let out = records(&[ok_line(1, ""), final_record(2, "t")]);
    let err = parse_projection(&out, 1, None).unwrap_err();
    assert!(err.to_string().contains("does not match EOF anchor 1"));
}

#[test]
fn records_after_final_record_are_protocol_error() {
    let out = records(&[final_record(1, "t"), ok_line(1, "")]);
    let err = parse_projection(&out, 1, None).unwrap_err();
    assert!(err.to_string().contains("line record after final record"));
}

#[test]
fn session_meta_thread_id_is_captured_from_first_record() {
    let out = records(&[
        ok_line(1, "\"sessionMeta\":{\"threadId\":\"thread-bridge\"}"),
        ok_line(
            2,
            "\"sessionMeta\":{\"sessionId\":\"x\",\"threadId\":\"thread-late\"}",
        ),
    ]);
    let projection = parse_projection(&out, 2, Some(2)).unwrap();
    assert_eq!(projection.owning_thread.as_deref(), Some("thread-bridge"));
}

#[test]
fn parse_kind_mapping() {
    assert_eq!(
        ProjectionKind::parse("userMessage"),
        ProjectionKind::Message
    );
    assert_eq!(
        ProjectionKind::parse("agentMessage"),
        ProjectionKind::Message
    );
    assert_eq!(ProjectionKind::parse("toolCall"), ProjectionKind::Tool);
    assert_eq!(
        ProjectionKind::parse("collabToolCall"),
        ProjectionKind::Tool
    );
    assert_eq!(
        ProjectionKind::parse("commandExecution"),
        ProjectionKind::Command
    );
    assert_eq!(ProjectionKind::parse("fileChange"), ProjectionKind::File);
    assert_eq!(ProjectionKind::parse("imageView"), ProjectionKind::File);
    assert_eq!(
        ProjectionKind::parse("reasoning"),
        ProjectionKind::Reflection
    );
    assert_eq!(ProjectionKind::parse("plan"), ProjectionKind::Reflection);
    assert_eq!(
        ProjectionKind::parse("contextCompaction"),
        ProjectionKind::Reflection
    );
    assert_eq!(
        ProjectionKind::parse("subAgentActivity"),
        ProjectionKind::Note
    );
    assert_eq!(ProjectionKind::parse("opaque"), ProjectionKind::Unknown);
    assert_eq!(
        ProjectionKind::parse("futureThing"),
        ProjectionKind::Unknown
    );
}

#[test]
fn final_items_are_deterministic_across_repeated_parses() {
    let b2 = change("turn-1", "agentMessage", "b-2", ",\"text\":\"two\"");
    let a3 = change("turn-1", "userMessage", "a-3", ",\"text\":\"three\"");
    let out = records(&[
        ok_line(2, &format!("\"changedItems\":[{b2}]")),
        ok_line(3, &format!("\"changedItems\":[{a3}]")),
    ]);
    let a = parse_projection(&out, 3, Some(2)).unwrap();
    let b = parse_projection(&out, 3, Some(2)).unwrap();
    assert_eq!(a.final_items, b.final_items);
    let ids: Vec<_> = a.final_items.iter().map(|i| i.item_id.clone()).collect();
    assert_eq!(ids, vec!["b-2".to_string(), "a-3".to_string()]);
}

#[test]
fn malformed_entry_drops_only_that_entry_and_keeps_record_count() {
    // One malformed changedItems entry plus one good one on the same line:
    // the malformed entry is dropped and counted, the good entry folds, and
    // the line still counts toward the full-file record count.
    let good = change("turn-1", "agentMessage", "a", ",\"text\":\"hi\"");
    let bad = "{\"item\":{\"kind\":\"agentMessage\",\"id\":\"x\"}}";
    let out = records(&[
        ok_line(1, &format!("\"changedItems\":[{bad},{good}]")),
        ok_line(2, "\"changedItems\":[]"),
    ]);
    let projection = parse_projection(&out, 2, Some(2)).unwrap();
    assert_eq!(
        projection.malformed, 1,
        "only the malformed entry is counted"
    );
    assert_eq!(projection.final_items.len(), 1);
    assert_eq!(projection.final_items[0].item_id, "a");
}

#[test]
fn second_final_record_is_protocol_error() {
    let out = records(&[final_record(1, "t"), final_record(1, "t")]);
    let err = parse_projection(&out, 1, None).unwrap_err();
    assert!(err.to_string().contains("second final record"));
}

#[test]
fn upserts_preserve_first_seen_and_track_last_seen() {
    let c1 = change("turn-1", "agentMessage", "msg-1", ",\"text\":\"first\"");
    let c2 = change("turn-1", "agentMessage", "msg-1", ",\"text\":\"second\"");
    let out = records(&[
        ok_line(2, &format!("\"changedItems\":[{c1}]")),
        ok_line(5, &format!("\"changedItems\":[{c2}]")),
    ]);
    let projection = parse_projection(&out, 5, Some(2)).unwrap();
    assert_eq!(projection.final_items.len(), 1);
    assert_eq!(projection.final_items[0].first_seen, 2);
    assert_eq!(projection.final_items[0].last_seen, 5);
    assert_eq!(projection.final_items[0].payload["text"], "second");
}

#[test]
fn inter_agent_and_compaction_lines_are_parsed() {
    let out = records(&[
        ok_line(
            1,
            "\"interAgent\":{\"id\":\"ia-1\",\"author\":\"sub\",\"content\":\"hi there\"}",
        ),
        ok_line(
            2,
            "\"compacted\":{\"message\":\"context compacted\",\"replacementCount\":1}",
        ),
    ]);
    let projection = parse_projection(&out, 2, Some(2)).unwrap();
    assert_eq!(projection.inter_agent_lines.len(), 1);
    assert_eq!(projection.inter_agent_lines[0].source_ordinal, 1);
    assert_eq!(projection.inter_agent_lines[0].content, "hi there");
    assert_eq!(
        projection.inter_agent_lines[0].author.as_deref(),
        Some("sub")
    );
    assert_eq!(projection.compacted_lines.len(), 1);
    assert_eq!(projection.compacted_lines[0].source_ordinal, 2);
    assert_eq!(projection.compacted_lines[0].message, "context compacted");
}

#[test]
fn session_meta_is_parsed_into_provider_neutral_metadata() {
    let meta = "{\"sessionId\":\"sess-1\",\"threadId\":\"thread-1\",\"parentThreadId\":\"parent-1\",\"forkedFromId\":\"source-1\",\"agentNickname\":\"Darwin\",\"agentPath\":\"/root/a\",\"threadSource\":\"subagent\",\"source\":{\"subagent\":{\"thread_spawn\":{\"depth\":1}}},\"originator\":\"codex\",\"modelProvider\":\"openai\",\"cwd\":\"/workspace/sub\"}";
    let out = records(&[ok_line(1, &format!("\"sessionMeta\":{meta}"))]);
    let projection = parse_projection(&out, 1, Some(1)).unwrap();
    let parsed = projection.session_meta.expect("session meta parsed");
    assert_eq!(parsed.thread_id, "thread-1");
    assert_eq!(parsed.session_id.as_deref(), Some("sess-1"));
    assert_eq!(parsed.parent_thread_id.as_deref(), Some("parent-1"));
    assert_eq!(parsed.forked_from_id.as_deref(), Some("source-1"));
    assert_eq!(parsed.agent_nickname.as_deref(), Some("Darwin"));
    assert_eq!(parsed.agent_path.as_deref(), Some("/root/a"));
    assert_eq!(parsed.thread_source, Some(serde_json::json!("subagent")));
    assert_eq!(
        parsed.source,
        Some(serde_json::json!({"subagent":{"thread_spawn":{"depth":1}}}))
    );
    assert_eq!(parsed.originator.as_deref(), Some("codex"));
    assert_eq!(parsed.model_provider.as_deref(), Some("openai"));
    assert_eq!(parsed.cwd.as_deref(), Some("/workspace/sub"));
}

#[test]
fn first_session_meta_wins_and_absent_structural_fields_stay_absent() {
    let first = "{\"sessionId\":\"s1\",\"threadId\":\"thread-bridge\"}";
    let second =
        "{\"sessionId\":\"s2\",\"threadId\":\"thread-late\",\"parentThreadId\":\"parent-1\"}";
    let out = records(&[
        ok_line(1, &format!("\"sessionMeta\":{first}")),
        ok_line(2, &format!("\"sessionMeta\":{second}")),
    ]);
    let projection = parse_projection(&out, 2, Some(2)).unwrap();
    let parsed = projection.session_meta.expect("session meta parsed");
    assert_eq!(parsed.thread_id, "thread-bridge");
    assert_eq!(parsed.session_id.as_deref(), Some("s1"));
    assert_eq!(
        parsed.parent_thread_id, None,
        "later sessionMeta records do not overwrite the first"
    );
    assert_eq!(
        parsed.cwd, None,
        "absent cwd stays absent for older bridge projections"
    );
}

#[test]
fn changed_turns_are_parsed_and_merged_by_turn_id() {
    let turn_a_first =
        "{\"turnId\":\"turn-1\",\"status\":\"inProgress\",\"startedAt\":100,\"durationMs\":5}";
    let turn_a_last = "{\"turnId\":\"turn-1\",\"status\":\"completed\",\"startedAt\":100,\"completedAt\":200,\"durationMs\":100}";
    let turn_b = "{\"turnId\":\"turn-2\",\"status\":\"completed\",\"durationMs\":50}";
    let out = records(&[
        ok_line(1, &format!("\"changedTurns\":[{turn_a_first}]")),
        ok_line(2, &format!("\"changedTurns\":[{turn_a_last},{turn_b}]")),
    ]);
    let projection = parse_projection(&out, 2, Some(2)).unwrap();
    assert_eq!(projection.turns.len(), 2, "first-appearance order");
    let turn_1 = &projection.turns[0];
    assert_eq!(turn_1.turn_id, "turn-1");
    assert_eq!(turn_1.status.as_deref(), Some("completed"));
    assert_eq!(turn_1.started_at, Some(100));
    assert_eq!(turn_1.completed_at, Some(200));
    assert_eq!(turn_1.duration_ms, Some(100));
    let turn_2 = &projection.turns[1];
    assert_eq!(turn_2.turn_id, "turn-2");
    assert_eq!(turn_2.status.as_deref(), Some("completed"));
}

#[test]
fn turn_entries_without_identity_are_dropped() {
    let out = records(&[ok_line(1, "\"changedTurns\":[{\"status\":\"completed\"}]")]);
    let projection = parse_projection(&out, 1, Some(1)).unwrap();
    assert!(projection.turns.is_empty());
}
