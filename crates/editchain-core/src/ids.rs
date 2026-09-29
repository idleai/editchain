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

/// Stable producer provenance, independent of canonical operation identity.
///
/// This is also the frozen EC02 operation address used during migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId {
    /// Node identifier.
    pub node: NodeId,
    /// Boot counter (incremented on restart).
    pub boot: u32,
    /// Monotonic sequence number within this boot epoch.
    pub seq: u64,
}

impl SourceId {
    /// Create a source address from its components.
    #[must_use]
    pub const fn new(node: NodeId, boot: u32, seq: u64) -> Self {
        Self { node, boot, seq }
    }

    /// Parse a source address from its display form `"node:boot:seq"`.
    ///
    /// Returns `None` if the string is not in the expected format. This is used
    /// to round-trip source addresses through JSON as strings, avoiding JavaScript's
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

impl Ord for SourceId {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary key: node → boot → seq
        self.node
            .0
            .cmp(&other.node.0)
            .then(self.boot.cmp(&other.boot))
            .then(self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for SourceId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl core::fmt::Display for SourceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}:{}", self.node.0, self.boot, self.seq)
    }
}

/// Canonical operation identity: a fixed 256-bit value, unrelated to ordering.
///
/// Machine interfaces use all 64 hexadecimal digits. Display abbreviations are
/// resolved against the retained chain, never persisted as identities.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId([u8; 32]);

impl SourceId {
    /// Deterministically map an EC02 address or producer position to its ID.
    #[must_use]
    pub fn id(self) -> OpId {
        let mut hash = blake3::Hasher::new_derive_key("editchain.operation-id.v1");
        let _: &mut blake3::Hasher = hash.update(&self.node.0.to_le_bytes());
        let _: &mut blake3::Hasher = hash.update(&self.boot.to_le_bytes());
        let _: &mut blake3::Hasher = hash.update(&self.seq.to_le_bytes());
        OpId(*hash.finalize().as_bytes())
    }
}

impl OpId {
    /// Derive an ID from a stable producer position (including legacy addresses).
    #[must_use]
    pub fn new(node: NodeId, boot: u32, seq: u64) -> Self {
        SourceId::new(node, boot, seq).id()
    }

    /// Construct a canonical identity from all its bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Full fixed-width binary identity.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse a full hexadecimal ID or deterministically translate an EC02 address.
    #[must_use]
    pub fn from_display_str(value: &str) -> Option<Self> {
        if value.contains(':') {
            return SourceId::from_display_str(value).map(SourceId::id);
        }
        if value.len() != 64 {
            return None;
        }
        let mut bytes = [0_u8; 32];
        for (out, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            let text = std::str::from_utf8(pair).ok()?;
            *out = u8::from_str_radix(text, 16).ok()?;
        }
        Some(Self(bytes))
    }
}

impl core::fmt::Display for OpId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl core::fmt::Debug for OpId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}

impl Serialize for OpId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.collect_str(self)
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for OpId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ReadId {
            Canonical(String),
            Legacy(SourceId),
        }
        if !deserializer.is_human_readable() {
            return <[u8; 32]>::deserialize(deserializer).map(Self);
        }
        match ReadId::deserialize(deserializer)? {
            ReadId::Canonical(text) => Self::from_display_str(&text)
                .ok_or_else(|| serde::de::Error::custom("expected a full 64-digit operation ID")),
            ReadId::Legacy(source) => Ok(source.id()),
        }
    }
}
