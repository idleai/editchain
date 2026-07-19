use editchain_import::claude_code::envelope::parse_envelope;

#[test]
fn parse_user_message() {
    let json = br#"{"type":"user","uuid":"abc-123","timestamp":"2025-01-01T00:00:00Z","message":{"role":"user","content":"hello world"}}"#;
    let env = parse_envelope(json).unwrap();
    assert_eq!(env.record_type, "user");
    assert_eq!(env.uuid, "abc-123");
    assert!(env.message.is_some());
    assert_eq!(env.message.unwrap().role, "user");
}

#[test]
fn parse_assistant_with_tool_use() {
    let json = br#"{"type":"assistant","uuid":"def-456","message":{"role":"assistant","content":[{"type":"text","text":"checking"},{"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"ls"}}]}}"#;
    let env = parse_envelope(json).unwrap();
    assert_eq!(env.record_type, "assistant");
    let msg = env.message.unwrap();
    assert_eq!(msg.content.len(), 2);
    match &msg.content[1] {
        editchain_import::claude_code::envelope::CcContentBlock::ToolUse { id, name, .. } => {
            assert_eq!(id, "call-1");
            assert_eq!(name, "Bash");
        }
        _ => panic!("expected ToolUse"),
    }
}

#[test]
fn parse_malformed_returns_none() {
    assert!(parse_envelope(b"not json").is_none());
}

#[test]
fn parse_empty_type_returns_none() {
    assert!(parse_envelope(b"{}").is_none());
}

#[test]
fn parse_system_entry() {
    let json = br#"{"type":"system","subtype":"turn_duration","uuid":"sys-001","durationMs":1000}"#;
    let env = parse_envelope(json).unwrap();
    assert_eq!(env.record_type, "system");
    assert_eq!(env.subtype, "turn_duration");
}

#[test]
fn parse_background_session_fields() {
    let json = br#"{"type":"user","uuid":"bg-001","sessionId":"bg-session-1","agentId":"agent-1","userType":"external","entrypoint":"sdk-ts","version":"2.1.195","message":{"role":"user","content":"work"}}"#;
    let env = parse_envelope(json).unwrap();
    assert_eq!(env.session_id, "bg-session-1");
    assert_eq!(env.agent_id, "agent-1");
    assert_eq!(env.user_type, "external");
    assert_eq!(env.entrypoint, "sdk-ts");
    assert_eq!(env.version, "2.1.195");
}