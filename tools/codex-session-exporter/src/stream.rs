//! Retained provider reducers over bounded, ordered batches from a native collector.

use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::project::SessionProjector;

pub const STREAM_SCHEMA: &str = "editchain-stream-v1";
const MAX_BATCH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SESSIONS: usize = 64;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Batch {
    pub schema: String,
    pub source: String,
    pub generation: u32,
    pub after: u64,
    #[serde(default)]
    pub reset: bool,
    /// Null represents one complete physical line with invalid UTF-8. The
    /// collector retains its exact bytes in the raw evidence lane.
    pub lines: Vec<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub schema: String,
    pub through: u64,
    pub records: Vec<Value>,
    pub records_projected: u64,
    pub error: Option<String>,
}

struct Session {
    projector: SessionProjector,
    through: u64,
    generation: u32,
    touched: u64,
    last: Option<(Batch, Reply)>,
}

#[derive(Default)]
pub struct StreamProjector {
    sessions: HashMap<String, Session>,
    clock: u64,
}

impl StreamProjector {
    pub fn apply(&mut self, batch: Batch) -> Reply {
        let error = |message: &str| Reply {
            schema: STREAM_SCHEMA.into(),
            through: batch.after,
            records: Vec::new(),
            records_projected: 0,
            error: Some(message.into()),
        };
        if batch.schema != STREAM_SCHEMA || batch.source.is_empty() {
            return error("unsupported stream schema or empty source");
        }
        if batch.lines.iter().flatten().any(|line| {
            !line.ends_with('\n') || line.strip_suffix('\n').unwrap_or(line).contains('\n')
        }) {
            return error("each input must be one complete physical line");
        }
        let Some(through) = batch.after.checked_add(batch.lines.len() as u64) else {
            return error("source ordinal exhausted");
        };
        self.clock = self.clock.saturating_add(1);
        if let Some(session) = self.sessions.get_mut(&batch.source) {
            if let Some((previous, reply)) = &session.last {
                if *previous == batch {
                    let mut reply = reply.clone();
                    reply.records_projected = 0;
                    session.touched = self.clock;
                    return reply;
                }
            }
        }
        if batch.reset {
            if batch.after != 0 {
                return error("reset must begin at ordinal zero");
            }
            if !self.sessions.contains_key(&batch.source) && self.sessions.len() >= MAX_SESSIONS {
                if let Some(oldest) = self
                    .sessions
                    .iter()
                    .min_by_key(|(_, session)| session.touched)
                    .map(|(key, _)| key.clone())
                {
                    self.sessions.remove(&oldest);
                }
            }
            self.sessions.insert(
                batch.source.clone(),
                Session {
                    projector: SessionProjector::new(),
                    through: 0,
                    generation: batch.generation,
                    touched: self.clock,
                    last: None,
                },
            );
        }
        let Some(session) = self.sessions.get_mut(&batch.source) else {
            return error("source reducer unavailable; bootstrap required");
        };
        if session.generation != batch.generation || session.through != batch.after {
            return error("source generation or ordinal gap; bootstrap required");
        }
        let mut records = Vec::new();
        for (index, line) in batch.lines.iter().enumerate() {
            let ordinal = batch.after + index as u64 + 1;
            let Some(line) = line else {
                records.push(
                    serde_json::to_value(crate::bridge::invalid_utf8_record(
                        &batch.source,
                        ordinal,
                    ))
                    .expect("line records are serializable"),
                );
                continue;
            };
            let line = line.strip_suffix('\n').unwrap_or(line);
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.as_bytes().iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let record = session.projector.line_record(&batch.source, ordinal, line);
            match serde_json::to_value(record) {
                Ok(record) => records.push(record),
                Err(_) => {
                    return error("provider projection cannot be serialized; bootstrap required")
                }
            }
        }
        session.through = through;
        session.touched = self.clock;
        let reply = Reply {
            schema: STREAM_SCHEMA.into(),
            through,
            records_projected: records.len() as u64,
            records,
            error: None,
        };
        session.last = Some((batch, reply.clone()));
        reply
    }
}

pub fn serve(mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut projector = StreamProjector::default();
    loop {
        let mut line = Vec::new();
        let count = (&mut input)
            .take(MAX_BATCH_BYTES + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            return Ok(());
        }
        if count as u64 > MAX_BATCH_BYTES || line.last() != Some(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete or oversized stream batch",
            ));
        }
        let batch: Batch = serde_json::from_slice(&line).map_err(io::Error::other)?;
        serde_json::to_writer(&mut output, &projector.apply(batch)).map_err(io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_batch_partition_and_retry_matches_the_offline_bridge() {
        let lines = [
            Some("{\"timestamp\":\"t\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn-1\"}}\n".to_owned()),
            Some("{\"timestamp\":\"t\",\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"working\"}}\n".to_owned()),
            Some("{\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"working\"}]}}\n".to_owned()),
            None,
            Some(" \r\n".to_owned()),
            Some("{invalid}\n".to_owned()),
            Some("{\"timestamp\":\"t\",\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_rolled_back\",\"num_turns\":1}}\n".to_owned()),
        ];
        let bytes: Vec<u8> = lines
            .iter()
            .flat_map(|line| {
                line.as_ref()
                    .map_or(vec![255, 10], |line| line.as_bytes().to_vec())
            })
            .collect();
        let mut offline = Vec::new();
        crate::bridge::process_lines(
            &mut io::Cursor::new(bytes),
            "test.jsonl",
            &crate::bridge::FileOptions { emit_final: false },
            &mut offline,
        )
        .unwrap();
        let expected: Vec<Value> = String::from_utf8(offline)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for size in 1..=lines.len() {
            let mut stream = StreamProjector::default();
            let mut through = 0;
            let mut actual = Vec::new();
            for chunk in lines.chunks(size) {
                let batch = Batch {
                    schema: STREAM_SCHEMA.into(),
                    source: "test.jsonl".into(),
                    generation: 0,
                    after: through,
                    reset: through == 0,
                    lines: chunk.to_vec(),
                };
                let reply = stream.apply(batch.clone());
                assert!(reply.error.is_none(), "{:?}", reply.error);
                let replay = stream.apply(batch);
                assert_eq!(replay.records, reply.records);
                assert_eq!(replay.records_projected, 0);
                through = reply.through;
                actual.extend(reply.records);
            }
            assert_eq!(actual, expected, "partition size {size}");
            let gap = Batch {
                schema: STREAM_SCHEMA.into(),
                source: "test.jsonl".into(),
                generation: 0,
                after: through + 1,
                reset: false,
                lines: vec![Some("{}\n".into())],
            };
            assert!(stream.apply(gap).error.is_some());
        }
    }
}
