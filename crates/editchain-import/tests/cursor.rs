//! Cursor tests for incremental file reading.

use blake3 as _;
use editchain_core as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

use editchain_import::cursor::split_lines;

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
