use editchain_core::{ActorId, ChainId, NodeId, OpId, PathId, SessionId, TurnId};
use sha2::{Digest, Sha256};

/// Error for ID derivation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// Overflow when packing source position into a u64 sequence number.
    Overflow {
        record_ordinal: u64,
        derived_ordinal: u16,
    },
}

impl core::fmt::Display for IdError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IdError::Overflow { record_ordinal, derived_ordinal } => {
                write!(
                    f,
                    "source position overflow: record_ordinal={}, derived_ordinal={}",
                    record_ordinal, derived_ordinal
                )
            }
        }
    }
}

impl std::error::Error for IdError {}

/// A checked source position that packs into a collision-free u64 sequence number.
///
/// Layout: bits 63..16 = record_ordinal, bits 15..0 = derived_ordinal.
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
    pub fn to_seq(self) -> Result<u64, IdError> {
        if self.record_ordinal > (u64::MAX >> 16) {
            return Err(IdError::Overflow {
                record_ordinal: self.record_ordinal,
                derived_ordinal: self.derived_ordinal,
            });
        }
        Ok((self.record_ordinal << 16) | u64::from(self.derived_ordinal))
    }

    /// Create a SourcePosition for a raw source record.
    pub const fn raw(record_ordinal: u64) -> Self {
        Self {
            record_ordinal,
            derived_ordinal: 0,
        }
    }

    /// Create a SourcePosition for the n-th derived operation (1-based).
    pub const fn derived(record_ordinal: u64, n: u16) -> Self {
        Self {
            record_ordinal,
            derived_ordinal: n,
        }
    }
}

/// Deterministic ID derivation from stable source data.
///
/// All IDs are derived from versioned hashes of stable source data.
/// NEVER use current time, random values, file mtime, or directory enumeration order.
///
/// Derive a NodeId from a workspace path.
pub fn derive_node_id(workspace_path: &str) -> NodeId {
    let hash = Sha256::digest(format!("editchain:node:{}", workspace_path).as_bytes());
    let val = u64::from_le_bytes(hash[..8].try_into().unwrap());
    NodeId(val)
}

/// Derive an ActorId from an actor identifier string.
pub fn derive_actor_id(actor_key: &str) -> ActorId {
    let hash = Sha256::digest(format!("editchain:actor:{}", actor_key).as_bytes());
    let val = u64::from_le_bytes(hash[..8].try_into().unwrap());
    ActorId(val)
}

/// Derive a ChainId from a chain name.
pub fn derive_chain_id(chain_name: &str) -> ChainId {
    let hash = Sha256::digest(format!("editchain:chain:{}", chain_name).as_bytes());
    let val = u64::from_le_bytes(hash[..8].try_into().unwrap());
    ChainId(val)
}

/// Derive a SessionId from a session UUID string.
pub fn derive_session_id(session_uuid: &str) -> SessionId {
    let hash = Sha256::digest(format!("editchain:session:{}", session_uuid).as_bytes());
    let val = u64::from_le_bytes(hash[..8].try_into().unwrap());
    SessionId(val)
}

/// Derive a TurnId from a turn identifier string.
pub fn derive_turn_id(turn_key: &str) -> TurnId {
    let hash = Sha256::digest(format!("editchain:turn:{}", turn_key).as_bytes());
    let val = u64::from_le_bytes(hash[..8].try_into().unwrap());
    TurnId(val)
}

/// Derive a PathId from a normalized file path.
pub fn derive_path_id(path: &str) -> PathId {
    let hash = Sha256::digest(format!("editchain:path:{}", path).as_bytes());
    let val = u64::from_le_bytes(hash[..8].try_into().unwrap());
    PathId(val)
}

/// Hash raw bytes with Blake3 for content addressing.
pub fn hash_raw(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}

/// Derive a deterministic `SourceStream` from a workspace identity and a
/// session file path. Each physical source stream gets its own lane so that
/// OpIds are independent of file discovery order.
///
/// The `boot` parameter allows generation tracking: increment it when a source
/// file is rewritten (truncated or replaced) to start a new ID space while
/// retaining the old generation's data.
pub fn derive_source_stream(workspace_path: &str, session_file_path: &str, boot: u32) -> SourceStream {
    let hash = Sha256::digest(
        format!("editchain:source-stream:v1:{}:{}", workspace_path, session_file_path).as_bytes(),
    );
    let val = u64::from_le_bytes(hash[..8].try_into().unwrap());
    SourceStream {
        node: NodeId(val),
        boot,
    }
}

/// A source stream identifier — used to generate monotonic OpId sequences.
///
/// Each physical source stream (session file or subagent log) gets its own
/// deterministic lane so that IDs are independent of file discovery order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceStream {
    pub node: NodeId,
    pub boot: u32,
}

impl SourceStream {
    pub fn new(node: NodeId, boot: u32) -> Self {
        Self { node, boot }
    }

    /// Create an OpId with the given sequence number.
    pub fn op_id(&self, seq: u64) -> OpId {
        OpId::new(self.node, self.boot, seq)
    }

    /// Create an OpId from a SourcePosition (checked packing).
    pub fn op_from_position(&self, pos: SourcePosition) -> Result<OpId, IdError> {
        Ok(OpId::new(self.node, self.boot, pos.to_seq()?))
    }
}

