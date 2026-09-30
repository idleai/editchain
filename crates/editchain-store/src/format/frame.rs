// Suppress unused_crate_dependencies warnings for crates consumed by other modules
// or by derive macros.
#[cfg(test)]
use proptest as _;
use serde as _;

use editchain_core::{Op, OpKind};

use super::page::PAGE_MAGIC;

// Ten continuation bytes cannot begin a valid EC02 u64 varint.
const RECORD_MAGIC: &[u8] = b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\xffECR3";
const RECORD_SCHEMA: u8 = 1;

/// Encode an operation into a binary frame using postcard.
///
/// # Errors
///
/// Returns `postcard::Error` if serialization fails.
pub fn encode_op(op: &Op) -> Result<Vec<u8>, postcard::Error> {
    if op.source.is_some_and(|source| source.id() != op.id) {
        return Err(postcard::Error::SerdeSerCustom);
    }
    let mut bytes = RECORD_MAGIC.to_vec();
    if let OpKind::Activity(record) = &op.kind {
        validate_activity(op, record)?;
        bytes.push(3);
        bytes.extend_from_slice(&postcard::to_stdvec(record)?);
        return Ok(bytes);
    }
    bytes.push(RECORD_SCHEMA);
    bytes.extend_from_slice(&postcard::to_stdvec(op)?);
    Ok(bytes)
}

/// Compute the exact encoded length without allocating the output buffer.
///
/// # Errors
///
/// Returns the same serialization errors as [`encode_op`].
pub fn encoded_op_len(op: &Op) -> Result<usize, postcard::Error> {
    if op.source.is_some_and(|source| source.id() != op.id) {
        return Err(postcard::Error::SerdeSerCustom);
    }
    let size = if let OpKind::Activity(record) = &op.kind {
        validate_activity(op, record)?;
        postcard::experimental::serialized_size(record)
    } else {
        postcard::experimental::serialized_size(op)
    };
    size.map(|size| size.saturating_add(RECORD_MAGIC.len()).saturating_add(1))
}

/// Decode an operation from a binary frame.
///
/// # Errors
///
/// Returns `postcard::Error` if deserialization fails or bytes remain after the
/// supported operation schema. A future extension must not masquerade as a
/// complete operation understood by this reader.
pub fn decode_op(bytes: &[u8]) -> Result<Op, postcard::Error> {
    let (op, remaining) = if let Some(bytes) = bytes.strip_prefix(RECORD_MAGIC) {
        match bytes.split_first() {
            Some((&RECORD_SCHEMA, body)) => {
                let (op, remaining): (Op, _) = postcard::take_from_bytes(body)?;
                if matches!(op.kind, OpKind::Activity(_)) {
                    return Err(postcard::Error::DeserializeBadEncoding);
                }
                (op, remaining)
            }
            Some((&3, body)) => {
                let (record, remaining) =
                    postcard::take_from_bytes::<editchain_core::activity::Operation>(body)?;
                (
                    record
                        .into_op()
                        .map_err(|_error| postcard::Error::DeserializeBadEncoding)?,
                    remaining,
                )
            }
            Some((&2, body)) => {
                let (legacy, remaining) =
                    postcard::take_from_bytes::<editchain_core::legacy::LegacyOp>(body)?;
                if matches!(legacy.kind, OpKind::Activity(_)) {
                    return Err(postcard::Error::DeserializeBadEncoding);
                }
                (legacy.into_canonical(), remaining)
            }
            _ => return Err(postcard::Error::DeserializeBadEncoding),
        }
    } else {
        let (legacy, remaining) =
            postcard::take_from_bytes::<editchain_core::legacy::LegacyOp>(bytes)?;
        if matches!(legacy.kind, OpKind::Activity(_)) {
            return Err(postcard::Error::DeserializeBadEncoding);
        }
        (legacy.into_canonical(), remaining)
    };
    if !remaining.is_empty() {
        return Err(postcard::Error::DeserializeBadEncoding);
    }
    if op.source.is_some_and(|source| source.id() != op.id) {
        return Err(postcard::Error::DeserializeBadEncoding);
    }
    Ok(op)
}

fn validate_activity(
    op: &Op,
    record: &editchain_core::activity::Operation,
) -> Result<(), postcard::Error> {
    record
        .validate()
        .map_err(|_error| postcard::Error::SerdeSerCustom)?;
    if !record.matches_envelope(op) {
        return Err(postcard::Error::SerdeSerCustom);
    }
    Ok(())
}

/// Rewrite a supported EC02 record while retaining noncanonical encoding variants.
/// Unknown records and already-versioned records retain their exact bytes.
/// # Errors
/// Returns canonical serialization errors.
pub fn migrate_record(bytes: &[u8]) -> Result<Vec<u8>, postcard::Error> {
    if bytes.starts_with(RECORD_MAGIC) {
        return Ok(bytes.to_vec());
    }
    let Ok((legacy, remaining)) =
        postcard::take_from_bytes::<editchain_core::legacy::LegacyOp>(bytes)
    else {
        return Ok(bytes.to_vec());
    };
    if !remaining.is_empty() {
        return Ok(bytes.to_vec());
    }
    if postcard::to_stdvec(&legacy)? == bytes {
        return encode_op(&legacy.into_canonical());
    }
    // Distinct EC02 byte variants must remain distinct and quarantined even
    // when they decode to the same fields (e.g. a nonminimal varint).
    let mut witness = RECORD_MAGIC.to_vec();
    witness.push(2);
    witness.extend_from_slice(bytes);
    Ok(witness)
}

// Legacy EC02 compatibility helpers
// ---------------------------------------------------------------------------

/// Detect the frame format from magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// Legacy EC02 page format.
    Ec02,
    /// Current EC03 framed format.
    Ec03,
}

/// Detect the format of a frame from its magic bytes.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "bytes.len() >= 4 checked before slicing"
)]
pub fn detect_format(bytes: &[u8]) -> Option<FrameFormat> {
    if bytes.len() < 4 {
        return None;
    }
    match &bytes[..4] {
        m if m == PAGE_MAGIC => Some(FrameFormat::Ec02),
        m if m == super::ec03::MAGIC => Some(FrameFormat::Ec03),
        _ => None,
    }
}
