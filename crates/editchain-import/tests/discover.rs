use editchain_import::claude_code::discover::discover_sessions;

#[test]
fn discover_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = discover_sessions(dir.path()).unwrap();
    assert!(sessions.is_empty());
}

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

#[test]
fn discover_ignores_agent_files_at_top_level() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("agent-123.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.path().join("main.jsonl"), "{}\n").unwrap();

    let sessions = discover_sessions(dir.path()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "main");
}