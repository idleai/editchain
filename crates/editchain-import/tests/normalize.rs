//! Normalization tests for Claude Code envelopes.

use blake3 as _;
use editchain_project as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

use editchain_core::{op::OpKind, payload::Payload, tags::Tags};
use editchain_import::claude_code::envelope::parse_envelope;
use editchain_import::claude_code::normalize::{
    is_metadata_record, normalize_envelope, NormalizeOptions,
};
use editchain_import::ids::{derive_node_id, hash_raw, SourceStream};
use editchain_import::sink::MemoryBlobSink;

#[test]
fn test_timestamp_parsing() {
    let ts = editchain_import::claude_code::normalize::parse_timestamp("2026-07-09T18:56:19.739Z");
    assert!(ts > 1_700_000_000_000);
    assert!(ts < 1_800_000_000_000);
}

#[test]
fn test_timestamp_no_millis() {
    let ts = editchain_import::claude_code::normalize::parse_timestamp("2026-07-09T18:56:19Z");
    assert!(ts > 1_700_000_000_000);
}

#[test]
fn test_empty_timestamp() {
    assert_eq!(
        editchain_import::claude_code::normalize::parse_timestamp(""),
        0
    );
}

#[test]
fn test_whitespace_only_assistant_is_metadata() {
    // An assistant turn with only whitespace text (no tool call) is a streaming
    // artifact — classified as metadata so it bundles into a real node.
    let json = br#"{"type":"assistant","uuid":"abc","sessionId":"sess-1","timestamp":"2026-07-09T18:56:19.739Z","message":{"role":"assistant","content":[{"type":"text","text":"\n\n\n"}]}}"#;
    let env = parse_envelope(json).unwrap();
    assert!(is_metadata_record(&env));
}

#[test]
fn test_assistant_with_text_is_not_metadata() {
    // An assistant turn with real prose is NOT metadata.
    let json = br#"{"type":"assistant","uuid":"abc","sessionId":"sess-1","timestamp":"2026-07-09T18:56:19.739Z","message":{"role":"assistant","content":[{"type":"text","text":"hello world"}]}}"#;
    let env = parse_envelope(json).unwrap();
    assert!(!is_metadata_record(&env));
}

#[test]
fn test_assistant_with_tool_use_is_not_metadata() {
    // An assistant turn with a tool call is NOT metadata (even if text is empty).
    let json = br#"{"type":"assistant","uuid":"abc","sessionId":"sess-1","timestamp":"2026-07-09T18:56:19.739Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"Bash","input":{}}]}}"#;
    let env = parse_envelope(json).unwrap();
    assert!(!is_metadata_record(&env));
}

#[expect(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "test assertions on known-length vec"
)]
#[test]
fn test_normalize_user_message() {
    let json = br#"{"type":"user","uuid":"abc","sessionId":"sess-1","timestamp":"2026-07-09T18:56:19.739Z","message":{"role":"user","content":"hello world"}}"#;
    let env = parse_envelope(json).unwrap();
    let stream = SourceStream::new(derive_node_id("/test"), 0);
    let hash = hash_raw(json);
    let mut blobs = MemoryBlobSink::new();

    let (raw, norm) = normalize_envelope(
        &env,
        hash,
        json,
        &stream,
        1,
        &NormalizeOptions::default(),
        &mut blobs,
        "sess-1",
    );

    assert!(matches!(raw.kind, OpKind::Import(_)));
    assert!(raw.tags.matches_any(Tags::IMPORT));

    assert_eq!(norm.len(), 1);

    match &norm[0].kind {
        OpKind::Message(msg) => match &msg.content {
            Payload::Inline(bytes) => assert_eq!(bytes.as_slice(), b"hello world"),
            _ => panic!("expected inline payload"),
        },
        _ => panic!("expected MessageOp"),
    }
}

#[expect(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "test assertions on known-length vec"
)]
#[test]
fn test_normalize_mode_event() {
    let json = br#"{"type":"mode","mode":"plan","sessionId":"sess-1","timestamp":"2026-07-09T18:56:19.739Z"}"#;
    let env = parse_envelope(json).unwrap();
    let stream = SourceStream::new(derive_node_id("/test"), 0);
    let hash = hash_raw(json);
    let mut blobs = MemoryBlobSink::new();

    let (raw, norm) = normalize_envelope(
        &env,
        hash,
        json,
        &stream,
        1,
        &NormalizeOptions::default(),
        &mut blobs,
        "sess-1",
    );

    assert!(matches!(raw.kind, OpKind::Import(_)));
    assert_eq!(norm.len(), 1);
    match &norm[0].kind {
        OpKind::Note(note) => match &note.content {
            Payload::Inline(bytes) => assert_eq!(bytes.as_slice(), b"mode=plan"),
            _ => panic!("expected inline payload"),
        },
        _ => panic!("expected NoteOp for mode event"),
    }
}

/// Regression: a metadata record with no `sessionId`/`session_id` must scope to
/// the owning source file's session, NOT `derive_session_id("")` (a constant that
/// would cause every session's snapshots to share one synthetic scope and stitch
/// unrelated sessions together).
#[expect(
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "test asserts exact scope variant"
)]
#[test]
fn test_metadata_without_session_id_falls_back_to_owning_session() {
    // A file-history-snapshot record carries no sessionId field.
    let json = br#"{"type":"file-history-snapshot","messageId":"m1","snapshot":{"trackedFileBackups":{}},"isSnapshotUpdate":false,"timestamp":"2026-07-09T18:56:19.739Z"}"#;
    let env = parse_envelope(json).expect("parse snapshot");
    assert!(
        env.session_id.is_empty(),
        "snapshot should have no sessionId"
    );
    assert!(is_metadata_record(&env));

    let stream = SourceStream::new(derive_node_id("/test"), 0);
    let hash = hash_raw(json);
    let mut blobs = MemoryBlobSink::new();

    // Pass a fallback session id ("sess-A"). The raw op must scope to that
    // session, not to derive_session_id("") (a constant), and not to "sess-B".
    let (raw, _norm) = normalize_envelope(
        &env,
        hash,
        json,
        &stream,
        1,
        &NormalizeOptions::default(),
        &mut blobs,
        "sess-A",
    );

    let expected = editchain_import::ids::derive_session_id("sess-A").0;
    let wrong_constant = editchain_import::ids::derive_session_id("").0;
    match raw.scope {
        editchain_core::scope::ScopeRef::Session(sid) => {
            assert_eq!(
                sid.0, expected,
                "snapshot must scope to owning session, not the empty-session constant"
            );
            assert_ne!(
                sid.0, wrong_constant,
                "must not use the derive_session_id(\"\") constant (synthetic stitch scope)"
            );
        }
        _ => panic!("expected Session scope"),
    }
}
