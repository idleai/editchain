//! Normalization tests for Claude Code envelopes.

use blake3 as _;
use editchain_codec as _;
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

use editchain_import::source_time::parse_source_time;

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
fn source_time_normalizes_offsets_and_preserves_fractional_milliseconds() {
    for (text, expected) in [
        ("1970-01-01T00:00:00Z", 0),
        ("1970-01-01T01:00:00+01:00", 0),
        ("1969-12-31T23:00:00-01:00", 0),
        ("1970-01-01T00:00:01.9Z", 1_900),
        ("1970-01-01T00:00:01.98Z", 1_980),
        ("1970-01-01T00:00:01.987654321Z", 1_987),
        ("1970-01-01t00:00:01.987z", 1_987),
        ("1970-01-01T05:30:01.987654321+05:30", 1_987),
        ("1969-12-31T18:30:01.987654321-05:30", 1_987),
        ("2016-12-31T23:59:60Z", 1_483_228_799_999),
    ] {
        assert_eq!(parse_source_time(text), Some(expected), "{text}");
        assert_eq!(
            editchain_import::codex::normalize::raw_clock(Some(text)),
            (editchain_core::Clock::UnixMs(expected), false)
        );
    }
    assert_eq!(
        parse_source_time("2000-02-29T12:34:56Z"),
        parse_source_time("2000-02-29T07:34:56-05:00")
    );
    assert!(parse_source_time("2000-02-29T12:34:56Z").is_some());
}

#[test]
fn malformed_and_pre_epoch_source_times_stay_unknown_without_panicking() {
    for text in [
        "",
        "not a time",
        "2026-07-09",
        "2026/07/09T18:56:19Z",
        "2026-07-09T18:56:19",
        "2026-07-09T18:56:19Z trailing",
        "2026-07-09T18:56:19.badZ",
        "2026-07-09T18:56:19.Z",
        "2026-07-09T18:56:19+25:00",
        "2026-07-09T18:56:19+01:60",
        "2026-00-09T18:56:19Z",
        "2026-13-09T18:56:19Z",
        "2026-07-00T18:56:19Z",
        "2026-04-31T18:56:19Z",
        "2026-02-29T18:56:19Z",
        "2100-02-29T18:56:19Z",
        "2026-07-09T24:00:00Z",
        "2026-07-09T18:60:00Z",
        "2026-07-09T18:56:61Z",
        "2026-07-09T18:56:19+0100",
        "202/-07-09T18:56:19Z",
        "2026-0/-09T18:56:19Z",
        "２０２６-07-09T18:56:19Z",
        "1969-12-31T23:59:59.999999999Z",
        "1970-01-01T00:00:00+00:01",
    ] {
        assert_eq!(parse_source_time(text), None, "{text}");
        assert_eq!(
            editchain_import::codex::normalize::raw_clock(Some(text)),
            (editchain_core::Clock::UnixMs(0), true)
        );
    }
}

#[test]
fn claude_capture_preserves_raw_bytes_when_source_time_is_invalid() {
    for (timestamp, expected_ms, unknown) in [
        ("2026-02-30T12:00:00Z", 0, true),
        ("202/-07-09T18:56:19Z", 0, true),
        ("1970-01-01T05:30:01.987654321+05:30", 1_987, false),
    ] {
        let raw_bytes = serde_json::to_vec(&serde_json::json!({
            "type": "user", "uuid": "event", "sessionId": "session",
            "timestamp": timestamp, "message": { "role": "user", "content": "hello" },
        }))
        .unwrap();
        let envelope = parse_envelope(&raw_bytes).unwrap();
        let stream = SourceStream::new(derive_node_id("/test"), 0);
        let (raw, normalized) = normalize_envelope(
            &envelope,
            hash_raw(&raw_bytes),
            &raw_bytes,
            &stream,
            1,
            &NormalizeOptions::default(),
            &mut MemoryBlobSink::new(),
            "session",
        )
        .unwrap();
        assert_eq!(raw.clock, editchain_core::Clock::UnixMs(expected_ms));
        assert_eq!(raw.tags.matches_any(Tags::SOURCE_TIME_UNKNOWN), unknown);
        assert!(
            matches!(&raw.kind, OpKind::Import(import) if import.raw_ref == Payload::Inline(raw_bytes))
        );
        assert!(!normalized.is_empty());
        assert!(normalized.iter().all(|op| op.clock == raw.clock));
    }
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
    )
    .expect("normalize user record");

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
    )
    .expect("normalize mode record");

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
    )
    .expect("normalize metadata record");

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

#[test]
fn test_transport_sidecars_are_exact_bundle_metadata() {
    for json in [
        br#"{"type":"atis-latch","atis":"","sessionId":"sess-1"}"#.as_slice(),
        br#"{"type":"fork-context-ref","parentSessionId":"sess-0","contextLength":8}"#.as_slice(),
        br#"{"type":"file-history-delta","messageId":"m1","trackingPath":"src/lib.rs"}"#.as_slice(),
    ] {
        let env = parse_envelope(json).expect("parse transport sidecar");
        assert!(is_metadata_record(&env), "record type: {}", env.record_type);

        let stream = SourceStream::new(derive_node_id("/transport-sidecar"), 0);
        let mut blobs = MemoryBlobSink::new();
        let (raw, normalized) = normalize_envelope(
            &env,
            hash_raw(json),
            json,
            &stream,
            1,
            &NormalizeOptions::default(),
            &mut blobs,
            "sess-1",
        )
        .expect("normalize transport sidecar");
        assert!(raw.tags.matches_any(Tags::META));
        assert!(normalized.is_empty());
    }
}
