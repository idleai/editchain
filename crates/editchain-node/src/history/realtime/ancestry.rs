//! Lift source order through first appearances; resolve references through revisions.

use editchain_core::{GitCommitKey, GitLink, GitLinkKind, Op, OpId, OpKind};
use editchain_project::{live::LiveProjection, live::LiveRow};
use std::collections::{BTreeSet, HashMap, HashSet};

#[cfg(test)]
mod tests;

#[derive(Debug, Default)]
pub(super) struct Ancestry {
    owners: HashMap<OpId, BTreeSet<String>>,
    owned: HashMap<String, Vec<OpId>>,
    appearances: HashMap<OpId, BTreeSet<String>>,
    first: HashMap<String, OpId>,
    roots: HashMap<String, Vec<OpId>>,
    root_users: HashMap<OpId, HashSet<String>>,
    memo: HashMap<OpId, BTreeSet<String>>,
    dependencies: HashMap<OpId, Vec<OpId>>,
    users: HashMap<OpId, HashSet<OpId>>,
    pending: BTreeSet<String>,
    links: HashMap<OpId, GitLink>,
    based: HashMap<OpId, HashMap<OpId, String>>,
    produced: HashMap<String, HashMap<OpId, OpId>>,
    references: HashMap<String, BTreeSet<String>>,
    reference_users: HashMap<String, HashSet<String>>,
}

impl Ancestry {
    pub(super) fn observe_links(&mut self, added: &[Op], removed: &[OpId]) {
        for id in removed {
            if let Some(link) = self.links.remove(id) {
                self.link(*id, &link, false);
            }
        }
        for op in added {
            if let OpKind::GitLink(link) = &op.kind {
                self.link(op.id, link, true);
                drop(self.links.insert(op.id, link.clone()));
            }
        }
    }

    fn link(&mut self, id: OpId, link: &GitLink, added: bool) {
        let target = GitCommitKey::new(link.target_repo, link.target_oid).to_string();
        if link.kind == GitLinkKind::ProducedBy {
            let links = self.produced.entry(target.clone()).or_default();
            if added {
                let _: Option<OpId> = links.insert(id, link.source);
            } else {
                let _: Option<OpId> = links.remove(&id);
            }
            let mut roots: Vec<_> = links.values().copied().collect();
            roots.sort_unstable();
            roots.dedup();
            for old in self.roots.remove(&target).into_iter().flatten() {
                if let Some(users) = self.root_users.get_mut(&old) {
                    let _: bool = users.remove(&target);
                }
            }
            for source in &roots {
                let _: bool = self
                    .root_users
                    .entry(*source)
                    .or_default()
                    .insert(target.clone());
            }
            drop(self.roots.insert(target.clone(), roots));
            let _: bool = self.pending.insert(target);
        } else {
            let links = self.based.entry(link.source).or_default();
            if added {
                drop(links.insert(id, target));
            } else {
                drop(links.remove(&id));
            }
            self.invalidate([link.source]);
        }
    }

    pub(super) fn invalidate(&mut self, ids: impl IntoIterator<Item = OpId>) {
        let mut pending: Vec<_> = ids.into_iter().collect();
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            self.pending
                .extend(self.root_users.get(&id).into_iter().flatten().cloned());
            pending.extend(self.users.remove(&id).into_iter().flatten());
            drop(self.memo.remove(&id));
            for dependency in self.dependencies.remove(&id).into_iter().flatten() {
                if let Some(users) = self.users.get_mut(&dependency) {
                    let _: bool = users.remove(&id);
                }
            }
        }
    }

    pub(super) fn remove(&mut self, key: &str) {
        if !self.owned.contains_key(key) {
            return;
        }
        self.pending
            .extend(self.reference_users.get(key).into_iter().flatten().cloned());
        for root in self.roots.remove(key).into_iter().flatten() {
            if let Some(users) = self.root_users.get_mut(&root) {
                let _: bool = users.remove(key);
            }
        }
        let ids = self.owned.remove(key).unwrap_or_default();
        if let Some(first) = self.first.remove(key) {
            if let Some(owners) = self.appearances.get_mut(&first) {
                let _: bool = owners.remove(key);
            }
        }
        for id in &ids {
            if let Some(owners) = self.owners.get_mut(id) {
                let _: bool = owners.remove(key);
            }
        }
        self.invalidate(ids);
    }

    pub(super) fn put(&mut self, input: &LiveRow, projection: &LiveProjection) {
        self.remove(&input.key);
        let mut ids: Vec<_> = input.operations.iter().map(|op| op.id).collect();
        ids.push(input.incarnation);
        ids.sort_unstable();
        ids.dedup();
        for id in &ids {
            let _: bool = self
                .owners
                .entry(*id)
                .or_default()
                .insert(input.key.clone());
        }
        self.invalidate(ids.iter().copied());
        drop(self.owned.insert(input.key.clone(), ids));
        let _: bool = self
            .appearances
            .entry(input.incarnation)
            .or_default()
            .insert(input.key.clone());
        let _: Option<OpId> = self.first.insert(input.key.clone(), input.incarnation);
        let roots: Vec<_> = projection
            .operation(input.incarnation)
            .map(|op| op.parents.iter().copied().collect())
            .unwrap_or_default();
        for root in &roots {
            let _: bool = self
                .root_users
                .entry(*root)
                .or_default()
                .insert(input.key.clone());
        }
        drop(self.roots.insert(input.key.clone(), roots));
        let _: bool = self.pending.insert(input.key.clone());
    }

    pub(super) fn changed(&mut self, projection: &LiveProjection) -> Vec<(String, Vec<String>)> {
        let pending = std::mem::take(&mut self.pending);
        let mut result = Vec::new();
        for key in pending {
            let Some(roots) = self.roots.get(&key).cloned() else {
                continue;
            };
            for owner in self.references.remove(&key).into_iter().flatten() {
                if let Some(users) = self.reference_users.get_mut(&owner) {
                    let _: bool = users.remove(&key);
                }
            }
            let mut parents = BTreeSet::new();
            for root in roots {
                let references = if self.produced.contains_key(&key) {
                    projection
                        .item_owners(root)
                        .into_iter()
                        .chain(self.owners.get(&root).into_iter().flatten().cloned())
                        .filter(|owner| self.owned.contains_key(owner))
                        .collect::<BTreeSet<_>>()
                } else {
                    BTreeSet::new()
                };
                if references.is_empty() {
                    parents.extend(self.resolve(root, projection));
                } else {
                    for owner in &references {
                        let _: bool = self
                            .reference_users
                            .entry(owner.clone())
                            .or_default()
                            .insert(key.clone());
                    }
                    self.references
                        .entry(key.clone())
                        .or_default()
                        .extend(references.iter().cloned());
                    parents.extend(references);
                }
            }
            let _: bool = parents.remove(&key);
            result.push((key, parents.into_iter().collect()));
        }
        result
    }

    fn resolve(&mut self, root: OpId, projection: &LiveProjection) -> BTreeSet<String> {
        let mut stack = vec![(root, false)];
        let mut visiting = HashSet::new();
        while let Some((id, finish)) = stack.pop() {
            if self.memo.contains_key(&id) {
                continue;
            }
            let owners: BTreeSet<_> = self
                .appearances
                .get(&id)
                .into_iter()
                .flatten()
                .cloned()
                .chain(
                    self.based
                        .get(&id)
                        .into_iter()
                        .flat_map(|links| links.values().cloned()),
                )
                .collect();
            if !owners.is_empty() {
                drop(self.memo.insert(id, owners));
                continue;
            }
            let dependencies: Vec<_> = projection
                .operation(id)
                .map(|op| op.parents.iter().copied().collect())
                .unwrap_or_default();
            if !finish {
                if !visiting.insert(id) {
                    continue;
                }
                stack.push((id, true));
                stack.extend(
                    dependencies
                        .iter()
                        .filter(|parent| !visiting.contains(parent))
                        .map(|parent| (*parent, false)),
                );
                continue;
            }
            let mut owners = BTreeSet::new();
            for dependency in &dependencies {
                owners.extend(self.memo.get(dependency).into_iter().flatten().cloned());
                let _: bool = self.users.entry(*dependency).or_default().insert(id);
            }
            drop(self.dependencies.insert(id, dependencies));
            drop(self.memo.insert(id, owners));
        }
        self.memo.get(&root).cloned().unwrap_or_default()
    }
}
