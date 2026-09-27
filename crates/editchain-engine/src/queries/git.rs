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

impl ChainQueries {
    /// Read Git observations, preserving imported/live ref snapshots separately.
    ///
    /// Multiple observations of a commit remain separate evidence-bearing records.
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
            key.is_some_and(|(repository, oid)| {
                repository == query.repository && query.oid.is_none_or(|target| target == oid)
            })
        });
        Ok(history)
    }
}
