use editchain_core::{
    op::OpKind,
    payload::Payload,
    tags::Tags,
};
use editchain_import::claude_code::envelope::parse_envelope;
use editchain_import::claude_code::normalize::{normalize_envelope, NormalizeOptions};
use editchain_import::ids::{derive_node_id, hash_raw, SourceStream};
use editchain_import::sink::MemoryBlobSink;

#[test]
fn test_timestamp_parsing() {
    let ts = editchain_import::claude_code::normalize::parse_timestamp("2026-07-09T18:56:19.739Z");
    assert!(ts > 1700000000000);
    assert!(ts < 1800000000000);
}

#[test]
fn test_timestamp_no_millis() {
    let ts = editchain_import::claude_code::normalize::parse_timestamp("2026-07-09T18:56:19Z");
    assert!(ts > 1700000000000);
}

#[test]
fn test_empty_timestamp() {
    assert_eq!(editchain_import::claude_code::normalize::parse_timestamp(""), 0);
}

#[test]
fn test_normalize_user_message() {
    let json = br#"{"type":"user","uuid":"abc","sessionId":"sess-1","timestamp":"2026-07-09T18:56:19.739Z","message":{"role":"user","content":"hello world"}}"#;
    let env = parse_envelope(json).unwrap();
    let stream = SourceStream::new(derive_node_id("/test"), 0);
    let hash = hash_raw(json);
    let mut blobs = MemoryBlobSink::new();

    let (raw, norm) =
        normalize_envelope(&env, hash, json, &stream, 1, &NormalizeOptions::default(), &mut blobs);

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