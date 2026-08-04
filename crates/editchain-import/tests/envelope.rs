//! Envelope parsing tests.

use blake3 as _;
use editchain_core as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

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

#[expect(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "test assertions on known-length vec"
)]
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

#[test]
fn parse_assistant_usage_and_message_id() {
    let json = br#"{"type":"assistant","uuid":"a-1","message":{"id":"msg-1","role":"assistant","model":"claude-opus-4-7","stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1666,"output_tokens":94,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"content":[{"type":"text","text":"hi"}]}}"#;
    let env = parse_envelope(json).unwrap();
    let msg = env.message.unwrap();
    assert_eq!(msg.id, "msg-1");
    assert_eq!(msg.model, "claude-opus-4-7");
    assert_eq!(msg.stop_reason.as_deref(), Some("end_turn"));
    let usage = msg.usage.unwrap();
    assert_eq!(usage.input_tokens, 1666);
    assert_eq!(usage.output_tokens, 94);
}

#[test]
fn parse_mode_event() {
    let json = br#"{"type":"mode","mode":"normal","sessionId":"s-1"}"#;
    let env = parse_envelope(json).unwrap();
    assert_eq!(env.record_type, "mode");
    assert_eq!(env.mode, "normal");
}

#[test]
fn parse_ai_title_event() {
    let json = br#"{"type":"ai-title","aiTitle":"Doing great - how can I help you today?","sessionId":"s-1"}"#;
    let env = parse_envelope(json).unwrap();
    assert_eq!(env.record_type, "ai-title");
    assert_eq!(env.ai_title, "Doing great - how can I help you today?");
}

#[test]
fn parse_slug_and_prompt_id() {
    let json = br#"{"type":"user","uuid":"u-1","slug":"boot-up-coo-golden-bird","promptId":"p-9","message":{"role":"user","content":"x"}}"#;
    let env = parse_envelope(json).unwrap();
    assert_eq!(env.slug, "boot-up-coo-golden-bird");
    assert_eq!(env.prompt_id, "p-9");
}

#[expect(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "test assertions on known-length vec"
)]
#[test]
fn parse_tool_result_array_content() {
    let json = br#"{"type":"user","uuid":"u-2","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":[{"type":"text","text":"line one"},{"type":"text","text":"line two"}]}]}}"#;
    let env = parse_envelope(json).unwrap();
    let msg = env.message.unwrap();
    match &msg.content[0] {
        editchain_import::claude_code::envelope::CcContentBlock::ToolResult { content, .. } => {
            assert_eq!(content, "line one\nline two");
        }
        _ => panic!("expected ToolResult"),
    }
}

#[expect(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "test assertions on known-length vec"
)]
#[test]
fn parse_thinking_signature() {
    let json = br#"{"type":"assistant","uuid":"a-2","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm","signature":"sig-abc"}]}}"#;
    let env = parse_envelope(json).unwrap();
    let msg = env.message.unwrap();
    match &msg.content[0] {
        editchain_import::claude_code::envelope::CcContentBlock::Thinking {
            thinking,
            signature,
        } => {
            assert_eq!(thinking, "hmm");
            assert_eq!(signature.as_deref(), Some("sig-abc"));
        }
        _ => panic!("expected Thinking"),
    }
}
