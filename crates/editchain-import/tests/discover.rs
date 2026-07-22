//! Session discovery tests.

use blake3 as _;
use editchain_core as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

use editchain_import::claude_code::discover::discover_sessions;

#[test]
fn discover_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = discover_sessions(dir.path()).unwrap();
    assert!(sessions.is_empty());
}

#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-length vec"
)]
#[test]
fn discover_single_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test-session.jsonl");
    std::fs::write(&path, "{}\n").unwrap();

    let sessions = discover_sessions(dir.path()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "test-session");
    assert!(!sessions[0].is_subagent);
}

#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-length vec"
)]
#[test]
fn discover_ignores_agent_files_at_top_level() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("agent-123.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.path().join("main.jsonl"), "{}\n").unwrap();

    let sessions = discover_sessions(dir.path()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "main");
}
