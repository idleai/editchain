//! Reader tests for session file streaming.

use blake3 as _;
use editchain_core as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;

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
