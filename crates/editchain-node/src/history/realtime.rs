//! Resident native workspace: canonical tail, logical items, and keyed row blocks.

mod ancestry;
mod collector;
mod git;
mod queries;
mod rows;
mod tasks;

use editchain_core::OpId;
use editchain_git::RepositoryCatalog;
use editchain_import::codex::live::LiveCodex;
use editchain_project::live::{LiveChanges, LiveProjection, LiveRow};
use editchain_protocol::{
    rank::{Measure, RankTree},
    LiveBaseline, LiveBlock, LiveDelta, LiveUpdate, LiveWork, OpenRequest, OpenResponse,
    SnapshotId, SyncLiveRequest, PROTOCOL_VERSION,
};
use editchain_store::{CanonicalTail, ChainDelta};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    time::Instant,
};

type Order = editchain_protocol::LiveOrder;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// State is created once by `OpenLive`, then updated by canonical deltas.
#[derive(Debug)]
pub(crate) struct LiveWorkspace {
    root: PathBuf,
    chain: PathBuf,
    tail: CanonicalTail,
    pending: ChainDelta,
    poisoned: bool,
    projection: LiveProjection,
    catalog: RepositoryCatalog,
    git: git::GitTracker,
    codex: Option<(String, LiveCodex)>,
    blocks: RankTree<Order, LiveBlock>,
    orders: HashMap<String, Order>,
    inputs: HashMap<String, LiveRow>,
    owners: HashMap<OpId, String>,
    epoch: SnapshotId,
    snapshot_id: SnapshotId,
    revision: u64,
    journal: VecDeque<LiveDelta>,
    journal_bytes: usize,
    search: queries::LiveSearch,
    ancestry: ancestry::Ancestry,
    graph: editchain_protocol::live_graph::LiveGraph,
    reconciliation: crate::reconcile::LiveReconciliation,
    tasks: tasks::Tasks,
}

impl LiveWorkspace {
    pub(crate) fn open(request: &OpenRequest) -> Result<Self> {
        let root = PathBuf::from(&request.workspace_path);
        let chain = if Path::new(&request.chain_dir).is_absolute() {
            PathBuf::from(&request.chain_dir)
        } else {
            root.join(&request.chain_dir)
        };
        let tail = CanonicalTail::open(&chain)?;
        let catalog = RepositoryCatalog::discover(&root)?;
        let epoch = super::unique_snapshot_id("live");
        let git = git::GitTracker::new(&catalog)?;
        let reconciliation = crate::reconcile::LiveReconciliation::new(&catalog)?;
        let mut workspace = Self {
            root,
            chain,
            tail,
            pending: ChainDelta::default(),
            poisoned: false,
            catalog,
            git,
            projection: LiveProjection::default(),
            codex: None,
            blocks: RankTree::default(),
            orders: HashMap::new(),
            inputs: HashMap::new(),
            owners: HashMap::new(),
            snapshot_id: epoch.clone(),
            epoch,
            revision: 0,
            journal: VecDeque::new(),
            journal_bytes: 0,
            search: queries::LiveSearch::new()?,
            ancestry: ancestry::Ancestry::default(),
            graph: editchain_protocol::live_graph::LiveGraph::default(),
            reconciliation,
            tasks: tasks::Tasks::default(),
        };
        let initial: Vec<_> = workspace
            .tail
            .chain()
            .located_ops()
            .map(|(op, _)| op.clone())
            .collect();
        let blobs = editchain_import::FsBlobSink::open_read_only(workspace.chain.join("blobs"))?;
        workspace
            .reconciliation
            .observe(&initial, std::iter::empty(), blobs.as_ref());
        workspace.ancestry.observe_links(&initial, &[]);
        workspace.git.follow_links(&initial);
        let changes = workspace.projection.apply(initial, &[]);
        let (removed, mut upserts) = workspace.apply_blocks(changes)?;
        let initial_git = workspace.git.poll()?;
        upserts.extend(workspace.apply_git(initial_git)?);
        let _changed = workspace.connect(&removed, upserts)?;
        Ok(workspace)
    }

    pub(crate) fn opened(&self) -> OpenResponse {
        OpenResponse {
            protocol_version: PROTOCOL_VERSION,
            live_updates: true,
            snapshot_id: self.snapshot_id.clone(),
            workspace: self.root.to_string_lossy().into_owned(),
            chain: self.chain.to_string_lossy().into_owned(),
            repos: self.catalog.len(),
            nodes: self.blocks.measure().expanded,
            chain_generation: u64::try_from(self.tail.chain().stats().accepted).unwrap_or(u64::MAX),
            render_snapshot: "retained-live".into(),
            diagnostics: serde_json::json!({ "chain": self.tail.chain().stats() }),
            warnings: Vec::new(),
            live: Some(LiveBaseline {
                epoch: self.epoch.clone(),
                revision: self.revision,
                total: self.blocks.measure().expanded,
                blocks: self
                    .blocks
                    .iter()
                    .map(|(_, block)| block.meta.clone())
                    .collect(),
            }),
        }
    }

    pub(crate) fn sync(&mut self, request: &SyncLiveRequest) -> Result<LiveUpdate> {
        self.validate_cursor(request)?;
        let capture_start = Instant::now();
        self.queue_tail()?;
        let mut work = LiveWork::default();
        if let Some(codex) = &request.codex {
            if let Err(error) = self.capture(codex, &mut work) {
                self.codex = None;
                return Err(error);
            }
        }
        self.queue_tail()?;
        // Git mutates its retained frontier. Any failure after this point needs
        // an explicit bootstrap, while pre-capture failures retain pending ops.
        self.poisoned = true;
        let new_ops: Vec<_> = self
            .pending
            .added
            .values()
            .map(|(op, _)| op.clone())
            .collect();
        let blobs = editchain_import::FsBlobSink::open_read_only(self.chain.join("blobs"))?;
        self.reconciliation.observe(
            &new_ops,
            self.pending.removed.iter().copied(),
            blobs.as_ref(),
        );
        self.git.follow_links(&new_ops);
        let mut commits = self.git.poll()?;
        let links = self
            .reconciliation
            .resolve(!commits.is_empty(), blobs.as_ref());
        if !links.is_empty() {
            self.append_links(&links)?;
            self.queue_tail()?;
            self.git.follow_links(&links);
            commits.extend(self.git.poll()?);
        }
        let admitted = std::mem::take(&mut self.pending);
        work.capture_ms = millis(capture_start.elapsed());
        work.chain_bytes = admitted.work.bytes_read;
        work.chain_records = admitted.work.records_decoded;
        let projection_start = Instant::now();
        if !admitted.added.is_empty() || !admitted.removed.is_empty() || !commits.is_empty() {
            self.poisoned = true;
            self.ancestry
                .invalidate(admitted.added.keys().chain(&admitted.removed).copied());
            self.ancestry.observe_links(
                &admitted
                    .added
                    .values()
                    .map(|(op, _)| op.clone())
                    .collect::<Vec<_>>(),
                &admitted.removed.iter().copied().collect::<Vec<_>>(),
            );
            let changes = self.projection.apply(
                admitted.added.into_values().map(|(op, _)| op).collect(),
                &admitted.removed.into_iter().collect::<Vec<_>>(),
            );
            work.presentation_ops = changes.work.presentation_ops;
            work.items = changes.work.items;
            work.occurrences = changes.work.occurrences;
            let (removed, mut upserts) = self.apply_blocks(changes)?;
            upserts.extend(self.apply_git(commits)?);
            let (removed, upserts) = self.connect(&removed, upserts)?;
            work.blocks = removed.len().saturating_add(upserts.len());
            work.projection_ms = millis(projection_start.elapsed());
            self.publish(removed, upserts, work)?;
        }
        self.poisoned = false;
        Ok(LiveUpdate {
            epoch: self.epoch.clone(),
            revision: self.revision,
            deltas: self
                .journal
                .iter()
                .filter(|delta| delta.revision > request.after_revision)
                .cloned()
                .collect(),
            work,
        })
    }

    fn validate_cursor(&self, request: &SyncLiveRequest) -> Result<()> {
        let first = self
            .journal
            .front()
            .map_or(self.revision, |delta| delta.base_revision);
        if self.poisoned
            || request.epoch != self.epoch
            || request.after_revision > self.revision
            || request.after_revision < first
        {
            return Err(super::stale_snapshot().into());
        }
        Ok(())
    }

    fn apply_blocks(&mut self, changes: LiveChanges) -> Result<(Vec<String>, Vec<LiveBlock>)> {
        self.tasks.observe(&changes, &self.projection);
        // Prepare every changed block before publishing any mutation.
        let upserts = changes
            .upserts
            .values()
            .map(|row| self.present(row).map(|block| (row.clone(), block)))
            .collect::<Result<Vec<_>>>()?;
        let mut removed = Vec::new();
        let mut replacements = Vec::new();
        for key in changes.removed {
            if self.remove_block(&key) {
                removed.push(key);
            }
        }
        for (input, block) in upserts {
            let key = input.key.clone();
            let previous = self
                .orders
                .get(&key)
                .and_then(|order| self.blocks.get(order));
            if let (Some(previous), Some(block)) = (previous, &block) {
                if serde_json::to_vec(previous)? == serde_json::to_vec(block)? {
                    continue;
                }
            }
            let existed = self.remove_block(&key);
            if let Some(block) = block {
                self.ancestry.put(&input, &self.projection);
                for op in &input.operations {
                    drop(self.owners.insert(op.id, key.clone()));
                }
                drop(self.inputs.insert(key.clone(), input));
                let order = block.meta.order();
                drop(self.orders.insert(key, order.clone()));
                self.search.put(&block)?;
                drop(self.blocks.insert(
                    order,
                    block.clone(),
                    Measure {
                        expanded: block.meta.row_count,
                        visible: block.meta.row_count,
                    },
                ));
                replacements.push(block);
            } else if existed {
                removed.push(key);
            }
        }
        Ok((removed, replacements))
    }

    fn queue_tail(&mut self) -> Result<()> {
        match self.tail.drain() {
            Ok(delta) => {
                merge(&mut self.pending, delta);
                Ok(())
            }
            Err(error) => {
                self.poisoned = true;
                Err(error.into())
            }
        }
    }

    fn remove_block(&mut self, key: &str) -> bool {
        let Some(order) = self.orders.remove(key) else {
            return false;
        };
        drop(self.blocks.remove(&order));
        self.ancestry.remove(key);
        self.search.remove(key);
        if let Some(input) = self.inputs.remove(key) {
            for op in input.operations {
                if self.owners.get(&op.id).is_some_and(|owner| owner == key) {
                    drop(self.owners.remove(&op.id));
                }
            }
        }
        true
    }

    fn publish(
        &mut self,
        removed: Vec<String>,
        upserts: Vec<LiveBlock>,
        work: LiveWork,
    ) -> Result<()> {
        let base_revision = self.revision;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("live revision exhausted")?;
        self.snapshot_id = SnapshotId::new(format!("{}:{}", self.epoch.as_str(), self.revision));
        let delta = LiveDelta {
            base_revision,
            revision: self.revision,
            snapshot_id: self.snapshot_id.clone(),
            removed,
            upserts,
            total: self.blocks.measure().expanded,
            chain_generation: u64::try_from(self.tail.chain().stats().accepted)?,
            max_lane: self.graph.max_lane(),
            work,
        };
        self.journal_bytes = self
            .journal_bytes
            .saturating_add(serde_json::to_vec(&delta)?.len());
        self.journal.push_back(delta);
        while self.journal.len() > 32
            || (self.journal.len() > 1 && self.journal_bytes > 16 * 1024 * 1024)
        {
            if let Some(expired) = self.journal.pop_front() {
                self.journal_bytes = self
                    .journal_bytes
                    .saturating_sub(serde_json::to_vec(&expired)?.len());
            }
        }
        Ok(())
    }

    fn connect(
        &mut self,
        removed: &[String],
        upserts: Vec<LiveBlock>,
    ) -> Result<(Vec<String>, Vec<LiveBlock>)> {
        let mut changed: std::collections::BTreeMap<_, _> = upserts
            .into_iter()
            .map(|block| (block.meta.key.clone(), block))
            .collect();
        for (key, mut parents) in self.ancestry.changed(&self.projection) {
            parents.extend(self.git.parent_keys(&key));
            parents.sort();
            parents.dedup();
            let Some(order) = self.orders.get(&key) else {
                continue;
            };
            let Some(block) = self.blocks.get(order) else {
                continue;
            };
            if block.meta.parents == parents {
                continue;
            }
            let mut block = block.clone();
            block.meta.parents = parents;
            drop(changed.insert(key, block));
        }
        let metas = changed
            .values()
            .map(|block| block.meta.clone())
            .collect::<Vec<_>>();
        self.graph.edit(removed, &metas);
        let groups = self.tasks.update(removed, &metas, &self.inputs);
        let mut removed = removed.to_vec();
        for key in groups.removed {
            if self.remove_block(&key) {
                removed.push(key);
            }
        }
        for (key, group) in groups.membership {
            if !changed.contains_key(&key) {
                if let Some(block) = self
                    .orders
                    .get(&key)
                    .and_then(|order| self.blocks.get(order))
                {
                    drop(changed.insert(key.clone(), block.clone()));
                }
            }
            if let Some(block) = changed.get_mut(&key) {
                block.meta.task_group = group;
            }
        }
        self.graph.set_headers(&removed, &groups.headers);
        for meta in groups.headers {
            let block = self.task_header(meta, &changed)?;
            if let Some(previous) = self
                .orders
                .insert(block.meta.key.clone(), block.meta.order())
            {
                drop(self.blocks.remove(&previous));
            }
            drop(changed.insert(block.meta.key.clone(), block));
        }
        for block in changed.values_mut() {
            for (slot, row) in block.rows.iter_mut().enumerate() {
                self.graph
                    .decorate(&block.meta.key, u64::try_from(slot)?, row);
            }
            drop(self.blocks.insert(
                block.meta.order(),
                block.clone(),
                Measure {
                    expanded: block.meta.row_count,
                    visible: block.meta.row_count,
                },
            ));
        }
        Ok((removed, changed.into_values().collect()))
    }
}

fn millis(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn merge(into: &mut ChainDelta, next: ChainDelta) {
    into.work.bytes_read = into.work.bytes_read.saturating_add(next.work.bytes_read);
    into.work.records_decoded = into
        .work
        .records_decoded
        .saturating_add(next.work.records_decoded);
    into.work.undecodable = into.work.undecodable.saturating_add(next.work.undecodable);
    into.added.extend(next.added);
    for id in next.removed {
        drop(into.added.remove(&id));
        let _: bool = into.removed.insert(id);
    }
}
