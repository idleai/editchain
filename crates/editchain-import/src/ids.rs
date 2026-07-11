use editchain_core::{ActorId, ChainId, NodeId, OpId, PathId, SessionId, TurnId};
use sha2::{Digest, Sha256};

/// Deterministic ID derivation from stable source data.
///
/// All IDs are derived from versioned hashes of stable source data.
/// NEVER use current time, random values, file mtime, or directory enumeration order.

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

/// A source stream identifier — used to generate monotonic OpId sequences.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_node_id() {
        let a = derive_node_id("/workspace/editchain");
        let b = derive_node_id("/workspace/editchain");
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_different_ids() {
        let a = derive_node_id("/workspace/a");
        let b = derive_node_id("/workspace/b");
        assert_ne!(a, b);
    }

    #[test]
    fn deterministic_session_id() {
        let uuid = "3f7db8b8-73a7-4cea-be8d-3d2d54fedd2c";
        let a = derive_session_id(uuid);
        let b = derive_session_id(uuid);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_raw_is_blake3() {
        let data = b"hello world";
        let h = hash_raw(data);
        let expected = blake3::hash(data);
        assert_eq!(h, *expected.as_bytes());
    }
}