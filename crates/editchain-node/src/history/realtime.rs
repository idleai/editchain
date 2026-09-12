//! Resident native workspace: canonical tail, logical items, and keyed row blocks.

mod ancestry;
mod checkpoint;
mod collector;
mod disclosure;
mod git;
mod open;
mod queries;
mod regroup;
mod rows;
mod storage;
mod tasks;
use storage::{RowStore, StoredBlock};

use editchain_core::OpId;
use editchain_git::RepositoryCatalog;
use editchain_import::codex::live::LiveCodex;
use editchain_project::live::{LiveChanges, LiveProjection, LiveRow};
use editchain_protocol::{
    rank::{Measure, RankTree},
    LiveBaseline, LiveBlock, LiveDelta, LiveUpdate, LiveWork, OpenRequest, OpenResponse,
    SnapshotId, SyncLiveRequest, PROTOCOL_VERSION,
};
use editchain_store::{ChainDelta, IndexedTail};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::Instant,
};

use editchain_index::Map as HashMap;

type Order = editchain_protocol::LiveOrder;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// State is created once by `OpenLive`, then updated by canonical deltas.
#[derive(Debug)]
pub(crate) struct LiveWorkspace {
    reused_checkpoint: bool,
    coordinates: editchain_protocol::rank::Axis,
    preparing: bool,
    disclosure: disclosure::Disclosure,
    checkpoint_store: std::rc::Rc<editchain_index::Storage>,
    root: PathBuf,
    chain: PathBuf,
    tail: IndexedTail,
    blobs: editchain_store::BlobReader,
    pending: ChainDelta,
    poisoned: bool,
    projection: LiveProjection,
    catalog: RepositoryCatalog,
    git: git::GitTracker,
    codex: Option<(String, LiveCodex)>,
    blocks: RankTree<Order, StoredBlock>,
    orders: HashMap<String, Order>,
    inputs: HashMap<String, LiveRow>,
    owners: HashMap<OpId, String>,
    epoch: SnapshotId,
    snapshot_id: SnapshotId,
    revision: u64,
    journal: VecDeque<LiveDelta>,
    journal_bytes: usize,
    search: Option<queries::LiveSearch>,
    rows: RowStore,
    ancestry: ancestry::Ancestry,
    graph: editchain_protocol::live_graph::LiveGraph,
    reconciliation: crate::reconcile::LiveReconciliation,
    tasks: tasks::Tasks,
}

impl LiveWorkspace {
    pub(crate) fn open(request: &OpenRequest) -> Result<Self> {
        editchain_index::boundary(|| Self::open_inner(request, false))?
    }

    fn open_inner(request: &OpenRequest, prepare: bool) -> Result<Self> {
        let root = std::fs::canonicalize(&request.workspace_path)?;
        let chain = if Path::new(&request.chain_dir).is_absolute() {
            PathBuf::from(&request.chain_dir)
        } else {
            root.join(&request.chain_dir)
        };
        std::fs::create_dir_all(&chain)?;
        let chain = std::fs::canonicalize(chain)?;
        let checkpoint_path = chain.join("live-v1");
        let checkpoint_store = editchain_index::Storage::open(&checkpoint_path)?;
        let saved = checkpoint::load(&checkpoint_store)?;
        if !prepare
            && saved
                .as_ref()
                .is_some_and(|saved| saved.version < checkpoint::VERSION)
        {
            return Err("history graph checkpoint needs preparation; run editchain prepare-view --workspace <workspace> --chain <chain>".into());
        }

        let tail = if saved.is_some() {
            IndexedTail::empty(&chain)
        } else {
            IndexedTail::open(&chain)?
        };
        let catalog = RepositoryCatalog::discover(&root)?;
        let epoch = super::unique_snapshot_id("live");
        let git = git::GitTracker::new(&catalog)?;
        let reconciliation = crate::reconcile::LiveReconciliation::new(&catalog)?;
        let mut workspace = Self {
            reused_checkpoint: saved.is_some(),
            coordinates: editchain_protocol::rank::Axis::Expanded,
            preparing: true,
            disclosure: disclosure::Disclosure::default(),
            checkpoint_store,
            blobs: editchain_store::BlobReader::open(&chain)?,
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
            search: Some(queries::LiveSearch::open(
                &checkpoint_path.join("search"),
                saved.is_none(),
            )?),
            rows: RowStore::open(&checkpoint_path.join("rows"))?,
            ancestry: ancestry::Ancestry::default(),
            graph: editchain_protocol::live_graph::LiveGraph::default(),
            reconciliation,
            tasks: tasks::Tasks::default(),
        };
        if let Some(saved) = saved {
            let repair = saved.version < checkpoint::VERSION;
            let regroup = saved.version == 1;
            workspace.adopt(saved, true)?;
            if regroup {
                workspace.regroup();
            }
            if repair {
                workspace.repair_graph()?;
            }
            workspace.preparing = false;
            return Ok(workspace);
        }
        let initial: Vec<_> = workspace.tail.chain().shared_ops().collect();
        let blobs = editchain_import::FsBlobSink::open_read_only(workspace.chain.join("blobs"))?;
        for chunk in initial.chunks(1024) {
            let ops: Vec<_> = chunk.iter().map(|op| op.as_ref().clone()).collect();
            workspace
                .reconciliation
                .observe(&ops, std::iter::empty(), blobs.as_ref());
            workspace.ancestry.observe_links(&ops, &[]);
            workspace.git.follow_links(&ops);
        }
        let changes = workspace.projection.apply_shared(initial, &[]);
        let (removed, mut upserts) = workspace.apply_blocks(changes)?;
        let initial_git = workspace.git.poll()?;
        upserts.extend(workspace.apply_git(initial_git)?);
        drop(workspace.connect(&removed, upserts)?);
        workspace.preparing = false;
        workspace.checkpoint()?;
        Ok(workspace)
    }

    pub(crate) fn prepare(request: &OpenRequest) -> Result<Self> {
        let mut workspace = editchain_index::boundary(|| Self::open_inner(request, true))??;
        workspace.coordinates = editchain_protocol::rank::Axis::Visible;
        Ok(workspace)
    }

    pub(crate) fn open_paged(request: &OpenRequest) -> Result<Self> {
        let mut workspace = Self::open(request)?;
        workspace.coordinates = editchain_protocol::rank::Axis::Visible;
        Ok(workspace)
    }

    fn paged(&self) -> bool {
        matches!(self.coordinates, editchain_protocol::rank::Axis::Visible)
    }

    fn total(&self) -> u64 {
        let measure = self.blocks.measure();
        if self.paged() {
            measure.visible
        } else {
            measure.expanded
        }
    }

    fn open_metadata(&self) -> OpenResponse {
        OpenResponse {
            protocol_version: PROTOCOL_VERSION,
            live_updates: true,
            snapshot_id: self.snapshot_id.clone(),
            workspace: self.root.to_string_lossy().into_owned(),
            chain: self.chain.to_string_lossy().into_owned(),
            repos: self.catalog.len(),
            nodes: self.total(),
            chain_generation: u64::try_from(self.tail.chain().stats().accepted).unwrap_or(u64::MAX),
            render_snapshot: "retained-live".into(),
            diagnostics: serde_json::json!({ "chain": self.tail.chain().stats(), "checkpoint": self.reused_checkpoint, "open_chain_records": if self.reused_checkpoint { 0 } else { self.tail.chain().stats().records } }),
            warnings: Vec::new(),
            live: None,
        }
    }

    pub(crate) fn opened(&self) -> OpenResponse {
        let mut response = self.open_metadata();
        response.live = Some(LiveBaseline {
            paged: self.paged(),
            epoch: self.epoch.clone(),
            revision: self.revision,
            total: self.total(),
            blocks: if self.paged() {
                Vec::new()
            } else {
                self.blocks
                    .iter()
                    .map(|(_, block)| block.meta.clone())
                    .collect()
            },
        });
        response
    }

    pub(crate) fn sync(&mut self, request: &SyncLiveRequest) -> Result<LiveUpdate> {
        match editchain_index::boundary(|| {
            let before = self.revision;
            let result = self.sync_inner(request)?;
            if self.revision != before || result.work.chain_bytes > 0 {
                self.checkpoint()?;
            } else {
                self.unload()?;
            }
            Ok(result)
        }) {
            Ok(result) => result,
            Err(error) => {
                self.poisoned = true;
                Err(error.into())
            }
        }
    }

    fn sync_inner(&mut self, request: &SyncLiveRequest) -> Result<LiveUpdate> {
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
            .map(|(op, _)| op.as_ref().clone())
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
                    .map(|(op, _)| op.as_ref().clone())
                    .collect::<Vec<_>>(),
                &admitted.removed.iter().copied().collect::<Vec<_>>(),
            );
            let changes = self.projection.apply_shared(
                admitted.added.into_values().map(|(op, _)| op).collect(),
                &admitted.removed.into_iter().collect::<Vec<_>>(),
            );
            work.presentation_ops = changes.work.presentation_ops;
            work.items = changes.work.items;
            work.occurrences = changes.work.occurrences;
            let (removed, mut upserts) = self.apply_blocks(changes)?;
            upserts.extend(self.apply_git(commits)?);
            let (removed, stored) = self.connect(&removed, upserts)?;
            let upserts = stored
                .iter()
                .map(|block| self.load_block(block))
                .collect::<Result<Vec<_>>>()?;
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

    fn apply_blocks(&mut self, changes: LiveChanges) -> Result<(Vec<String>, Vec<StoredBlock>)> {
        self.blobs = editchain_store::BlobReader::open(&self.chain)?;
        self.tasks.observe(&changes, &self.projection);
        self.ancestry.observe_relationships(changes.relationships);
        let mut removed = Vec::new();
        let mut replacements = Vec::new();
        for key in changes.removed {
            if self.remove_block(&key) {
                removed.push(key);
            }
        }
        // A failed transaction poisons this epoch. Keep only one item's row
        // payload in memory while bootstrapping, rather than staging all rows.
        for (key, input) in changes.upserts {
            let block = self.present(&input)?;
            let previous = self
                .orders
                .get(&key)
                .and_then(|order| self.blocks.get(order));
            if let (Some(previous), Some(block)) = (previous, &block) {
                if previous.matches(block)? {
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
                if let Some(search) = &mut self.search {
                    search.put(&block)?;
                }
                let block = self.rows.put(block)?;
                let order = block.meta.order();
                drop(self.orders.insert(key, order.clone()));
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

    fn load_block(&self, block: &StoredBlock) -> Result<LiveBlock> {
        let mut rows = self.rows.rows(block)?;
        for (slot, row) in rows.iter_mut().enumerate() {
            self.graph
                .decorate(&block.meta.key, u64::try_from(slot)?, row);
            if slot == 0 {
                row.task_group.clone_from(&block.meta.task_summary);
            }
        }
        Ok(LiveBlock {
            meta: block.meta.clone(),
            rows,
        })
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
        if let Some(block) = self.blocks.remove(&order) {
            self.rows.remove(&block);
        }
        self.ancestry.remove(key);
        if let Some(search) = &mut self.search {
            search.remove(key);
        }
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
            visible_total: Some(self.blocks.measure().visible),
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
        upserts: Vec<StoredBlock>,
    ) -> Result<(Vec<String>, Vec<StoredBlock>)> {
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
        let metas = self.graph.causal_updates(&metas)?;
        for meta in &metas {
            if let Some(block) = changed.get_mut(&meta.key) {
                block.meta = meta.clone();
            } else if let Some(block) = self
                .orders
                .get(&meta.key)
                .and_then(|order| self.blocks.get(order))
            {
                let mut block = block.clone();
                block.meta = meta.clone();
                drop(changed.insert(meta.key.clone(), block));
            }
        }
        self.graph.edit(removed, &metas);
        let groups = self
            .tasks
            .update(removed, &metas, &self.inputs, &self.graph);
        for key in groups.removed {
            self.disclosure.forget_group(&key);
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
        for (key, summary) in groups.summaries {
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
                block.meta.task_summary = summary;
            }
        }
        for block in changed.values() {
            let order = block.meta.order();
            if let Some(old) = self.orders.insert(block.meta.key.clone(), order.clone()) {
                if old != order {
                    drop(self.blocks.remove(&old));
                }
            }
            drop(self.blocks.insert(
                order,
                block.clone(),
                Measure {
                    expanded: block.meta.row_count,
                    visible: block.meta.row_count,
                },
            ));
        }
        self.update_disclosure(removed, &changed.keys().cloned().collect::<Vec<_>>())?;
        Ok((removed.to_vec(), changed.into_values().collect()))
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
