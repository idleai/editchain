//! Ordered Git observation policy for the history projection.

use std::collections::BTreeMap;

use editchain_core::{GitCommitEntity, GitLink, GitOid, Op, OpId, OpKind, RepositoryId};

/// Git observations and explicit links for an ordered history projection.
///
/// Replaying the same input order produces the same view. A later observation
/// replaces the commit at its repository-qualified key; links retain input
/// order. This mutable view is separate from immutable core operation facts.
#[derive(Debug, Clone, Default)]
pub struct GitProjection {
    /// Commits keyed by `(RepositoryId, GitOid)`.
    commits: BTreeMap<(RepositoryId, GitOid), GitCommitEntity>,
    /// Explicit links keyed by source `OpId`.
    links: BTreeMap<OpId, Vec<GitLink>>,
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
    pub fn reduce(&mut self, op: &Op) {
        if let OpKind::GitCommit(commit) = &op.kind {
            self.observe_commit((**commit).clone());
        } else if let OpKind::GitLink(link) = &op.kind {
            self.links
                .entry(link.source)
                .or_default()
                .push(link.clone());
        }
    }

    /// Replace one commit observation, preserving the supplied repository key.
    pub fn observe_commit(&mut self, commit: GitCommitEntity) {
        drop(self.commits.insert((commit.repository, commit.oid), commit));
    }

    /// Current commit observations in repository/OID order.
    #[must_use]
    pub const fn commits(&self) -> &BTreeMap<(RepositoryId, GitOid), GitCommitEntity> {
        &self.commits
    }

    /// Explicit links grouped by their source operation.
    #[must_use]
    pub const fn links(&self) -> &BTreeMap<OpId, Vec<GitLink>> {
        &self.links
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
