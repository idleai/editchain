use super::encoding::Encoding;
use editchain_protocol::editor::EditorEvent;
use serde_json::{json, Value};

fn observation(payload: &Value) -> EditorEvent {
    serde_json::from_value(
        json!({"schema":1,"session":"recorder", "sequence":2,"time_ms":1234,
        "identity":{"kind":"unsigned","guid":"unsigned-human", "stream":"workspace"}, "event":payload}),
    )
    .expect("source fixture")
}

fn canonical(encoding: &mut Encoding, event: &EditorEvent) {
    assert_eq!(
        encoding.encode(event).expect("source encoding"),
        serde_json::to_vec(&json!({"source":"vscode.editor", "event":event}))
            .expect("legacy source encoding"),
        "source hashes must remain byte-identical to the established encoding"
    );
}

#[test]
fn cached_sources_preserve_canonical_bytes_for_unicode_replacements_and_metadata() {
    let before = "a😀\"\\\n\r\t\u{1}🦀éz";
    let units: Vec<_> = before.encode_utf16().collect();
    let document = json!({"id":"buffer", "path":"a.rs", "uri":"file:///a.rs", "version":2});
    let baseline =
        observation(&json!({"type":"document_snapshot", "document":document, "text":before}));
    let mut encoding = Encoding::default();
    for start in 0..=units.len() {
        for end in start..=units.len() {
            for text in ["", "X", "🙂", "\u{1}\"\\\r\n"] {
                let mut changed = units.clone();
                drop(changed.splice(start..end, text.encode_utf16()));
                let Ok(after) = String::from_utf16(&changed) else {
                    continue;
                };
                canonical(&mut encoding, &baseline);
                let mut event = observation(
                    &json!({"type":"document_changed", "document":document,
                    "before_version":1,"before":before,"after":after,"reason":"undo",
                    "changes":[{"offset":start,"length":end.saturating_sub(start),"text":text}],
                    "origin":{"source":"cursor","kind":"type","detailed_source":"keyboard","name":"input","extension_id":"test"}}),
                );
                canonical(&mut encoding, &event);
                event.identity = None;
                canonical(&mut encoding, &event);
            }
        }
    }
    let fallback = observation(&json!({"type":"document_changed", "document":document,
        "before_version":1,"before":before,"after":"different","reason":null,"changes":[]}));
    canonical(&mut encoding, &fallback);
}
