//! Persist object/ref frontiers, reopening only repository handles at startup.

use super::{GitTracker, Result};
use editchain_core::{GitCommitEntity, GitOid, RepositoryId};
use editchain_index::Map;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Serialize, Deserialize)]
struct Repository {
    id: RepositoryId,
    refs: BTreeMap<GitOid, Vec<Vec<u8>>>,
    head: Option<GitOid>,
    pending: HashSet<GitOid>,
    initialized: bool,
}

#[derive(Debug, Deserialize)]
pub(in super::super) struct Saved {
    commits: Map<(RepositoryId, GitOid), GitCommitEntity>,
    repositories: Vec<Repository>,
}

#[derive(Serialize)]
struct Borrowed<'a> {
    commits: &'a Map<(RepositoryId, GitOid), GitCommitEntity>,
    repositories: Vec<Repository>,
}

impl Serialize for GitTracker {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        Borrowed {
            commits: &self.commits,
            repositories: self
                .repositories
                .iter()
                .map(|repo| Repository {
                    id: repo.handle.discovery.id,
                    refs: repo.refs.clone(),
                    head: repo.head,
                    pending: repo.pending.clone(),
                    initialized: repo.initialized,
                })
                .collect(),
        }
        .serialize(serializer)
    }
}

impl GitTracker {
    pub(in super::super) fn restore(&mut self, saved: Saved) -> Result<()> {
        if saved.repositories.len() != self.repositories.len() {
            return Err("checkpoint repository set changed; prepare the view again".into());
        }
        for state in saved.repositories {
            let repo = self
                .repositories
                .iter_mut()
                .find(|repo| repo.handle.discovery.id == state.id)
                .ok_or("checkpoint repository identity changed")?;
            repo.refs = state.refs;
            repo.head = state.head;
            repo.pending = state.pending;
            repo.initialized = state.initialized;
        }
        self.commits = saved.commits;
        Ok(())
    }
}
