//! Live Git repository resolution for the unified history model.
//!
//! This crate wraps `gix` to discover repositories in a workspace, resolve
//! commit/tree/blob/ref/diff objects without mutating or fetching, and merge
//! imported and live commit entities by `(RepositoryId, GitOid)`.

#[cfg(test)]
use tempfile as _;

/// Repository discovery and identity derivation.
pub mod discover;
/// Merging imported and live commit entities.
pub mod merge;
/// Object resolution (commits, trees, refs, diffs) via `gix`.
pub mod resolve;

pub use discover::{discover_repositories, RepositoryDiscovery, RepositoryHandle};
pub use merge::{merge_commit_entities, MergeOutcome};
pub use resolve::{resolve_commit, walk_history, CommitResolution, ResolutionError};

use editchain_core::RepositoryId;

/// Derive a deterministic `RepositoryId` from a canonical repository path.
///
/// Uses SHA-256 of the canonicalized path so the same repository always maps
/// to the same ID across imports and live resolution.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "SHA-256 digest is always at least 8 bytes; slicing the first 8 is safe"
)]
pub fn repository_id_from_path(path: &std::path::Path) -> RepositoryId {
    use sha2::{Digest, Sha256};
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    // Take the first 8 bytes as a u64.
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    RepositoryId(u64::from_le_bytes(bytes))
}
