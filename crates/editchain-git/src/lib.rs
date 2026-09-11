//! Live Git repository resolution for the unified history model.
//!
//! This crate wraps `gix` to discover repositories in a workspace, resolve
//! commit/tree/blob/ref/diff objects without mutating or fetching.

#[cfg(test)]
use tempfile as _;

mod catalog;
pub use catalog::{DiscoveryIssue, RepositoryCatalog};
mod observation;
pub use observation::{HistoryRead, HistoryReadIssue, RefSnapshot};

/// Commit tree diffs and immutable blob resolution.
pub mod diff;
/// Repository discovery and identity derivation.
pub mod discover;
/// Object resolution (commits, trees, refs, diffs) via `gix`.
pub mod resolve;

pub use diff::{
    commit_file_changes, resolve_blob, resolve_path_at_commit, GitBlob, GitFileChange,
    GitFileStatus, GitPathObject,
};
pub use discover::{discover_repositories, open_repository, RepositoryDiscovery, RepositoryHandle};
pub use resolve::{
    resolve_branch_tip_at_time, resolve_commit, resolve_commit_prefix, walk_history,
    ResolutionError,
};

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
