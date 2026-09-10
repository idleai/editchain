//! Live ref observations kept separate from immutable commit object reads.

use std::collections::BTreeMap;

use editchain_core::{GitCommitEntity, GitOid};

use crate::resolve::git_oid_from_gix;
use crate::{RepositoryHandle, ResolutionError};

/// Refs observed in one pass over a repository, indexed by exact target OID.
#[derive(Debug, Clone, Default)]
pub struct RefSnapshot {
    refs: BTreeMap<GitOid, Vec<Vec<u8>>>,
}

impl RefSnapshot {
    /// Capture direct refs without mutating or fetching.
    ///
    /// # Errors
    ///
    /// Returns an error if any ref cannot be enumerated or decoded.
    pub fn capture(handle: &RepositoryHandle) -> Result<Self, ResolutionError> {
        let references = handle
            .repo
            .references()
            .map_err(|error| ResolutionError::Decode(error.to_string()))?;
        let all = references
            .all()
            .map_err(|error| ResolutionError::Decode(error.to_string()))?;
        let mut snapshot = Self::default();
        for reference in all {
            let reference =
                reference.map_err(|error| ResolutionError::Decode(error.to_string()))?;
            if reference.target().try_id().is_some() {
                snapshot
                    .refs
                    .entry(git_oid_from_gix(&reference.id().detach()))
                    .or_default()
                    .push(reference.name().as_bstr().to_vec());
            }
        }
        for names in snapshot.refs.values_mut() {
            names.sort();
            names.dedup();
        }
        Ok(snapshot)
    }

    /// Names observed pointing directly at this object, sorted by raw bytes.
    #[must_use]
    pub fn refs_for(&self, oid: &GitOid) -> &[Vec<u8>] {
        self.refs.get(oid).map_or(&[], Vec::as_slice)
    }
}

/// One failed observation or object read during a history walk.
#[derive(Debug)]
pub struct HistoryReadIssue {
    /// Object whose read failed, when an exact identity was available.
    pub oid: Option<GitOid>,
    /// Ref, traversal, or commit decoding failure.
    pub error: ResolutionError,
}

/// Available commits plus explicit limits and gaps in the history read.
#[derive(Debug, Default)]
pub struct HistoryRead {
    /// Resolved immutable objects with labels from this read's ref snapshot.
    pub commits: Vec<GitCommitEntity>,
    /// Ref labels captured once for the whole walk.
    pub refs: RefSnapshot,
    /// Failed reads that previously disappeared from the result.
    pub issues: Vec<HistoryReadIssue>,
    /// The requested commit limit stopped traversal before exhaustion.
    pub truncated: bool,
    /// A shallow boundary limits available ancestry.
    pub shallow: bool,
}

impl HistoryRead {
    /// Whether traversal exhausted the available full history without any gaps.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.issues.is_empty() && !self.truncated && !self.shallow
    }
}
