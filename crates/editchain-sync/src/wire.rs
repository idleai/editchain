//! Versioned messages, independent of local RPC and physical storage pages.

use serde::{Deserialize, Serialize};
use std::io;

use crate::{invalid, RecordKey, MAX_FRAME_BYTES};

/// Peer protocol messages carried only inside an authenticated channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    /// Negotiate the space, peer protocol and operation encoding.
    Hello {
        /// Peer protocol version; see [`crate::PEER_VERSION`].
        version: u16,
        /// Exact operation-encoding version, currently one.
        encoding: u16,
        /// Locally approved collaboration space.
        space: String,
    },
    /// Begin a stable inventory or continue after an exclusive cursor.
    Inventory {
        /// Last record in the preceding page; absent starts a fresh snapshot.
        after: Option<RecordKey>,
    },
    /// One bounded page of exact record identities.
    Page {
        /// Position of the first identity in the stable parent-ordered snapshot.
        offset: u64,
        /// Parent-first identities, including quarantined variants.
        records: Vec<RecordKey>,
        /// Whether the stable snapshot has another page.
        more: bool,
    },
    /// Request part of a record or its authorized referenced content.
    Need {
        /// Record authorizing the request.
        record: RecordKey,
        /// Absent for operation bytes; present for a content-addressed blob.
        blob: Option<[u8; 32]>,
        /// Offset into the requested object.
        offset: u32,
    },
    /// A bounded chunk whose full digest is verified before durable publication.
    Chunk {
        /// Record identity associated with this transfer.
        record: RecordKey,
        /// Content identity, absent for an operation record.
        blob: Option<[u8; 32]>,
        /// Absolute chunk offset.
        offset: u32,
        /// Total object length.
        total: u32,
        /// Object bytes.
        bytes: Vec<u8>,
    },
    /// Content is not currently available or permitted for this record.
    Missing {
        /// Requested record.
        record: RecordKey,
        /// Requested blob, if any.
        blob: Option<[u8; 32]>,
    },
    /// The receiver has completed durable publication of this exact item.
    Ack {
        /// Persisted record.
        record: RecordKey,
        /// Persisted blob, if any.
        blob: Option<[u8; 32]>,
    },
}

/// Encode one bounded, length-prefixed peer frame.
///
/// # Errors
/// Rejects serialization failure or a payload exceeding the frame limit.
pub fn encode_message(message: &Message) -> io::Result<Vec<u8>> {
    let payload = postcard::to_stdvec(message).map_err(io::Error::other)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(invalid("peer frame exceeds limit"));
    }
    let mut frame = u32::try_from(payload.len())
        .map_err(io::Error::other)?
        .to_le_bytes()
        .to_vec();
    frame.extend(payload);
    Ok(frame)
}

/// Decode an exact payload, rejecting trailing bytes and unsupported variants.
///
/// # Errors
/// Returns a framing or codec error for malformed messages.
pub fn decode_message(payload: &[u8]) -> io::Result<Message> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(invalid("peer frame exceeds limit"));
    }
    let (message, remaining) = postcard::take_from_bytes(payload).map_err(io::Error::other)?;
    if !remaining.is_empty() {
        return Err(invalid("peer message has trailing bytes"));
    }
    Ok(message)
}

/// Incremental decoder retaining at most one bounded frame.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    bytes: Vec<u8>,
}

impl FrameDecoder {
    /// Consume one transport chunk; fragmented headers and payloads are supported.
    ///
    /// # Errors
    /// Rejects oversized chunks/frames, malformed messages and arithmetic overflow.
    pub fn push(&mut self, bytes: &[u8]) -> io::Result<Vec<Message>> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(invalid("transport chunk exceeds limit"));
        }
        self.bytes.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while let Some(header) = self.bytes.get(..4) {
            let header: [u8; 4] = header.try_into().map_err(io::Error::other)?;
            let length = usize::try_from(u32::from_le_bytes(header)).map_err(io::Error::other)?;
            if length == 0 || length > MAX_FRAME_BYTES {
                return Err(invalid("invalid peer frame length"));
            }
            let end = length
                .checked_add(4)
                .ok_or_else(|| invalid("peer frame length overflow"))?;
            let Some(payload) = self.bytes.get(4..end) else {
                break;
            };
            messages.push(decode_message(payload)?);
            drop(self.bytes.drain(..end));
        }
        Ok(messages)
    }

    /// Verify that EOF occurred between complete frames.
    ///
    /// # Errors
    /// Rejects incomplete trailing headers or message bodies.
    pub fn finish(&self) -> io::Result<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(invalid("truncated peer frame"))
        }
    }
}
