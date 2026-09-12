//! Resolve only newly reachable Git objects; retain prior commit data and refs.

use super::{
    super::{files::git_file_change_index, parse_git_oid, HistoryWindowOptions, Workspace},
    LiveWorkspace, Result,
};
use editchain_core::{GitCommitEntity, GitOid, Payload, RepositoryId};
use editchain_git::{open_repository, RefSnapshot, RepositoryCatalog, RepositoryHandle};
use editchain_index::Map as HashMap;
use editchain_project::HistoryProjection;
use editchain_protocol::{rank::Measure, LiveBlock, LiveBlockMeta};
use std::collections::{BTreeMap, HashSet};
mod checkpoint;
pub(super) use checkpoint::Saved;

#[derive(Debug)]
struct Repository {
    handle: RepositoryHandle,
    refs: BTreeMap<GitOid, Vec<Vec<u8>>>,
    head: Option<GitOid>,
    pending: HashSet<GitOid>,
    initialized: bool,
}

#[derive(Debug)]
pub(super) struct GitTracker {
    repositories: Vec<Repository>,
    commits: HashMap<(RepositoryId, GitOid), GitCommitEntity>,
}

impl GitTracker {
    pub(super) fn follow_links(&mut self, ops: &[editchain_core::Op]) {
        for op in ops {
            if let editchain_core::OpKind::GitLink(link) = &op.kind {
                if self
                    .commits
                    .contains_key(&(link.target_repo, link.target_oid))
                {
                    continue;
                }
                if let Some(repository) = self
                    .repositories
                    .iter_mut()
                    .find(|repo| repo.handle.discovery.id == link.target_repo)
                {
                    let _: bool = repository.pending.insert(link.target_oid);
                }
            }
        }
    }

    pub(super) fn parent_keys(&self, key: &str) -> Vec<String> {
        let Some(key) = editchain_core::GitCommitKey::from_display_str(key) else {
            return Vec::new();
        };
        self.commits
            .get(&(key.repository, key.oid))
            .map(|commit| {
                commit
                    .parents
                    .iter()
                    .map(|oid| editchain_core::GitCommitKey::new(key.repository, *oid).to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn new(catalog: &RepositoryCatalog) -> Result<Self> {
        let repositories = catalog
            .iter()
            .map(|entry| {
                Ok(Repository {
                    handle: open_repository(entry)?,
                    refs: BTreeMap::new(),
                    head: None,
                    pending: HashSet::new(),
                    initialized: false,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            repositories,
            commits: HashMap::new(),
        })
    }

    pub(super) fn poll(&mut self) -> Result<Vec<GitCommitEntity>> {
        let mut output = Vec::new();
        for repository in &mut self.repositories {
            let snapshot = RefSnapshot::capture(&repository.handle)?;
            let refs: BTreeMap<_, _> = snapshot
                .entries()
                .map(|(oid, names)| (*oid, names.to_vec()))
                .collect();
            let head = repository
                .handle
                .repo
                .head_id()
                .ok()
                .and_then(|id| parse_git_oid(&id.to_string()).ok());
            if repository.initialized
                && refs == repository.refs
                && head == repository.head
                && repository.pending.is_empty()
            {
                continue;
            }
            let repo = repository.handle.discovery.id;
            let mut pending: Vec<_> = head.into_iter().chain(repository.pending.drain()).collect();
            let mut seen = HashSet::new();
            while let Some(oid) = pending.pop() {
                if !seen.insert(oid) || self.commits.contains_key(&(repo, oid)) {
                    continue;
                }
                match editchain_git::resolve::resolve_commit_with_refs(
                    &repository.handle,
                    &oid,
                    &snapshot,
                ) {
                    Ok(commit) => {
                        pending.extend(commit.parents.iter().copied());
                        drop(self.commits.insert((repo, oid), commit.clone()));
                        output.push(commit);
                    }
                    Err(editchain_git::ResolutionError::NotFound(_)) => {
                        let _: bool = repository.pending.insert(oid);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            for oid in refs
                .keys()
                .chain(repository.refs.keys())
                .copied()
                .collect::<HashSet<_>>()
            {
                if refs.get(&oid) == repository.refs.get(&oid) {
                    continue;
                }
                if let Some(commit) = self.commits.get_mut(&(repo, oid)) {
                    commit.live_refs = snapshot
                        .refs_for(&oid)
                        .iter()
                        .cloned()
                        .map(Payload::Inline)
                        .collect();
                    output.push(commit.clone());
                }
            }
            repository.refs = refs;
            repository.head = head;
            repository.initialized = true;
        }
        let mut unique = BTreeMap::new();
        for commit in output {
            drop(unique.insert((commit.repository, commit.oid), commit));
        }
        Ok(unique.into_values().collect())
    }

    pub(super) fn commit(&self, repository: RepositoryId, oid: GitOid) -> Option<&GitCommitEntity> {
        self.commits.get(&(repository, oid))
    }
}

impl LiveWorkspace {
    pub(super) fn git_workspace(&self, commit: &GitCommitEntity) -> Workspace {
        let mut projection = HistoryProjection::new();
        projection.merge_git_commits(vec![commit.clone()]);
        let mut workspace = Workspace::from_projection(projection);
        workspace.repositories = self.catalog.clone();
        workspace.root_path.clone_from(&self.root);
        workspace.git_file_changes =
            git_file_change_index(&workspace.projection, self.catalog.entries());
        workspace
    }

    pub(super) fn apply_git(
        &mut self,
        commits: Vec<GitCommitEntity>,
    ) -> Result<Vec<super::StoredBlock>> {
        let mut blocks = Vec::new();
        for commit in commits {
            let mut workspace = self.git_workspace(&commit);
            let window = workspace.history_window(HistoryWindowOptions {
                offset: 0,
                limit: u64::MAX,
                include_layout: false,
            })?;
            let Some(first) = window.rows.first() else {
                continue;
            };
            let meta = LiveBlockMeta {
                task_group: None,
                task_summary: None,
                task_protected: true,
                key: first.node_key.clone(),
                sort_time: first.timestamp_ms,
                row_count: window.total,
                spans: window.expansion_spans.unwrap_or_default(),
                node_key: first.node_key.clone(),
                parents: commit
                    .parents
                    .iter()
                    .map(|oid| {
                        editchain_core::GitCommitKey::new(commit.repository, *oid).to_string()
                    })
                    .collect(),
                chain_state: first.chain_state,
            };
            let mut block = LiveBlock {
                meta,
                rows: window.rows,
            };
            for (slot, row) in block.rows.iter_mut().enumerate() {
                row.continuity_key = if slot == 0 {
                    block.meta.key.clone()
                } else if let Some(change) = &row.file_change {
                    format!("{}:file:{}", block.meta.key, change.path)
                } else {
                    format!("{}:child:{slot}", block.meta.key)
                };
            }
            let _existed = self.remove_block(&block.meta.key);
            if let Some(search) = &mut self.search {
                search.put(&block)?;
            }
            let block = self.rows.put(block)?;
            let order = block.meta.order();
            drop(self.orders.insert(block.meta.key.clone(), order.clone()));
            drop(self.blocks.insert(
                order,
                block.clone(),
                Measure {
                    expanded: block.meta.row_count,
                    visible: block.meta.row_count,
                },
            ));
            blocks.push(block);
        }
        Ok(blocks)
    }
}
