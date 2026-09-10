//! Reader tests for session file streaming.

use blake3 as _;
use editchain_codec as _;
use editchain_core as _;
use editchain_project as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use time as _;
use tokio as _;

use editchain_import::claude_code::reader::read_session_file;
use std::io::Write;

#[test]
fn read_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.jsonl");
    std::fs::write(&path, b"").unwrap();

    let (lines, bytes, cursor) = read_session_file(&path, None).unwrap();
    assert!(lines.is_empty());
    assert_eq!(bytes, 0);
    assert_eq!(cursor.byte_offset, 0);
}

#[test]
fn capture_and_later_replay_share_cancellation() {
    use editchain_import::source_read::{SourceReadControl, SourceReadPlan};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jsonl");
    std::fs::write(&path, "first\nsecond\n").unwrap();
    let control = SourceReadControl::default();
    let plan = SourceReadPlan::capture_controlled(&path, None, 0, None, &control).unwrap();
    assert_eq!(plan.lines().len(), 2);
    control.cancellation.cancel();
    assert!(matches!(
        plan.all_lines(),
        Err(editchain_import::ImportError::Cancelled { .. })
    ));
    assert!(matches!(
        SourceReadPlan::capture_controlled(&path, None, 0, None, &control),
        Err(editchain_import::ImportError::Cancelled { .. })
    ));
}

#[test]
fn read_single_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("single.jsonl");
    std::fs::write(&path, b"{\"type\":\"test\"}\n").unwrap();

    let (lines, bytes, _) = read_session_file(&path, None).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(bytes, 16);
}

#[test]
fn read_partial_final_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.jsonl");
    std::fs::write(&path, b"{\"type\":\"a\"}\n{\"type\":\"b").unwrap();

    let (lines, bytes, cursor) = read_session_file(&path, None).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(bytes, 13); // only the complete line
    assert_eq!(cursor.byte_offset, 13); // partial not counted
}

#[test]
fn read_appended_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("append.jsonl");

    // Write first line.
    std::fs::write(&path, b"line1\n").unwrap();
    let (lines1, _, cursor1) = read_session_file(&path, None).unwrap();
    assert_eq!(lines1.len(), 1);

    // Append second line.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "line2").unwrap();
    drop(f);

    // Read from cursor.
    let (lines2, _, cursor2) = read_session_file(&path, Some(&cursor1)).unwrap();
    assert_eq!(lines2.len(), 1);
    assert_eq!(cursor2.byte_offset, 12); // 6 + 6 bytes
}

#[test]
fn incremental_reader_rejects_changed_accepted_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jsonl");
    std::fs::write(&path, b"alpha\n").unwrap();
    let (_, _, cursor) = read_session_file(&path, None).unwrap();
    for changed in [b"ALPHA\n".as_slice(), b"ALPHA\nbeta\n"] {
        std::fs::write(&path, changed).unwrap();
        assert!(matches!(
            read_session_file(&path, Some(&cursor)),
            Err(editchain_import::ImportError::SourceGenerationChanged { .. })
        ));
    }
}

#[test]
fn plan_retains_captured_bytes_through_live_rewrite_and_partial_completion() {
    use editchain_import::source_read::{SourceReadLimits, SourceReadPlan, SourceReadState};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jsonl");
    std::fs::write(&path, b"alpha\npartial").unwrap();
    let plan = SourceReadPlan::capture(&path, None, 0, SourceReadLimits::default()).unwrap();
    let captured = plan.captured_path().to_path_buf();
    assert_eq!(captured.file_name(), path.file_name());
    assert_eq!(plan.state(), SourceReadState::Fresh);
    assert_eq!(plan.partial(), Some(false));
    assert_eq!(plan.checkpoint().byte_offset, 6);
    std::fs::write(&path, b"ALPHA\nchanged\n").unwrap();
    assert_eq!(
        std::fs::read(plan.captured_path()).unwrap(),
        b"alpha\npartial"
    );
    assert_eq!(plan.all_lines().unwrap().first().unwrap().data, b"alpha\n");
    let rewritten = SourceReadPlan::capture(
        &path,
        Some(plan.checkpoint()),
        7,
        SourceReadLimits::default(),
    )
    .unwrap();
    assert_eq!(rewritten.state(), SourceReadState::Rewritten);
    assert_eq!(rewritten.generation(), 8);
    assert_eq!(rewritten.start_seq(), 0);
    assert_eq!(rewritten.lines().len(), 2);
    assert_eq!(rewritten.partial(), None);
    drop(plan);
    assert!(!captured.exists());
}

#[test]
fn only_complete_accepted_bytes_constrain_source_continuity() {
    use editchain_import::source_read::{SourceReadLimits, SourceReadPlan, SourceReadState};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jsonl");
    std::fs::write(&path, b"alpha\nlong partial").unwrap();
    let (_, _, cursor) = read_session_file(&path, None).unwrap();
    std::fs::write(&path, b"alpha\n ").unwrap();
    let same =
        SourceReadPlan::capture(&path, Some(&cursor), 3, SourceReadLimits::default()).unwrap();
    assert_eq!(same.state(), SourceReadState::Unchanged);
    assert_eq!(same.generation(), 3);
    assert_eq!(same.partial(), Some(true));
    assert!(same.lines().is_empty());
    std::fs::write(&path, b"alpha\n \n").unwrap();
    let appended = SourceReadPlan::capture(
        &path,
        Some(same.checkpoint()),
        3,
        SourceReadLimits::default(),
    )
    .unwrap();
    assert_eq!(appended.state(), SourceReadState::Append);
    assert_eq!(appended.start_seq(), 1);
    assert_eq!(appended.lines().first().unwrap().data, b" \n");
    assert_eq!(appended.checkpoint().ops_emitted, 2);
    assert_eq!(
        appended.checkpoint().content_hash,
        editchain_import::hash_raw(b"alpha\n \n")
    );
}

#[test]
fn source_record_and_generation_limits_fail_explicitly() {
    use editchain_import::source_read::{SourceReadLimits, SourceReadPlan};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jsonl");
    std::fs::write(&path, b"alpha\nbeta\n").unwrap();
    for (limits, resource) in [
        (
            SourceReadLimits {
                source_bytes: 4,
                ..SourceReadLimits::default()
            },
            "source bytes",
        ),
        (
            SourceReadLimits {
                record_bytes: 4,
                ..SourceReadLimits::default()
            },
            "record bytes",
        ),
        (
            SourceReadLimits {
                records: 1,
                ..SourceReadLimits::default()
            },
            "source records",
        ),
    ] {
        assert!(matches!(
            SourceReadPlan::capture(&path, None, 0, limits),
            Err(editchain_import::ImportError::ResourceLimit { resource: failed, .. }) if failed == resource
        ));
    }
    let (_, _, cursor) = read_session_file(&path, None).unwrap();
    std::fs::write(&path, b"changed\n").unwrap();
    assert!(matches!(
        SourceReadPlan::capture(&path, Some(&cursor), u32::MAX, SourceReadLimits::default()),
        Err(editchain_import::ImportError::CursorStore(detail)) if detail.contains("generation exhausted")
    ));
    // A giant partial record is also bounded before any capture can be accepted.
    std::fs::write(&path, b"partial").unwrap();
    assert!(matches!(
        SourceReadPlan::capture(
            &path,
            None,
            0,
            SourceReadLimits {
                record_bytes: 4,
                ..SourceReadLimits::default()
            }
        ),
        Err(editchain_import::ImportError::ResourceLimit {
            resource: "record bytes",
            ..
        })
    ));
}
