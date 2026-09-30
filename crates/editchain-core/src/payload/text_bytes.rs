//! Lossless content bytes: UTF-8 text in JSON, unchanged bytes in binary formats.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Bytes(Vec<u8>),
}

/// Serialize valid UTF-8 as text only in human-readable formats.
///
/// # Errors
/// Returns the serializer's error if writing fails.
pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    if serializer.is_human_readable() {
        if let Ok(text) = std::str::from_utf8(bytes) {
            return serializer.serialize_str(text);
        }
    }
    bytes.serialize(serializer)
}

/// Read text or legacy byte arrays without changing the binary representation.
///
/// # Errors
/// Returns the deserializer's error for values that are neither text nor bytes.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    if !deserializer.is_human_readable() {
        return Vec::<u8>::deserialize(deserializer);
    }
    Content::deserialize(deserializer).map(|content| match content {
        Content::Text(text) => text.into_bytes(),
        Content::Bytes(bytes) => bytes,
    })
}
