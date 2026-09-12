//! A live checkpoint includes reducers, identities, grouping and graph state.
//! The row file is durable before the new root publishes the append frontier.

use super::{ancestry::Ancestry, git, tasks::Tasks, LiveWorkspace, Order, Result, StoredBlock};
use editchain_core::{Op, OpId};
use editchain_index::{Map, Storage};
use editchain_project::live::{LiveProjection, LiveRow};
use editchain_protocol::{live_graph::LiveGraph, rank::RankTree};
use editchain_store::IndexedTail;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, rc::Rc};

// Increment when reducer, routing, task or disk-index semantics change.
pub(super) const VERSION: u64 = 4;

#[derive(Serialize)]
struct Borrowed<'a> {
    version: u64,
    disclosure: &'a super::disclosure::Disclosure,
    workspace: &'a PathBuf,
    tail: &'a IndexedTail,
    projection: &'a LiveProjection,
    blocks: &'a RankTree<Order, StoredBlock>,
    orders: &'a Map<String, Order>,
    inputs: &'a Map<String, LiveRow>,
    owners: &'a Map<OpId, String>,
    ancestry: &'a Ancestry,
    graph: &'a LiveGraph,
    tasks: &'a Tasks,
    git: &'a git::GitTracker,
    reconciliation: (&'a HashMap<OpId, Op>, bool),
}

#[derive(Debug, Deserialize)]
pub(super) struct Saved {
    disclosure: super::disclosure::Disclosure,
    pub(super) version: u64,
    workspace: PathBuf,
    tail: IndexedTail,
    projection: LiveProjection,
    blocks: RankTree<Order, StoredBlock>,
    orders: Map<String, Order>,
    inputs: Map<String, LiveRow>,
    owners: Map<OpId, String>,
    ancestry: Ancestry,
    graph: LiveGraph,
    tasks: Tasks,
    git: git::Saved,
    reconciliation: (HashMap<OpId, Op>, bool),
}

impl LiveWorkspace {
    fn saved(&self) -> Borrowed<'_> {
        Borrowed {
            version: VERSION,
            disclosure: &self.disclosure,
            workspace: &self.root,
            tail: &self.tail,
            projection: &self.projection,
            blocks: &self.blocks,
            orders: &self.orders,
            inputs: &self.inputs,
            owners: &self.owners,
            ancestry: &self.ancestry,
            graph: &self.graph,
            tasks: &self.tasks,
            git: &self.git,
            reconciliation: self.reconciliation.checkpoint(),
        }
    }

    pub(super) fn adopt(&mut self, saved: Saved, validate: bool) -> Result<()> {
        if !(1..=VERSION).contains(&saved.version) || saved.workspace != self.root {
            return Err("live checkpoint schema/workspace changed; close History, remove the derived live-v1 directory and run prepare-view".into());
        }
        if validate {
            saved.tail.resume(&self.chain)?;
        }
        self.git.restore(saved.git)?;
        self.reconciliation.restore(saved.reconciliation);
        self.disclosure = saved.disclosure;
        self.tail = saved.tail;
        self.projection = saved.projection;
        self.blocks = saved.blocks;
        self.orders = saved.orders;
        self.inputs = saved.inputs;
        self.owners = saved.owners;
        self.ancestry = saved.ancestry;
        self.graph = saved.graph;
        self.tasks = saved.tasks;
        Ok(())
    }

    pub(super) fn checkpoint(&mut self) -> Result<()> {
        self.poisoned = true;
        self.rows.flush()?;
        if let Some(search) = &mut self.search {
            search.flush()?;
        }
        let storage = Rc::clone(&self.checkpoint_store);
        let saved = storage.commit(&self.saved())?;
        self.adopt(saved, false)?;
        self.poisoned = false;
        Ok(())
    }

    pub(super) fn unload(&mut self) -> Result<()> {
        // Capture can fail after the canonical frontier advanced. Keep those
        // unpublished pages and their pending projection delta together.
        if self.pending.work.bytes_read > 0
            || !self.pending.added.is_empty()
            || !self.pending.removed.is_empty()
        {
            return Ok(());
        }
        let storage = Rc::clone(&self.checkpoint_store);
        let saved = storage.unload(&self.saved())?;
        self.adopt(saved, false)
    }
}

pub(super) fn load(storage: &Rc<Storage>) -> Result<Option<Saved>> {
    match storage.load() {
        Ok(saved) => Ok(Some(saved)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;
