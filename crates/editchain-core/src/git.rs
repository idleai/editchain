#[cfg(not(feature = "use-std"))]
use alloc::collections::BTreeMap;
#[cfg(not(feature = "use-std"))]
use alloc::{string::String, vec::Vec};
use core::cmp::Ordering;
use serde::{Deserialize, Serialize};
#[cfg(feature = "use-std")]
use std::collections::BTreeMap;

use crate::ids::{OpId, PathId};
use crate::op::{Op, OpKind};
use crate::payload::Payload;

// ---------------------------------------------------------------------------
// Git identity
// ---------------------------------------------------------------------------

/// A repository identifier — 64 bits wide.
///
/// Derived deterministically from the canonical workspace root plus the
/// repository-relative path, so the same repository always maps to the same
/// `RepositoryId` across imports and live resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepositoryId(pub u64);

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GitOid {
    /// Object format this OID was produced under.
    pub format: GitObjectFormat,
    /// Full OID bytes (32 bytes; SHA-1 occupies the first 20).
    pub bytes: [u8; 32],
}

impl GitOid {
    /// Create a new `GitOid` from a full 32-byte digest.
    #[must_use]
    pub const fn new(format: GitObjectFormat, bytes: [u8; 32]) -> Self {
        Self { format, bytes }
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

impl core::fmt::Display for GitOid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
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

// ---------------------------------------------------------------------------
// Explicit EditChain-to-Git links
// ---------------------------------------------------------------------------

/// The relation an explicit link records between an `EditChain` operation and a
/// Git object. The viewer displays this relation without strengthening or
/// reinterpreting it — a link never becomes a causal parent merely because
/// both appear in one view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitLinkKind {
    /// The operation was based on this commit's state.
    BasedOn,
    /// The operation represents a checkpoint of work.
    Checkpoint,
    /// The operation's work was committed as this commit.
    CommittedAs,
    /// The operation produced this commit (e.g. a git push/tool call).
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

// ---------------------------------------------------------------------------
// Deterministic git projection
// ---------------------------------------------------------------------------

/// A deterministic projection of git history from a set of operations.
///
/// Recomputable from any replica with the same operations: commits are keyed
/// by `(RepositoryId, GitOid)` and links are keyed by their source `OpId`.
/// This projection is the git analogue of `CanonicalView`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitProjection {
    /// Commits keyed by `(RepositoryId, GitOid)`.
    pub commits: BTreeMap<(RepositoryId, GitOid), GitCommitEntity>,
    /// Explicit links keyed by source `OpId`.
    pub links: BTreeMap<OpId, Vec<GitLink>>,
}

impl GitProjection {
    /// Create an empty projection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            commits: BTreeMap::new(),
            links: BTreeMap::new(),
        }
    }

    /// Reduce a single operation into this projection.
    ///
    /// Handles `OpKind::GitCommit` and `OpKind::GitLink`; all other kinds are
    /// ignored. A later commit with the same `(RepositoryId, GitOid)` replaces
    /// an earlier one (last-writer-wins by iteration order).
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "GitProjection only handles git kinds; all other kinds are silently ignored"
    )]
    pub fn reduce(&mut self, op: &Op) {
        match &op.kind {
            OpKind::GitCommit(commit) => {
                let key = (commit.repository, commit.oid);
                drop(self.commits.insert(key, (**commit).clone()));
            }
            OpKind::GitLink(link) => {
                let entry = self.links.entry(link.source).or_default();
                entry.push(link.clone());
            }
            _ => {}
        }
    }

    /// Reduce a sequence of operations into this projection.
    #[must_use]
    pub fn from_ops(ops: &[Op]) -> Self {
        let mut proj = Self::new();
        for op in ops {
            proj.reduce(op);
        }
        proj
    }

    /// Returns the commit for a given repository and OID, if present.
    #[must_use]
    pub fn commit(&self, repository: RepositoryId, oid: &GitOid) -> Option<&GitCommitEntity> {
        self.commits.get(&(repository, *oid))
    }

    /// Returns the explicit links originating from an operation.
    #[must_use]
    pub fn links_from(&self, source: &OpId) -> &[GitLink] {
        self.links.get(source).map_or(&[], Vec::as_slice)
    }
}
