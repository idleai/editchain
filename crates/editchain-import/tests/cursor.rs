//! Cursor tests for incremental file reading.

use blake3 as _;
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

use editchain_import::claude_code::reader::read_session_file;
use editchain_import::cursor::{check_file_generation, split_lines};
use editchain_import::ImportError;

#[test]
fn split_lines_complete() {
    let data = b"line1\nline2\nline3\n";
    let (lines, remainder) = split_lines(data);
    assert_eq!(lines.len(), 3);
    assert!(remainder.is_empty());
}

#[test]
fn split_lines_partial_final() {
    let data = b"line1\nline2\npartial";
    let (lines, remainder) = split_lines(data);
    assert_eq!(lines.len(), 2);
    assert_eq!(remainder, b"partial");
}

#[test]
fn split_lines_empty() {
    let data = b"";
    let (lines, remainder) = split_lines(data);
    assert!(lines.is_empty());
    assert!(remainder.is_empty());
}

#[test]
fn exact_prefix_hash_detects_same_size_and_grown_rewrites() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jsonl");
    std::fs::write(&path, b"alpha\nbeta\n").unwrap();
    let (_lines, _bytes, cursor) = read_session_file(&path, None).unwrap();

    let mut unchanged = cursor.clone();
    assert!(check_file_generation(&path, &mut unchanged).unwrap());
    assert_eq!(unchanged.content_hash_version, 1);

    std::fs::write(&path, b"ALPHA\nbeta\n").unwrap();
    let mut same_size = cursor.clone();
    assert!(matches!(
        check_file_generation(&path, &mut same_size),
        Err(ImportError::SourceGenerationChanged { .. })
    ));

    std::fs::write(&path, b"ALPHA\nbeta\ngamma\n").unwrap();
    let mut grown = cursor;
    assert!(matches!(
        check_file_generation(&path, &mut grown),
        Err(ImportError::SourceGenerationChanged { .. })
    ));
}

#[test]
fn partial_tail_becomes_append_only_when_a_line_completes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jsonl");
    std::fs::write(&path, b"alpha\n").unwrap();
    let (_lines, _bytes, mut cursor) = read_session_file(&path, None).unwrap();

    std::fs::write(&path, b"alpha\npartial").unwrap();
    assert!(check_file_generation(&path, &mut cursor).unwrap());

    std::fs::write(&path, b"alpha\npartial\n").unwrap();
    assert!(!check_file_generation(&path, &mut cursor).unwrap());
}
