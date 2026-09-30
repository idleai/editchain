//! Git queries report recorded observations, never current repository state.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{GitOid, OpKind, RepositoryId};

use super::{ChainQueries, HistoryEntry, PageRequest, QueryPage};

/// Select recorded Git commit observations and explicit links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitQuery {
    /// Required repository identity; equal OIDs in other repositories stay separate.
    pub repository: RepositoryId,
    /// Exact object identity, or all objects recorded for the repository.
    pub oid: Option<GitOid>,
}

impl GitQuery {
    fn matches(self, repository: RepositoryId, oid: GitOid) -> bool {
        repository == self.repository && self.oid.is_none_or(|target| target == oid)
    }
}

impl ChainQueries {
    /// Read Git observations, preserving imported/live ref snapshots separately.
    ///
    /// Multiple observations of a commit remain separate records with their own references.
    /// Availability is what the producer recorded, not a claim about today's object
    /// database. `page` bounds scanned operations; continue through empty pages.
    /// Resolve ref payloads using [`Self::content`] and `GitImportedRef`/`GitLiveRef`.
    /// Missing objects stay visible through explicit links even without a commit record.
    ///
    /// # Errors
    /// Returns invalid page limits or index/source IO errors.
    pub fn git(&self, query: GitQuery, page: PageRequest) -> io::Result<QueryPage<HistoryEntry>> {
        let mut history = self.history(None, page)?;
        history.items.retain(|entry| {
            let key = match &entry.operation.kind {
                OpKind::Activity(record) => match &record.kind {
                    editchain_core::activity::Kind::Commit(commit) => {
                        Some((commit.repository, commit.oid))
                    }
                    editchain_core::activity::Kind::Link(link) => {
                        return link.to.iter().any(|target| match target {
                            editchain_core::activity::Entity::Git { repository, oid } => {
                                query.matches(*repository, *oid)
                            }
                            editchain_core::activity::Entity::Operation(_)
                            | editchain_core::activity::Entity::Item(_) => false,
                        });
                    }
                    editchain_core::activity::Kind::Session(_)
                    | editchain_core::activity::Kind::Turn(_)
                    | editchain_core::activity::Kind::Message(_)
                    | editchain_core::activity::Kind::Tool(_)
                    | editchain_core::activity::Kind::File(_)
                    | editchain_core::activity::Kind::Note(_)
                    | editchain_core::activity::Kind::Author(_)
                    | editchain_core::activity::Kind::Original(_) => None,
                },
                OpKind::GitCommit(commit) => Some((commit.repository, commit.oid)),
                OpKind::GitLink(link) => Some((link.target_repo, link.target_oid)),
                OpKind::ChainStart(_)
                | OpKind::Actor(_)
                | OpKind::Session(_)
                | OpKind::Message(_)
                | OpKind::Tool(_)
                | OpKind::Command(_)
                | OpKind::File(_)
                | OpKind::Reflection(_)
                | OpKind::Import(_)
                | OpKind::Note(_)
                | OpKind::Error(_)
                | OpKind::Unknown(_) => None,
            };
            key.is_some_and(|(repository, oid)| query.matches(repository, oid))
        });
        Ok(history)
    }
}
