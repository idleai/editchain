//! Merging imported and live commit entities.

use editchain_core::{GitAvailability, GitCommitEntity};

/// The outcome of merging an imported and a live commit entity.
#[derive(Debug, Clone)]
pub struct MergeOutcome {
    /// The merged commit entity.
    pub merged: GitCommitEntity,
    /// Whether the live data was used (vs imported-only).
    pub used_live: bool,
}

/// Merge an imported and a live commit entity into one.
///
/// Both are keyed by `(RepositoryId, GitOid)`. Live data wins for object
/// contents when available; imported metadata (e.g. `imported_record`) is
/// retained. If either is absent, the other is returned unchanged.
#[must_use]
pub fn merge_commit_entities(
    imported: Option<GitCommitEntity>,
    live: Option<GitCommitEntity>,
) -> Option<MergeOutcome> {
    match (imported, live) {
        (Some(imp), Some(live)) => {
            let mut merged = live;
            // Retain the imported record link.
            merged.imported_record = imp.imported_record;
            // Merge refs: prefer live, fall back to imported.
            if merged.live_refs.is_empty() {
                merged.live_refs = imp.live_refs;
            }
            if merged.imported_refs.is_empty() {
                merged.imported_refs = imp.imported_refs;
            }
            merged.availability = GitAvailability::Resolved;
            Some(MergeOutcome {
                merged,
                used_live: true,
            })
        }
        (Some(imp), None) => Some(MergeOutcome {
            merged: imp,
            used_live: false,
        }),
        (None, Some(live)) => Some(MergeOutcome {
            merged: live,
            used_live: true,
        }),
        (None, None) => None,
    }
}
