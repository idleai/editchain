//! Retain only unresolved successful commit observations between live deltas.
use super::{produced::derive_produced_commit_links, Repositories};
use editchain_core::{Op, OpId, OpKind};
use editchain_git::RepositoryCatalog;
use editchain_import::{git_evidence::collect_commit_evidence, FsBlobSink};
use std::collections::HashMap;

#[derive(Debug)]
pub(crate) struct LiveReconciliation {
    repositories: Repositories,
    pending: HashMap<OpId, Op>,
    dirty: bool,
}

impl LiveReconciliation {
    pub(crate) fn new(catalog: &RepositoryCatalog) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            repositories: Repositories {
                workspace: std::path::PathBuf::new(),
                catalog: catalog.clone(),
                handles: catalog
                    .entries()
                    .iter()
                    .map(editchain_git::open_repository)
                    .collect::<Result<_, _>>()?,
            },
            pending: HashMap::new(),
            dirty: false,
        })
    }

    pub(crate) fn observe(
        &mut self,
        ops: &[Op],
        removed: impl Iterator<Item = OpId>,
        blobs: Option<&FsBlobSink>,
    ) {
        for id in removed {
            drop(self.pending.remove(&id));
        }
        for evidence in collect_commit_evidence(ops, blobs) {
            if evidence.invokes_git_commit() {
                drop(
                    self.pending
                        .insert(evidence.source.id, evidence.source.clone()),
                );
                self.dirty = true;
            }
        }
    }

    pub(crate) fn resolve(&mut self, git_changed: bool, blobs: Option<&FsBlobSink>) -> Vec<Op> {
        if !self.dirty && !git_changed {
            return Vec::new();
        }
        self.dirty = false;
        let ops: Vec<_> = self.pending.values().cloned().collect();
        let links = derive_produced_commit_links(&self.repositories, &ops, blobs);
        for link in &links {
            if let OpKind::GitLink(link) = &link.kind {
                drop(self.pending.remove(&link.source));
            }
        }
        links
    }
}
