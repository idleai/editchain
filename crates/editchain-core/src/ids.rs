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
    /// Node identifier.
    pub node: NodeId,
    /// Boot counter (incremented on restart).
    pub boot: u32,
    /// Monotonic sequence number within this boot epoch.
    pub seq: u64,
}

impl OpId {
    /// Create a new `OpId` from its components.
    #[must_use]
    pub const fn new(node: NodeId, boot: u32, seq: u64) -> Self {
        Self { node, boot, seq }
    }

    /// Parse an `OpId` from its display form `"node:boot:seq"`.
    ///
    /// Returns `None` if the string is not in the expected format. This is used
    /// to round-trip `OpId`s through JSON as strings, avoiding JavaScript's
    /// precision loss on u64 values that exceed 2^53.
    #[must_use]
    pub fn from_display_str(s: &str) -> Option<Self> {
        let mut parts = s.split(':');
        let node = parts.next()?.parse().ok()?;
        let boot = parts.next()?.parse().ok()?;
        let seq = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            node: NodeId(node),
            boot,
            seq,
        })
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
