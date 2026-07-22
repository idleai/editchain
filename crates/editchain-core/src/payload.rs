#[cfg(not(feature = "use-std"))]
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// Content-addressed or locally-addressed blob identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentId {
    /// Local content reference — cheap for embedded devices.
    Local {
        /// Node identifier.
        node: NodeId,
        /// Sequence number on the node.
        seq: u64,
    },
    /// 128-bit hash (e.g. Blake3 truncated).
    Hash128([u8; 16]),
    /// 256-bit hash (e.g. full Blake3).
    Hash256([u8; 32]),
}

/// Reference to a blob payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    /// Content identifier.
    pub id: ContentId,
    /// Length of the blob in bytes.
    pub len: u32,
}

/// A payload — inline bytes or a blob reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Payload {
    /// Empty payload (no content).
    #[default]
    Empty,
    /// Small inline byte slice.
    Inline(Vec<u8>),
    /// Reference to an external blob.
    Blob(BlobRef),
}

impl Payload {
    /// Returns true if this payload is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Returns the byte length of the payload content, if known inline.
    #[must_use]
    #[expect(
        clippy::as_conversions,
        reason = "BlobRef.len is u32 stored in wire format; widening to usize for API convenience"
    )]
    pub const fn len(&self) -> Option<usize> {
        match self {
            Self::Empty => Some(0),
            Self::Inline(b) => Some(b.len()),
            Self::Blob(r) => Some(r.len as usize),
        }
    }
}
