use editchain_core::{ActorId, ChainId, NodeId, OpId, PathId, SessionId, TurnId};
use sha2::{Digest, Sha256};

/// Error for ID derivation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// Overflow when packing source position into a u64 sequence number.
    Overflow {
        /// The record ordinal that caused the overflow.
        record_ordinal: u64,
        /// The derived ordinal being packed.
        derived_ordinal: u16,
    },
}

impl core::fmt::Display for IdError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Overflow {
                record_ordinal,
                derived_ordinal,
            } => {
                write!(
                    f,
                    "source position overflow: record_ordinal={record_ordinal}, derived_ordinal={derived_ordinal}"
                )
            }
        }
    }
}

impl std::error::Error for IdError {}

/// A checked source position that packs into a collision-free u64 sequence number.
///
/// Layout: bits 63..16 = `record_ordinal`, bits 15..0 = `derived_ordinal`.
/// - `derived_ordinal = 0` → the raw source record itself.
/// - `derived_ordinal >= 1` → a normalized/derived operation from that record.
///
/// This provides 65,535 derived operations per source record and makes
/// overflow explicit rather than silently wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    /// Ordinal of the source record within its lane (1-based).
    pub record_ordinal: u64,
    /// Sub-ordinal within the record (0 = raw record).
    pub derived_ordinal: u16,
}

impl SourcePosition {
    /// Pack into a u64 sequence number.
    ///
    /// Returns `Err(IdError::Overflow)` if `record_ordinal` exceeds
    /// the 48-bit usable range (max ~281 trillion records).
    ///
    /// # Errors
    ///
    /// Returns `IdError::Overflow` if `record_ordinal` exceeds 2^48 - 1.
    pub fn to_seq(self) -> Result<u64, IdError> {
        if self.record_ordinal > (u64::MAX >> 16) {
            return Err(IdError::Overflow {
                record_ordinal: self.record_ordinal,
                derived_ordinal: self.derived_ordinal,
            });
        }
        Ok((self.record_ordinal << 16) | u64::from(self.derived_ordinal))
    }

    /// Create a `SourcePosition` for a raw source record.
    #[must_use]
    pub const fn raw(record_ordinal: u64) -> Self {
        Self {
            record_ordinal,
            derived_ordinal: 0,
        }
    }

    /// Create a `SourcePosition` for the n-th derived operation (1-based).
    #[must_use]
    pub const fn derived(record_ordinal: u64, n: u16) -> Self {
        Self {
            record_ordinal,
            derived_ordinal: n,
        }
    }
}

/// Extract the first 8 bytes of a SHA-256 digest as a `u64` (little-endian).
///
/// SHA-256 always produces 32 bytes, so the `[..8]` slice is infallible.
#[expect(
    clippy::expect_used,
    reason = "SHA-256 output is always 32 bytes, so [..8] is infallible"
)]
fn hash_prefix_u64(hash: [u8; 32]) -> u64 {
    let bytes: [u8; 8] = hash[..8].try_into().expect("SHA-256 output is 32 bytes");
    u64::from_le_bytes(bytes)
}

/// Deterministic ID derivation from stable source data.
///
/// All IDs are derived from versioned hashes of stable source data.
/// NEVER use current time, random values, file mtime, or directory enumeration order.
///
/// # Panics
///
/// This function cannot panic — SHA-256 always produces 32 bytes.
///
/// Derive a `NodeId` from a workspace path.
#[must_use]
pub fn derive_node_id(workspace_path: &str) -> NodeId {
    let hash: [u8; 32] =
        Sha256::digest(format!("editchain:node:{workspace_path}").as_bytes()).into();
    NodeId(hash_prefix_u64(hash))
}

/// Derive an `ActorId` from an actor identifier string.
///
/// # Panics
///
/// This function cannot panic — SHA-256 always produces 32 bytes.
#[must_use]
pub fn derive_actor_id(actor_key: &str) -> ActorId {
    let hash: [u8; 32] = Sha256::digest(format!("editchain:actor:{actor_key}").as_bytes()).into();
    ActorId(hash_prefix_u64(hash))
}

/// Derive a `ChainId` from a chain name.
///
/// # Panics
///
/// This function cannot panic — SHA-256 always produces 32 bytes.
#[must_use]
pub fn derive_chain_id(chain_name: &str) -> ChainId {
    let hash: [u8; 32] = Sha256::digest(format!("editchain:chain:{chain_name}").as_bytes()).into();
    ChainId(hash_prefix_u64(hash))
}

/// Derive a `SessionId` from a session UUID string.
///
/// # Panics
///
/// This function cannot panic — SHA-256 always produces 32 bytes.
#[must_use]
pub fn derive_session_id(session_uuid: &str) -> SessionId {
    let hash: [u8; 32] =
        Sha256::digest(format!("editchain:session:{session_uuid}").as_bytes()).into();
    SessionId(hash_prefix_u64(hash))
}

/// Derive a `TurnId` from a turn identifier string.
///
/// # Panics
///
/// This function cannot panic — SHA-256 always produces 32 bytes.
#[must_use]
pub fn derive_turn_id(turn_key: &str) -> TurnId {
    let hash: [u8; 32] = Sha256::digest(format!("editchain:turn:{turn_key}").as_bytes()).into();
    TurnId(hash_prefix_u64(hash))
}

/// Derive a `PathId` from a normalized file path.
///
/// # Panics
///
/// This function cannot panic — SHA-256 always produces 32 bytes.
#[must_use]
pub fn derive_path_id(path: &str) -> PathId {
    let hash: [u8; 32] = Sha256::digest(format!("editchain:path:{path}").as_bytes()).into();
    PathId(hash_prefix_u64(hash))
}

/// Hash raw bytes with Blake3 for content addressing.
#[must_use]
pub fn hash_raw(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}

/// Derive a deterministic `SourceStream` from a workspace identity and a
/// session file path. Each physical source stream gets its own lane so that
/// `OpIds` are independent of file discovery order.
///
/// The `boot` parameter allows generation tracking: increment it when a source
/// file is rewritten (truncated or replaced) to start a new ID space while
/// retaining the old generation's data.
///
/// # Panics
///
/// This function cannot panic — SHA-256 always produces 32 bytes.
#[must_use]
pub fn derive_source_stream(
    workspace_path: &str,
    session_file_path: &str,
    boot: u32,
) -> SourceStream {
    let hash: [u8; 32] = Sha256::digest(
        format!("editchain:source-stream:v1:{workspace_path}:{session_file_path}").as_bytes(),
    )
    .into();
    SourceStream {
        node: NodeId(hash_prefix_u64(hash)),
        boot,
    }
}

/// A source stream identifier — used to generate monotonic `OpId` sequences.
///
/// Each physical source stream (session file or subagent log) gets its own
/// deterministic lane so that IDs are independent of file discovery order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceStream {
    /// The node ID for this source stream.
    pub node: NodeId,
    /// Boot generation (incremented when source file is rewritten).
    pub boot: u32,
}

impl SourceStream {
    /// Create a new source stream with the given node and boot generation.
    #[must_use]
    pub const fn new(node: NodeId, boot: u32) -> Self {
        Self { node, boot }
    }

    /// Create an `OpId` with the given sequence number.
    #[must_use]
    pub const fn op_id(&self, seq: u64) -> OpId {
        OpId::new(self.node, self.boot, seq)
    }

    /// Create an `OpId` from a `SourcePosition` (checked packing).
    ///
    /// # Errors
    ///
    /// Returns `IdError::Overflow` if the source position's record ordinal
    /// exceeds the 48-bit usable range.
    pub fn op_from_position(&self, pos: SourcePosition) -> Result<OpId, IdError> {
        Ok(OpId::new(self.node, self.boot, pos.to_seq()?))
    }
}
