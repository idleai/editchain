use core::cmp::Ordering;
use serde::{Deserialize, Serialize};

/// A node identifier — 64 bits wide, cheap for embedded devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u64);

/// An actor identifier — 64 bits wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActorId(pub u64);

/// A chain identifier — 64 bits wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChainId(pub u64);

/// A session identifier — 64 bits wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

/// A turn identifier — 64 bits wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(pub u64);

/// A path identifier — 64 bits wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct PathId(pub u64);

/// Globally unique operation identifier.
///
/// Cheap embedded identity: node + boot counter + monotonic sequence.
/// Gateways may add proof hashes alongside these IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OpId {
    pub node: NodeId,
    pub boot: u32,
    pub seq: u64,
}

impl OpId {
    pub const fn new(node: NodeId, boot: u32, seq: u64) -> Self {
        Self { node, boot, seq }
    }
}

impl Ord for OpId {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary key: node → boot → seq
        self.node
            .0
            .cmp(&other.node.0)
            .then(self.boot.cmp(&other.boot))
            .then(self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for OpId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl core::fmt::Display for OpId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}:{}", self.node.0, self.boot, self.seq)
    }
}