//! Bulk capture preserves source identities, resource bounds, and writer reuse.

use blake3 as _;
use editchain_core as _;
use editchain_engine as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use time as _;
use tokio as _;

use std::{cell::Cell, io, path::PathBuf};

use editchain_import::{
    capture_import, capture_import_file, discover_import_files, human::HumanImportRequest,
    BatchLimits, DiscoveryRequest, ImportOptions, ImportSource, MemoryBlobSink, MemoryCursorStore,
};
use editchain_store::{AppendLog, LogReadStats, LogStore, RecordVisitor};

#[derive(Default)]
struct CountedLog {
    reads: Cell<usize>,
    records: Vec<Vec<u8>>,
}

impl AppendLog for CountedLog {
    fn visit_records(&self, visitor: &mut RecordVisitor<'_>) -> io::Result<LogReadStats> {
        self.reads.set(self.reads.get().saturating_add(1));
        for bytes in &self.records {
            visitor(0, bytes)?;
        }
        Ok(LogReadStats::default())
    }

    fn append_record(&mut self, _flags: u8, encoded: &[u8]) -> io::Result<()> {
        self.records.push(encoded.to_vec());
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn many_files_fit_per_file_limits_and_only_replay_the_writer_once() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..64 {
        std::fs::write(dir.path().join(format!("{n:03}.jsonl")), b"{}\n").unwrap();
    }
    let request = HumanImportRequest {
        source: dir.path().to_owned(),
        recorded_root: None,
    };
    let source = ImportSource::Human(&request);
    let options = ImportOptions {
        batch_limits: BatchLimits {
            operations: 1,
            encoded_bytes: 1024,
        },
        ..ImportOptions::default()
    };
    let mut blobs = MemoryBlobSink::new();
    let mut cursors = MemoryCursorStore::new();
    assert!(capture_import(source, &options, &mut blobs, &cursors).is_err());
    let files = discover_import_files(source, &options).unwrap();
    let mut writer = LogStore::new(CountedLog::default());
    for file in &files {
        let batch = capture_import_file(source, file, &options, &mut blobs, &cursors).unwrap();
        assert_eq!(batch.operations().len(), 1);
        assert_eq!(
            batch
                .persist(&mut writer, &mut cursors)
                .unwrap()
                .admission
                .written,
            1
        );
    }
    for file in &files {
        let batch = capture_import_file(source, file, &options, &mut blobs, &cursors).unwrap();
        assert_eq!(
            batch
                .persist(&mut writer, &mut cursors)
                .unwrap()
                .admission
                .written,
            0
        );
    }
    let log = writer.into_inner();
    assert_eq!(log.reads.get(), 1);
    assert_eq!(log.records.len(), 64);
}

#[test]
fn nested_claude_selection_keeps_exact_records_and_parent_metadata() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("parent.jsonl"),
        include_bytes!("fixtures/claude/session.jsonl"),
    )
    .unwrap();
    let nested = dir.path().join("parent/subagents");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("agent-child.jsonl"),
        include_bytes!("fixtures/claude/session.jsonl"),
    )
    .unwrap();
    std::fs::write(
        nested.join("agent-child.meta.json"),
        b"{\"toolUseId\":\"spawn-child\"}",
    )
    .unwrap();
    let request = DiscoveryRequest {
        workspace_path: "/workspace".into(),
        sessions_dir: dir.path().to_owned(),
        chain_dir: PathBuf::new(),
    };
    let source = ImportSource::Claude(&request);
    let options = ImportOptions::default();
    let mut blobs = MemoryBlobSink::new();
    let cursors = MemoryCursorStore::new();
    let all = capture_import(source, &options, &mut blobs, &cursors).unwrap();
    let files = discover_import_files(source, &options).unwrap();
    assert_eq!(files.len(), 2);
    let mut collected = Vec::new();
    for file in &files {
        let batch = capture_import_file(source, file, &options, &mut blobs, &cursors).unwrap();
        collected.extend_from_slice(batch.operations());
    }
    assert_eq!(collected, all.operations());
}

#[test]
fn failed_file_does_not_advance_its_cursor_or_lose_prior_commits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jsonl"), b"{}\n").unwrap();
    std::fs::write(dir.path().join("b.jsonl"), b"{\"long\":true}\n").unwrap();
    let request = HumanImportRequest {
        source: dir.path().to_owned(),
        recorded_root: None,
    };
    let source = ImportSource::Human(&request);
    let mut limited = ImportOptions::default();
    limited.source_limits.source_bytes = 4;
    let files = discover_import_files(source, &limited).unwrap();
    let mut cursors = MemoryCursorStore::new();
    let mut blobs = MemoryBlobSink::new();
    let mut writer = LogStore::new(CountedLog::default());
    let first = files.first().unwrap();
    let second = files.get(1).unwrap();
    let batch = capture_import_file(source, first, &limited, &mut blobs, &cursors).unwrap();
    assert_eq!(
        batch
            .persist(&mut writer, &mut cursors)
            .unwrap()
            .admission
            .written,
        1
    );
    assert!(capture_import_file(source, second, &limited, &mut blobs, &cursors).is_err());
    let options = ImportOptions::default();
    for (file, expected) in [(first, 0), (second, 1)] {
        let batch = capture_import_file(source, file, &options, &mut blobs, &cursors).unwrap();
        assert_eq!(
            batch
                .persist(&mut writer, &mut cursors)
                .unwrap()
                .admission
                .written,
            expected
        );
    }
    assert_eq!(writer.into_inner().records.len(), 2);
}
