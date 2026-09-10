use core::cmp::Ordering;
use serde::{Deserialize, Serialize};

use crate::ids::{OpId, PathId};
use crate::payload::Payload;

// ---------------------------------------------------------------------------
// Git identity
// ---------------------------------------------------------------------------

/// A repository identifier — 64 bits wide.
///
/// The live Git adapter and importers preserve the legacy SHA-256 path-derived
/// ID of the repository's `.git` marker. Relocating a repository requires an
/// explicit mapping when existing durable links must retain their identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepositoryId(pub u64);

/// A Git commit identity qualified by its repository.
///
/// Clones and linked worktrees may contain the same OID while remaining
/// distinct history sources. Graph, search, and display keys retain both parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitCommitKey {
    /// Repository supplying this commit's history and live observations.
    pub repository: RepositoryId,
    /// Full commit object identity.
    pub oid: GitOid,
}

impl GitCommitKey {
    /// Create a repository-qualified identity.
    #[must_use]
    pub const fn new(repository: RepositoryId, oid: GitOid) -> Self {
        Self { repository, oid }
    }

    /// Parse the display key. Bare OIDs are ambiguous and are rejected.
    #[must_use]
    pub fn from_display_str(value: &str) -> Option<Self> {
        let (repository, oid) = value.strip_prefix("git:")?.split_once(':')?;
        Some(Self::new(
            RepositoryId(repository.parse().ok()?),
            GitOid::from_hex(oid)?,
        ))
    }
}

impl core::fmt::Display for GitCommitKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "git:{}:{}", self.repository.0, self.oid)
    }
}

/// The object format of a Git repository (SHA-1 or SHA-256).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub enum GitObjectFormat {
    /// SHA-1 object format (legacy default).
    Sha1,
    /// SHA-256 object format.
    Sha256,
}

/// A full Git object identifier.
///
/// The `bytes` field always holds 32 bytes: SHA-256 uses all 32; SHA-1 uses
/// the first 20 bytes and leaves the remainder zero. This keeps the type a
/// fixed size regardless of object format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct GitOid {
    /// Object format this OID was produced under.
    format: GitObjectFormat,
    /// Full OID bytes (32 bytes; SHA-1 occupies the first 20).
    bytes: [u8; 32],
}

impl GitOid {
    /// Validate a padded 32-byte digest. SHA-1 requires a zero unused tail;
    /// otherwise the same displayed hash could compare as different keys.
    #[must_use]
    pub fn new(format: GitObjectFormat, bytes: [u8; 32]) -> Option<Self> {
        if format == GitObjectFormat::Sha1 && bytes.iter().skip(20).any(|byte| *byte != 0) {
            None
        } else {
            Some(Self { format, bytes })
        }
    }

    /// Object format this identifier was produced under.
    #[must_use]
    pub const fn format(&self) -> GitObjectFormat {
        self.format
    }

    /// Fixed-width storage, including the canonical zero tail for SHA-1.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// Create a `GitOid` from a 20-byte SHA-1 digest.
    #[must_use]
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "Copying a fixed 20-byte SHA-1 into a 32-byte buffer; loop bound is constant"
    )]
    pub const fn from_sha1(bytes: [u8; 20]) -> Self {
        let mut full = [0u8; 32];
        let mut i = 0;
        while i < bytes.len() {
            full[i] = bytes[i];
            i += 1;
        }
        Self {
            format: GitObjectFormat::Sha1,
            bytes: full,
        }
    }

    /// Create a `GitOid` from a 32-byte SHA-256 digest.
    #[must_use]
    pub const fn from_sha256(bytes: [u8; 32]) -> Self {
        Self {
            format: GitObjectFormat::Sha256,
            bytes,
        }
    }

    /// Returns the number of significant bytes for this object format.
    #[must_use]
    pub const fn digest_len(&self) -> usize {
        match self.format {
            GitObjectFormat::Sha1 => 20,
            GitObjectFormat::Sha256 => 32,
        }
    }

    /// Returns the lowercase hex representation of the significant digest bytes.
    #[must_use]
    #[expect(
        clippy::as_conversions,
        clippy::indexing_slicing,
        reason = "Hex nibble lookup uses a fixed table; byte index is bounded by digest_len"
    )]
    pub fn to_hex(&self) -> String {
        const HEX: [char; 16] = [
            '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
        ];
        let len = self.digest_len();
        let mut out = String::with_capacity(len.saturating_mul(2));
        for &b in self.bytes.iter().take(len) {
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 0x0f) as usize]);
        }
        out
    }
}

impl<'de> Deserialize<'de> for GitOid {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Keep the existing named fields and Postcard sequence unchanged.
        #[derive(Deserialize)]
        #[serde(rename = "GitOid")]
        struct WireOid {
            format: GitObjectFormat,
            bytes: [u8; 32],
        }
        let wire = WireOid::deserialize(deserializer)?;
        Self::new(wire.format, wire.bytes)
            .ok_or_else(|| serde::de::Error::custom("SHA-1 OID has nonzero padding"))
    }
}

impl core::fmt::Display for GitOid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl GitOid {
    /// Parse a `GitOid` from its lowercase hex representation.
    ///
    /// Accepts 40 hex chars (SHA-1) or 64 hex chars (SHA-256). Returns `None`
    /// for any other length or a non-hex character.
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "Byte index is bounded by the validated hex length; nibble math on u8 is safe"
    )]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let bytes = hex.as_bytes();
        let format = match bytes.len() {
            40 => GitObjectFormat::Sha1,
            64 => GitObjectFormat::Sha256,
            _ => return None,
        };
        let mut out = [0u8; 32];
        for i in 0..bytes.len() / 2 {
            let hi = hex_nibble(bytes[i * 2])?;
            let lo = hex_nibble(bytes[i * 2 + 1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self { format, bytes: out })
    }
}

/// Decode a single ASCII hex nibble into its 0–15 value.
#[must_use]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Nibble arithmetic on validated ASCII hex digits is bounded to 0..=15"
)]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A Git author or committer signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSignature {
    /// Display name.
    pub name: Payload,
    /// Email address.
    pub email: Payload,
    /// Unix timestamp (seconds).
    pub when: i64,
}

// ---------------------------------------------------------------------------
// Git commit entity
// ---------------------------------------------------------------------------

/// Availability of a commit's object data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitAvailability {
    /// Object resolved from a live repository.
    Resolved,
    /// Represented in `EditChain` but absent from the current object database.
    ImportedOnly,
    /// Discovered live but not imported into `EditChain`.
    LiveOnly,
    /// Referenced but missing from the object database (e.g. shallow clone).
    MissingFromObjectDatabase,
}

/// A Git commit entity in the unified history model.
///
/// A commit may be imported only, live only, or both. When both are present
/// they are merged into one entity keyed by `(RepositoryId, GitOid)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitEntity {
    /// Repository this commit belongs to.
    pub repository: RepositoryId,
    /// Object format of the repository.
    pub object_format: GitObjectFormat,
    /// Full commit OID.
    pub oid: GitOid,
    /// `EditChain` operation that imported this commit, if any.
    pub imported_record: Option<OpId>,
    /// Availability of the underlying object data.
    pub availability: GitAvailability,
    /// Tree OID referenced by this commit.
    pub tree: GitOid,
    /// Parent commit OIDs (ancestry).
    pub parents: Vec<GitOid>,
    /// Author signature.
    pub author: GitSignature,
    /// Committer signature.
    pub committer: GitSignature,
    /// Author timestamp (Unix seconds).
    pub authored_at: i64,
    /// Commit timestamp (Unix seconds).
    pub committed_at: i64,
    /// Commit message (subject + body).
    pub message: Payload,
    /// Refs observed at import time (snapshot).
    pub imported_refs: Vec<Payload>,
    /// Refs observed live (snapshot; may change).
    pub live_refs: Vec<Payload>,
    /// Paths changed by this commit.
    pub changed_paths: Vec<PathId>,
}

impl GitCommitEntity {
    /// Repository-qualified identity used by graph and view consumers.
    #[must_use]
    pub const fn key(&self) -> GitCommitKey {
        GitCommitKey::new(self.repository, self.oid)
    }
}

// ---------------------------------------------------------------------------
// Explicit EditChain-to-Git links
// ---------------------------------------------------------------------------

/// The relation an explicit link records between an `EditChain` operation and a
/// Git object. The viewer may draw this stored relation as a graph edge, but
/// it never rewrites the operation's durable causal [`crate::ParentSet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitLinkKind {
    /// The operation was based on this commit's state.
    BasedOn,
    /// The operation represents a checkpoint of work.
    Checkpoint,
    /// The operation's work was committed as this commit.
    CommittedAs,
    /// The operation produced this commit (for example, a successful shell
    /// command that ran `git commit`). Unlike the other link kinds, projection
    /// renders the operation as a causal parent of the commit.
    ProducedBy,
    /// The operation mentions this commit in its content.
    Mentions,
    /// A custom relation kind supplied by an importer or producer.
    Custom(Payload),
}

/// An explicit link from an `EditChain` operation to a Git object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLink {
    /// The `EditChain` operation that is the source of the link.
    pub source: OpId,
    /// Repository containing the target object.
    pub target_repo: RepositoryId,
    /// Target object OID.
    pub target_oid: GitOid,
    /// Stored relation kind.
    pub kind: GitLinkKind,
}

impl GitLink {
    /// Repository-qualified target, shared by ancestry and display adapters.
    #[must_use]
    pub const fn target_key(&self) -> GitCommitKey {
        GitCommitKey::new(self.target_repo, self.target_oid)
    }
}

impl Ord for GitOid {
    fn cmp(&self, other: &Self) -> Ordering {
        self.format
            .cmp(&other.format)
            .then_with(|| self.bytes.cmp(&other.bytes))
    }
}

impl PartialOrd for GitOid {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RepositoryId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for RepositoryId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
