//! Streaming per-file pipeline: raw rollout JSONL -> deterministic NDJSON.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::io::{self};
use std::path::Path;

use codex_app_server_protocol::Turn;
use serde::Serialize;

use crate::project;
use crate::schema::*;

#[derive(Debug, Clone, Default)]
pub struct FileOptions {
    pub emit_final: bool,
}

#[derive(Debug, Clone)]
pub struct FileResult {
    pub path: String,
    pub physical_lines: u64,
    pub decoded_lines: u64,
    pub failed_lines: u64,
    pub records_written: u64,
}

/// Projects one rollout file to NDJSON on `out`.
///
/// Each non-blank physical line emits exactly one `line` record carrying its
/// 1-based physical ordinal; blank lines (including the trailing newline) emit
/// nothing. IO failures are returned to the caller; per-line decode failures
/// are captured inside the records so ordinal alignment is never lost.
pub fn process_file(
    path: &Path,
    opts: &FileOptions,
    out: &mut impl Write,
) -> io::Result<FileResult> {
    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;

    let mut file = File::open(path)?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic)?;
    let is_gzip = n == 2 && magic == [0x1f, 0x8b];
    file.seek(SeekFrom::Start(0))?;
    let reader = BufReader::new(file);
    let mut reader: Box<dyn BufRead> = if is_gzip {
        Box::new(BufReader::new(flate2::read::MultiGzDecoder::new(reader)))
    } else {
        Box::new(reader)
    };
    let source_path = path.to_string_lossy().into_owned();
    let mut result = process_lines(&mut *reader, &source_path, opts, out)?;
    result.path = source_path;
    Ok(result)
}

pub fn process_lines(
    reader: &mut dyn BufRead,
    source_path: &str,
    opts: &FileOptions,
    out: &mut impl Write,
) -> io::Result<FileResult> {
    let mut projector = project::SessionProjector::new();
    let mut tracker = SeenTracker::default();
    let mut physical_lines: u64 = 0;
    let mut decoded_lines: u64 = 0;
    let mut failed_lines: u64 = 0;
    let mut failed_ordinals: Vec<u64> = Vec::new();
    let mut first_session_meta: Option<SessionMetaProjection> = None;
    let mut inter_agent_messages: u64 = 0;
    let mut response_item_messages: u64 = 0;
    let mut compactions: u64 = 0;
    let mut records_written: u64 = 0;
    let mut buf: Vec<u8> = Vec::new();

    loop {
        buf.clear();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(n) => n,
            // flate2 readers surface clean end-of-stream as UnexpectedEof under
            // BufRead; treat it as EOF (any partial line still gets processed).
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                if buf.is_empty() {
                    break;
                }
                0
            }
            Err(err) => return Err(err),
        };
        if n == 0 && buf.is_empty() {
            break;
        }
        physical_lines += 1;

        let raw = match std::str::from_utf8(&buf) {
            Ok(s) => s,
            Err(_) => {
                failed_lines += 1;
                failed_ordinals.push(physical_lines);
                let record = invalid_utf8_record(source_path, physical_lines);
                write_record(out, &record)?;
                records_written += 1;
                continue;
            }
        };
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.as_bytes().iter().all(u8::is_ascii_whitespace) {
            continue;
        }

        let record = projector.line_record(source_path, physical_lines, line);
        if record.decode.status == DECODE_ERROR {
            failed_lines += 1;
            failed_ordinals.push(physical_lines);
        } else {
            decoded_lines += 1;
        }
        tracker.update(&record.projection.changed_items, physical_lines);
        // The physical rollout thread owns the file; subagent files embed the
        // parent's session_meta later, so the first meta line is authoritative.
        if first_session_meta.is_none() {
            first_session_meta.clone_from(&record.projection.session_meta);
        }
        if record.projection.inter_agent.is_some() {
            inter_agent_messages += 1;
        }
        response_item_messages = projector.response_item_message_count();
        if record.projection.compacted.is_some() {
            compactions += 1;
        }
        write_record(out, &record)?;
        records_written += 1;
    }

    if opts.emit_final {
        let (turns, registry) = projector.finish();
        let final_record = build_final_record(
            source_path,
            physical_lines,
            decoded_lines,
            failed_lines,
            failed_ordinals,
            &turns,
            &tracker,
            first_session_meta,
            inter_agent_messages,
            response_item_messages,
            compactions,
            &registry,
        );
        write_record(out, &final_record)?;
        records_written += 1;
    }

    Ok(FileResult {
        path: source_path.to_string(),
        physical_lines,
        decoded_lines,
        failed_lines,
        records_written,
    })
}

pub(crate) fn invalid_utf8_record(source_path: &str, source_ordinal: u64) -> LineRecord {
    LineRecord {
        schema_version: SCHEMA_VERSION.to_string(),
        record_type: RECORD_TYPE_LINE.to_string(),
        source_path: source_path.to_string(),
        source_ordinal,
        decode: DecodeInfo {
            status: DECODE_ERROR.to_string(),
            diagnostic: Some("line is not valid UTF-8".to_string()),
            kind: KIND_UNKNOWN_JSON.to_string(),
            event_type: None,
            rollout_ordinal: None,
            timestamp: None,
        },
        projection: ProjectionRecord::default(),
    }
}

fn write_record(out: &mut impl Write, record: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *out, record).map_err(io::Error::from)?;
    out.write_all(b"\n")
}

#[derive(Debug, Clone)]
struct ItemSeen {
    first: u64,
    last: u64,
    count: u64,
}

#[derive(Debug, Default)]
struct SeenTracker {
    items: HashMap<(String, String), ItemSeen>,
}

impl SeenTracker {
    fn update(&mut self, changes: &[ItemChange], ordinal: u64) {
        for change in changes {
            let key = (change.turn_id.clone(), change.item.id().to_string());
            let seen = self.items.entry(key).or_insert_with(|| ItemSeen {
                first: ordinal,
                last: ordinal,
                count: 0,
            });
            seen.last = ordinal;
            seen.count += 1;
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "final-session summary aggregates per-file counters, turn snapshots, builder turns, and reconciliation trackers"
)]
fn build_final_record(
    source_path: &str,
    physical_lines: u64,
    decoded_lines: u64,
    failed_lines: u64,
    failed_ordinals: Vec<u64>,
    turns: &[Turn],
    tracker: &SeenTracker,
    session_meta: Option<SessionMetaProjection>,
    inter_agent_messages: u64,
    response_item_messages: u64,
    compactions: u64,
    registry: &project::ResponseRegistry,
) -> FinalRecord {
    let mut turns = turns
        .iter()
        .map(|turn| TurnSummary {
            turn_id: turn.id.clone(),
            status: project::enum_str(&turn.status),
            error_message: turn.error.as_ref().map(|e| e.message.clone()),
            started_at: turn.started_at,
            completed_at: turn.completed_at,
            duration_ms: turn.duration_ms,
            item_count: turn.items.len(),
            items: turn
                .items
                .iter()
                .map(|item| {
                    let seen = tracker.items.get(&(turn.id.clone(), item.id().to_string()));
                    ItemRef {
                        item_id: item.id().to_string(),
                        kind: project::thread_item_kind_label(item).to_string(),
                        first_seen_ordinal: seen.map(|s| s.first).unwrap_or(physical_lines),
                        last_seen_ordinal: seen.map(|s| s.last).unwrap_or(physical_lines),
                        seen_line_count: seen.map(|s| s.count).unwrap_or(1),
                    }
                })
                .collect(),
        })
        .collect();
    merge_response_items(&mut turns, registry, tracker, physical_lines);
    FinalRecord {
        schema_version: SCHEMA_VERSION.to_string(),
        record_type: RECORD_TYPE_FINAL.to_string(),
        source_path: source_path.to_string(),
        source_ordinal: physical_lines,
        decode: DecodeInfo {
            status: DECODE_OK.to_string(),
            diagnostic: None,
            kind: "sessionSummary".to_string(),
            event_type: None,
            rollout_ordinal: None,
            timestamp: None,
        },
        physical_line_count: physical_lines,
        decoded_lines,
        failed_lines,
        failed_ordinals,
        thread_id: session_meta.as_ref().map(|m| m.thread_id.clone()),
        session_meta,
        turns,
        inter_agent_messages,
        response_item_messages,
        response_derived_items: registry.entries().len() as u64,
        compactions,
    }
}

/// Merges response-derived registry items into the per-turn final snapshot,
/// appending ItemRefs to matching builder turns and creating deterministic
/// synthetic turns for registry turn ids the builder never opened.
fn merge_response_items(
    summaries: &mut Vec<TurnSummary>,
    registry: &project::ResponseRegistry,
    tracker: &SeenTracker,
    physical_lines: u64,
) {
    let mut index_by_turn: HashMap<String, usize> = summaries
        .iter()
        .enumerate()
        .map(|(index, summary)| (summary.turn_id.clone(), index))
        .collect();
    for entry in registry.entries() {
        let seen = tracker
            .items
            .get(&(entry.turn_id.clone(), entry.item_id.clone()));
        let item_ref = ItemRef {
            item_id: entry.item_id.clone(),
            kind: entry.kind_label.to_string(),
            first_seen_ordinal: seen.map(|s| s.first).unwrap_or(physical_lines),
            last_seen_ordinal: seen.map(|s| s.last).unwrap_or(physical_lines),
            seen_line_count: seen.map(|s| s.count).unwrap_or(1),
        };
        let summary_index = if let Some(&index) = index_by_turn.get(&entry.turn_id) {
            index
        } else {
            let index = summaries.len();
            index_by_turn.insert(entry.turn_id.clone(), index);
            summaries.push(TurnSummary {
                turn_id: entry.turn_id.clone(),
                status: None,
                error_message: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
                item_count: 0,
                items: Vec::new(),
            });
            index
        };
        summaries[summary_index].items.push(item_ref);
        summaries[summary_index].item_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const AGENT_LINE: &str = r#"{"timestamp":"2026-08-17T03:25:48.586Z","type":"event_msg","payload":{"type":"agent_message","message":"same text","phase":null,"memory_citation":null}}"#;
    const ECHO_LINE: &str = r#"{"timestamp":"2026-08-17T03:25:48.586Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"same text"}]}}"#;

    fn run(lines: &[&str], emit_final: bool) -> Vec<String> {
        let input = lines.join("\n") + "\n";
        let mut reader = Cursor::new(input);
        let mut out = Vec::new();
        let opts = FileOptions { emit_final };
        process_lines(&mut reader, "test.jsonl", &opts, &mut out).expect("process");
        String::from_utf8(out)
            .expect("utf8")
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn agent_message_echo_folds_to_one_logical_item_with_same_content_hash() {
        let records = run(&[AGENT_LINE, ECHO_LINE], false);
        assert_eq!(records.len(), 2);
        let first: serde_json::Value = serde_json::from_str(&records[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(&records[1]).unwrap();

        assert_eq!(first["decode"]["status"], "ok");
        assert_eq!(first["decode"]["kind"], "eventMsg");
        assert_eq!(first["decode"]["eventType"], "agent_message");
        assert_eq!(first["sourceOrdinal"], 1);
        assert_eq!(
            first["projection"]["changedItems"][0]["item"]["kind"],
            "agentMessage"
        );
        assert_eq!(
            first["projection"]["changedItems"][0]["item"]["text"],
            "same text"
        );

        assert_eq!(second["decode"]["kind"], "responseItem");
        assert_eq!(second["sourceOrdinal"], 2);
        // The echo folds onto the materialized item: same id, same hash, full text.
        let echo_item = &second["projection"]["changedItems"][0]["item"];
        assert_eq!(
            echo_item["id"],
            first["projection"]["changedItems"][0]["item"]["id"]
        );
        assert_eq!(echo_item["kind"], "agentMessage");
        assert_eq!(echo_item["text"], "same text");
        assert_eq!(
            echo_item["contentHash"],
            first["projection"]["changedItems"][0]["item"]["contentHash"]
        );
    }

    #[test]
    fn final_reconciliation_includes_response_derived_items() {
        let session_meta = r#"{"timestamp":"t","type":"session_meta","payload":{"session_id":"S1","id":"T1","timestamp":"t","cwd":"/tmp","originator":"test","cli_version":"1.0","source":"vscode"}}"#;
        let task_started = r#"{"timestamp":"t","ordinal":0,"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","started_at":1786934553,"model_context_window":null,"collaboration_mode_kind":"default"}}"#;
        let user_msg = r#"{"timestamp":"t","ordinal":1,"type":"response_item","payload":{"type":"message","id":"msg_user_1","role":"user","content":[{"type":"input_text","text":"audit the tree"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}"#;
        let agent_msg = r#"{"timestamp":"t","ordinal":2,"type":"response_item","payload":{"type":"message","id":"msg_agent_1","role":"assistant","content":[{"type":"output_text","text":"working"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}"#;
        let records = run(&[session_meta, task_started, user_msg, agent_msg], true);
        let final_rec: serde_json::Value = serde_json::from_str(records.last().unwrap()).unwrap();
        assert_eq!(final_rec["recordType"], "final");
        assert_eq!(final_rec["responseDerivedItems"], 2);
        assert_eq!(final_rec["responseItemMessages"], 2);
        let items = &final_rec["turns"][0]["items"];
        let ids: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["itemId"].as_str())
            .collect();
        assert!(ids.contains(&"msg_user_1"));
        assert!(ids.contains(&"msg_agent_1"));
        let user_ref = items
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["itemId"] == "msg_user_1")
            .unwrap();
        assert_eq!(user_ref["kind"], "userMessage");
        assert_eq!(user_ref["firstSeenOrdinal"], 3);
        assert_eq!(user_ref["lastSeenOrdinal"], 3);
        assert_eq!(user_ref["seenLineCount"], 1);
    }

    #[test]
    fn lifecycle_upsert_shares_item_id_and_final_snapshot_dedups() {
        let task_started = r#"{"timestamp":"t","ordinal":0,"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","started_at":1786934553,"model_context_window":null,"collaboration_mode_kind":"default"}}"#;
        let started = r#"{"timestamp":"t","ordinal":1,"type":"event_msg","payload":{"type":"item_completed","thread_id":"11111111-1111-7111-8111-111111111111","turn_id":"turn-1","item":{"type":"SubAgentActivity","id":"sub-1","kind":"started","agent_thread_id":"22222222-2222-7222-8222-222222222222","agent_path":"/root/a"}}}"#;
        let interacted = r#"{"timestamp":"t","ordinal":2,"type":"event_msg","payload":{"type":"item_completed","thread_id":"11111111-1111-7111-8111-111111111111","turn_id":"turn-1","item":{"type":"SubAgentActivity","id":"sub-1","kind":"interacted","agent_thread_id":"22222222-2222-7222-8222-222222222222","agent_path":"/root/a"}}}"#;
        let records = run(&[task_started, started, interacted], true);
        assert_eq!(records.len(), 4);

        let first: serde_json::Value = serde_json::from_str(&records[1]).unwrap();
        let second: serde_json::Value = serde_json::from_str(&records[2]).unwrap();
        assert_eq!(
            first["projection"]["changedItems"][0]["item"]["id"],
            "sub-1"
        );
        assert_eq!(
            second["projection"]["changedItems"][0]["item"]["id"],
            "sub-1"
        );
        assert_eq!(
            second["projection"]["changedItems"][0]["item"]["kind"],
            "subAgentActivity"
        );

        let final_rec: serde_json::Value = serde_json::from_str(&records[3]).unwrap();
        assert_eq!(final_rec["recordType"], "final");
        assert_eq!(final_rec["turns"][0]["items"].as_array().unwrap().len(), 1);
        let item_ref = &final_rec["turns"][0]["items"][0];
        assert_eq!(item_ref["itemId"], "sub-1");
        assert_eq!(item_ref["seenLineCount"], 2);
        assert_eq!(item_ref["firstSeenOrdinal"], 2);
        assert_eq!(item_ref["lastSeenOrdinal"], 3);
    }

    #[test]
    fn output_is_byte_deterministic_across_runs() {
        let lines = [
            r#"{"timestamp":"t","type":"session_meta","payload":{"session_id":"S1","id":"T1","timestamp":"t","cwd":"/tmp","originator":"test","cli_version":"1.0","source":"vscode"}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","started_at":1786934553,"model_context_window":null,"collaboration_mode_kind":"default"}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"hello","local_images":[],"local_audio":[],"text_elements":[]}}"#,
            AGENT_LINE,
            ECHO_LINE,
        ];
        let a = run(&lines, true);
        let b = run(&lines, true);
        assert_eq!(a, b);
        assert_eq!(a.len(), 6); // 4 non-blank lines + 1 blank skipped + final
    }

    #[test]
    fn blank_lines_are_skipped_but_ordinals_remain_physical() {
        let records = run(&["", AGENT_LINE, "", ECHO_LINE], true);
        assert_eq!(records.len(), 3);
        let first: serde_json::Value = serde_json::from_str(&records[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(&records[1]).unwrap();
        let final_rec: serde_json::Value = serde_json::from_str(&records[2]).unwrap();
        assert_eq!(first["sourceOrdinal"], 2);
        assert_eq!(second["sourceOrdinal"], 4);
        assert_eq!(final_rec["physicalLineCount"], 4);
        assert_eq!(final_rec["sourceOrdinal"], 4);
    }

    #[test]
    fn non_ascii_whitespace_is_a_record_not_a_blank_line() {
        let records = run(&["\u{00a0}"], true);
        assert_eq!(records.len(), 2, "line record plus final record");
        let line: serde_json::Value = serde_json::from_str(&records[0]).unwrap();
        assert_eq!(line["sourceOrdinal"], 1);
        assert_eq!(line["decode"]["status"], "error");
    }

    #[test]
    fn malformed_and_unknown_lines_keep_ordinal_alignment() {
        let lines = [
            AGENT_LINE,
            "this is not json {",
            r#"{"timestamp":"t","type":"future_gadget","payload":{"x":1}}"#,
            ECHO_LINE,
        ];
        let records = run(&lines, true);
        let second: serde_json::Value = serde_json::from_str(&records[1]).unwrap();
        let third: serde_json::Value = serde_json::from_str(&records[2]).unwrap();
        let final_rec: serde_json::Value = serde_json::from_str(&records[4]).unwrap();

        assert_eq!(second["decode"]["status"], "error");
        assert_eq!(second["decode"]["kind"], "unknownJson");
        assert!(second["decode"]["diagnostic"].is_string());
        assert_eq!(second["sourceOrdinal"], 2);

        assert_eq!(third["decode"]["status"], "error");
        assert_eq!(third["decode"]["kind"], "future_gadget");
        assert_eq!(third["sourceOrdinal"], 3);

        assert_eq!(final_rec["failedLines"], 2);
        assert_eq!(final_rec["failedOrdinals"], serde_json::json!([2, 3]));
    }

    #[test]
    fn gzip_input_is_decoded_transparently() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write as _;

        let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
        gz.write_all((AGENT_LINE.to_string() + "\n").as_bytes())
            .unwrap();
        let compressed = gz.finish().unwrap();

        let dir = std::env::temp_dir().join("codex-session-exporter-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-test.jsonl.gz");
        std::fs::write(&path, &compressed).unwrap();

        let mut out = Vec::new();
        let opts = FileOptions { emit_final: true };
        let result = process_file(&path, &opts, &mut out).expect("process gz");
        assert_eq!(result.decoded_lines, 1);
        let text = String::from_utf8(out).unwrap();
        let records: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["decode"]["kind"], "eventMsg");
        assert_eq!(records[1]["recordType"], "final");
        let _ = std::fs::remove_file(&path);
    }
}
