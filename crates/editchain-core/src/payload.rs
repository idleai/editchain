use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// Content-addressed or locally-addressed blob identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentId {
    /// Local content reference — cheap for embedded devices.
    Local { node: NodeId, seq: u64 },
    /// 128-bit hash (e.g. Blake3 truncated).
    Hash128([u8; 16]),
    /// 256-bit hash (e.g. full Blake3).
    Hash256([u8; 32]),
}

/// Reference to a blob payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub id: ContentId,
    pub len: u32,
}

/// A payload — inline bytes or a blob reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum Payload {
    #[default]
    Empty,
    /// Small inline byte slice.
    Inline(Vec<u8>),
    /// Reference to an external blob.
    Blob(BlobRef),
}


impl Payload {
    /// Returns true if this payload is empty.
    pub fn is_empty(&self) -> bool {
        matches!(self, Payload::Empty)
    }

    /// Returns the byte length of the payload content, if known inline.
    pub fn len(&self) -> Option<usize> {
        match self {
            Payload::Empty => Some(0),
            Payload::Inline(b) => Some(b.len()),
            Payload::Blob(r) => Some(r.len as usize),
        }
    }
}