//! Dependency-indexed current provider items and standalone operations.
//!
//! Live activity has one stable block per logical item. Durable occurrence
//! history remains immutable; the offline Activity view also retains its
//! historical work-group contractions. These are distinct presentation modes.

use crate::{materialization::selected_codex, provider::decode_evidence, CodexLogicalItem};
use editchain_core::provider::{CodexDerivationEvidence, CodexLogicalChange, ProviderFact};
use editchain_core::{Op, OpId, OpKind};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use editchain_index::{Map, OrderedMap, OrderedSet};

mod neighbors;
use neighbors::Neighbors;
/// Incremental provider spawn and completion relationships.
pub mod topology;

type Stream = (u64, u32);
type Turn = (Stream, String);
type Item = (Turn, String);

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Coverage {
    raw: OrderedSet<u64>,
    ready: OrderedSet<u64>,
}

impl Coverage {
    fn complete(&self) -> bool {
        self.raw.len() == self.ready.len()
            && self.raw.last().copied().unwrap_or(0)
                == u64::try_from(self.raw.len()).unwrap_or(u64::MAX)
    }
}

/// Content inputs for one independent, stable presentation block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveRow {
    /// Stable presentation identity; physical operation IDs remain separate.
    pub key: String,
    /// Raw occurrence or standalone operation owning this block.
    pub anchor: OpId,
    /// First occurrence of the current logical incarnation.
    pub incarnation: OpId,
    /// Only the operations required to present this block.
    pub operations: Vec<Arc<Op>>,
    /// Native task membership, independent of a row's presentation scope.
    pub task: Option<TaskIdentity>,
}

/// A provider task incarnation. Rollback/reuse cannot inherit old disclosure.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskIdentity {
    /// Stable opaque identity within one captured source generation.
    pub key: String,
    /// Full provider thread identity.
    pub thread: String,
    /// Full native task/turn identity.
    pub turn: String,
    /// Most recent removal boundary, or sequence zero for the original task.
    pub boundary: OpId,
}

/// Work counters used to detect accidental global rebuilds.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct LiveProjectWork {
    /// Incoming canonical additions and retractions.
    pub admissions: usize,
    /// Occurrences revalidated through reverse dependencies.
    pub occurrences: usize,
    /// Logical items whose latest revision was recomputed.
    pub items: usize,
    /// Operations copied into changed presentation blocks.
    pub presentation_ops: usize,
}

/// Keyed changes; a key appears in at most one of the two lanes.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct LiveChanges {
    /// Exact structural changes, including changes without a new content row.
    pub relationships: topology::RelationChanges,
    /// Added or revised logical blocks.
    pub upserts: BTreeMap<String, LiveRow>,
    /// Retired block identities.
    pub removed: BTreeSet<String>,
    /// Work performed for this delta only.
    pub work: LiveProjectWork,
}

/// Retained current-item projection over canonically admitted operations.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct LiveProjection {
    #[serde(default)]
    topology: topology::Topology,
    ops: Map<OpId, Arc<Op>>,
    owners: Map<OpId, Option<OpId>>,
    children: Map<OpId, Neighbors>,
    facts: Map<OpId, Neighbors>,
    dependents: Map<OpId, Neighbors>,
    selected: Map<OpId, CodexDerivationEvidence>,
    coverage: Map<Stream, Coverage>,
    items: Map<Item, OrderedMap<OpId, CodexLogicalItem>>,
    turns: Map<Turn, OrderedSet<Item>>,
    source_items: Map<Stream, OrderedSet<Item>>,
    removals: Map<Turn, OrderedSet<OpId>>,
    published: Map<Item, String>,
}

impl LiveProjection {
    /// Apply admitted additions/retractions. Exact duplicates are filtered by
    /// canonical admission before this API. Only dependency closures are read.
    pub fn apply(&mut self, added: Vec<Op>, removed: &[OpId]) -> LiveChanges {
        self.apply_shared(added.into_iter().map(Arc::new).collect(), removed)
    }

    /// Apply shared immutable operations from a retained canonical reader.
    pub fn apply_shared(&mut self, added: Vec<Arc<Op>>, removed: &[OpId]) -> LiveChanges {
        let changed: HashSet<_> = added
            .iter()
            .map(|op| op.id)
            .chain(removed.iter().copied())
            .collect();
        let mut result = LiveChanges::default();
        for id in &changed {
            if let Some(op) = self.ops.get(id) {
                self.topology.observe(op, false);
            }
        }
        for op in &added {
            self.topology.observe(op, true);
        }
        result.work.admissions = changed.len();
        let mut sources = HashSet::new();
        let mut streams = HashMap::new();
        for id in &changed {
            sources.extend(self.dependents.get(id).into_iter().flatten().copied());
            sources.extend(self.raw_owner(*id));
            if let Some(op) = self.ops.get(id) {
                if matches!(op.kind, OpKind::Import(_)) {
                    let key = stream(*id);
                    let _: &mut bool = streams
                        .entry(key)
                        .or_insert_with(|| self.coverage.get(&key).is_none_or(Coverage::complete));
                }
            }
        }
        for id in removed {
            self.remove_op(*id);
        }
        for op in added {
            self.index_op(op, &mut sources, &mut streams);
        }
        let descendants = self.changed_descendants(&changed);
        for id in changed.iter().chain(&descendants) {
            let _: Option<Option<OpId>> = self.owners.remove(id);
        }
        for id in changed.iter().chain(&descendants) {
            self.index_owner(*id);
        }
        for id in changed.iter().chain(&descendants) {
            sources.extend(self.raw_owner(*id));
        }
        let mut items = HashSet::new();
        for source in &sources {
            let key = stream(*source);
            let _: &mut bool = streams
                .entry(key)
                .or_insert_with(|| self.coverage.get(&key).is_none_or(Coverage::complete));
            self.reduce_occurrence(*source, &mut items);
        }
        for (source, before) in streams {
            if before != self.coverage.get(&source).is_none_or(Coverage::complete) {
                items.extend(
                    self.source_items
                        .get(&source)
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
        }
        result.work.occurrences = sources.len();
        result.work.items = items.len();
        for item in items {
            self.publish_item(&item, &mut result);
        }
        for source in sources {
            self.publish_record(source, &mut result);
        }
        for id in changed.into_iter().chain(descendants) {
            self.publish_standalone(id, &mut result);
        }
        result.work.presentation_ops = result
            .upserts
            .values()
            .map(|row| row.operations.len())
            .sum();
        result.relationships = self.topology.resolve(&self.ops);
        result
    }

    /// Prepare missing derived topology from retained operations, without replaying materialization.
    pub fn prepare_relationships(&mut self) -> topology::RelationChanges {
        self.topology = topology::Topology::default();
        for op in self.ops.values() {
            self.topology.observe(op, true);
        }
        self.topology.resolve(&self.ops)
    }

    /// Accepted immutable source lookup, also used by detail adapters.
    #[must_use]
    pub fn operation(&self, id: OpId) -> Option<&Op> {
        self.ops.get(&id).map(AsRef::as_ref)
    }

    /// Complete accepted occurrence proof, for adapters reading persisted metadata.
    #[must_use]
    pub fn codex_derivation(&self, source: OpId) -> Option<&CodexDerivationEvidence> {
        self.selected.get(&source)
    }

    /// Current presentation owners of an exact historical item occurrence.
    /// References to old results retain their item identity without making
    /// those results a second chronological appearance of the same item.
    #[must_use]
    pub fn item_owners(&self, id: OpId) -> Vec<String> {
        let source = self.raw_owner(id).unwrap_or(id);
        self.selected
            .get(&source)
            .into_iter()
            .flat_map(|meta| &meta.changes)
            .filter_map(|change| {
                let CodexLogicalChange::Upsert {
                    turn,
                    item,
                    incarnation,
                    outputs,
                } = change
                else {
                    return None;
                };
                if id != source && !outputs.contains(&id) {
                    return None;
                }
                let key = ((stream(source), turn.clone()), item.clone());
                (self.current(&key)?.incarnation == *incarnation)
                    .then(|| self.published.get(&key).cloned())
                    .flatten()
            })
            .collect()
    }

    /// Complete current item state, for offline replay comparison and export.
    #[must_use]
    pub fn current_items(&self) -> Vec<CodexLogicalItem> {
        self.items
            .keys()
            .filter_map(|key| self.current(key).cloned())
            .collect()
    }

    fn current(&self, key: &Item) -> Option<&CodexLogicalItem> {
        if !self.coverage.get(&key.0 .0).is_some_and(Coverage::complete) {
            return None;
        }
        let (source, item) = self.items.get(key)?.last_key_value()?;
        let removed = self.removals.get(&key.0).and_then(OrderedSet::last);
        (removed.is_none_or(|removed| source > removed)).then_some(item)
    }

    fn remove_op(&mut self, id: OpId) {
        if let Some(op) = self.ops.remove(&id) {
            if matches!(op.kind, OpKind::Import(_)) {
                if let Some(coverage) = self.coverage.get_mut(&stream(id)) {
                    let _: bool = coverage.raw.remove(&(id.seq >> 16));
                }
            }
        }
    }

    fn index_op(
        &mut self,
        op: Arc<Op>,
        sources: &mut HashSet<OpId>,
        streams: &mut HashMap<Stream, bool>,
    ) {
        let key = stream(op.id);
        for parent in &op.parents {
            let _: bool = self.children.entry(*parent).or_default().insert(op.id);
        }
        if matches!(op.kind, OpKind::Import(_)) {
            let _: &mut bool = streams
                .entry(key)
                .or_insert_with(|| self.coverage.get(&key).is_none_or(Coverage::complete));
            let _: bool = self
                .coverage
                .entry(key)
                .or_default()
                .raw
                .insert(op.id.seq >> 16);
            let _: bool = sources.insert(op.id);
        }
        if let Some(record) = decode_evidence(&op) {
            if let ProviderFact::CodexDerivation(meta) = record.payload.fact {
                let source = record.payload.source;
                let _: bool = self.facts.entry(source).or_default().insert(op.id);
                let mut dependencies = meta.outputs;
                dependencies.extend([source, op.id]);
                for change in meta.changes {
                    if let CodexLogicalChange::Upsert { incarnation, .. } = change {
                        dependencies.push(incarnation);
                    }
                }
                for dependency in dependencies {
                    let _: bool = self
                        .dependents
                        .entry(dependency)
                        .or_default()
                        .insert(source);
                }
                let _: bool = sources.insert(source);
            }
        }
        drop(self.ops.insert(op.id, op));
    }

    fn raw_owner(&self, id: OpId) -> Option<OpId> {
        self.owners.get(&id).copied().flatten()
    }

    fn index_owner(&mut self, mut id: OpId) {
        let mut seen = HashSet::new();
        let owner = loop {
            if let Some(owner) = self.owners.get(&id) {
                break *owner;
            }
            if !seen.insert(id) {
                break None;
            }
            let Some(op) = self.ops.get(&id) else {
                break None;
            };
            if matches!(op.kind, OpKind::Import(_)) {
                break Some(id);
            }
            if let Some(evidence) = decode_evidence(op) {
                break Some(evidence.payload.source);
            }
            let Some(parent) = op.parents.iter().next() else {
                break None;
            };
            id = *parent;
        };
        for id in seen {
            let _: Option<Option<OpId>> = self.owners.insert(id, owner);
        }
    }

    fn changed_descendants(&self, changed: &HashSet<OpId>) -> HashSet<OpId> {
        let mut visited = HashSet::new();
        let mut pending: Vec<_> = changed.iter().copied().collect();
        while let Some(parent) = pending.pop() {
            for child in self.children.get(&parent).into_iter().flatten() {
                if self
                    .ops
                    .get(child)
                    .is_some_and(|op| matches!(op.kind, OpKind::Import(_)))
                {
                    continue;
                }
                if visited.insert(*child) {
                    pending.push(*child);
                }
            }
        }
        visited
    }

    fn reduce_occurrence(&mut self, source: OpId, affected: &mut HashSet<Item>) {
        let next = selected_codex(
            source,
            self.facts
                .get(&source)
                .into_iter()
                .flatten()
                .filter_map(|id| self.ops.get(id).map(AsRef::as_ref)),
            &self.ops,
        );
        if next.as_ref() == self.selected.get(&source) {
            return;
        }
        if let Some(previous) = self.selected.remove(&source) {
            self.unapply(source, &previous, affected);
        }
        let coverage = self.coverage.entry(stream(source)).or_default();
        let _: bool = coverage.ready.remove(&(source.seq >> 16));
        if let Some(meta) = next {
            let _: bool = coverage.ready.insert(source.seq >> 16);
            self.apply_changes(source, &meta, affected);
            drop(self.selected.insert(source, meta));
        }
    }

    fn unapply(
        &mut self,
        source: OpId,
        meta: &CodexDerivationEvidence,
        affected: &mut HashSet<Item>,
    ) {
        for change in &meta.changes {
            match change {
                CodexLogicalChange::Upsert { turn, item, .. } => {
                    let key = ((stream(source), turn.clone()), item.clone());
                    if let Some(revisions) = self.items.get_mut(&key) {
                        drop(revisions.remove(&source));
                    }
                    let _: bool = affected.insert(key);
                }
                CodexLogicalChange::RemoveTurn { turn } => {
                    let key = (stream(source), turn.clone());
                    if let Some(removals) = self.removals.get_mut(&key) {
                        let _: bool = removals.remove(&source);
                    }
                    affected.extend(self.turns.get(&key).into_iter().flatten().cloned());
                }
            }
        }
    }

    fn apply_changes(
        &mut self,
        source: OpId,
        meta: &CodexDerivationEvidence,
        affected: &mut HashSet<Item>,
    ) {
        for change in &meta.changes {
            match change {
                CodexLogicalChange::Upsert {
                    turn,
                    item,
                    incarnation,
                    outputs,
                } => {
                    let key = ((stream(source), turn.clone()), item.clone());
                    let current = CodexLogicalItem {
                        thread: meta.thread.clone(),
                        turn: turn.clone(),
                        item: item.clone(),
                        incarnation: *incarnation,
                        source,
                        outputs: outputs.clone(),
                    };
                    drop(
                        self.items
                            .entry(key.clone())
                            .or_default()
                            .insert(source, current),
                    );
                    let _: bool = self
                        .turns
                        .entry(key.0.clone())
                        .or_default()
                        .insert(key.clone());
                    let _: bool = self
                        .source_items
                        .entry(stream(source))
                        .or_default()
                        .insert(key.clone());
                    let _: bool = affected.insert(key);
                }
                CodexLogicalChange::RemoveTurn { turn } => {
                    let key = (stream(source), turn.clone());
                    let _: bool = self.removals.entry(key.clone()).or_default().insert(source);
                    affected.extend(self.turns.get(&key).into_iter().flatten().cloned());
                }
            }
        }
    }

    fn publish_item(&mut self, key: &Item, output: &mut LiveChanges) {
        if let Some(previous) = self.published.remove(key) {
            retire(previous, output);
        }
        let Some(item) = self.current(key) else {
            return;
        };
        let identity = format!(
            "item:{}:{}:{}:{}:{}",
            item.incarnation,
            item.turn.len(),
            item.turn,
            item.item.len(),
            item.item
        );
        let mut row = self.row(
            identity.clone(),
            item.source,
            item.incarnation,
            &item.outputs,
        );
        let boundary = self
            .removals
            .get(&key.0)
            .and_then(OrderedSet::last)
            .copied()
            .unwrap_or(OpId {
                seq: 0,
                ..item.source
            });
        row.task = Some(TaskIdentity {
            key: format!(
                "codex:{boundary}:{}:{}:{}:{}",
                item.thread.0.len(),
                item.thread.0,
                item.turn.len(),
                item.turn
            ),
            thread: item.thread.0.clone(),
            turn: item.turn.clone(),
            boundary,
        });
        put(row, output);
        drop(self.published.insert(key.clone(), identity));
    }

    fn row(&self, key: String, anchor: OpId, incarnation: OpId, outputs: &[OpId]) -> LiveRow {
        let operations = std::iter::once(&anchor)
            .chain(outputs)
            .filter_map(|id| self.ops.get(id).cloned())
            .collect();
        LiveRow {
            key,
            anchor,
            incarnation,
            operations,
            task: None,
        }
    }

    fn publish_record(&self, source: OpId, output: &mut LiveChanges) {
        let key = format!("record:{source}");
        retire(key.clone(), output);
        if !self
            .ops
            .get(&source)
            .is_some_and(|op| matches!(op.kind, OpKind::Import(_)))
        {
            return;
        }
        if let Some(meta) = self.selected.get(&source) {
            let owned: HashSet<_> = meta
                .changes
                .iter()
                .flat_map(|change| match change {
                    CodexLogicalChange::Upsert { outputs, .. } => outputs.as_slice(),
                    CodexLogicalChange::RemoveTurn { .. } => &[],
                })
                .copied()
                .collect();
            let extra: Vec<_> = meta
                .outputs
                .iter()
                .filter(|id| !owned.contains(id))
                .copied()
                .collect();
            if !extra.is_empty() {
                put(self.row(key, source, source, &extra), output);
            }
        } else if !self.facts.contains_key(&source) {
            let mut children: Vec<_> = self
                .children
                .get(&source)
                .into_iter()
                .flatten()
                .filter(|id| {
                    self.ops.get(id).is_some_and(|op| {
                        !matches!(op.kind, OpKind::Import(_)) && decode_evidence(op).is_none()
                    })
                })
                .copied()
                .collect();
            children.sort_unstable();
            put(self.row(key, source, source, &children), output);
        }
    }

    fn publish_standalone(&self, id: OpId, output: &mut LiveChanges) {
        let key = format!("op:{id}");
        if self.raw_owner(id).is_some() {
            retire(key, output);
            return;
        }
        let Some(op) = self.ops.get(&id) else {
            retire(key, output);
            return;
        };
        if decode_evidence(op).is_some()
            || matches!(op.kind, OpKind::GitLink(_) | OpKind::GitCommit(_))
        {
            retire(key, output);
            return;
        }
        put(self.row(key, id, id, &[]), output);
    }
}

fn stream(id: OpId) -> Stream {
    (id.node.0, id.boot)
}

fn retire(key: String, output: &mut LiveChanges) {
    drop(output.upserts.remove(&key));
    let _: bool = output.removed.insert(key);
}

fn put(row: LiveRow, output: &mut LiveChanges) {
    let _: bool = output.removed.remove(&row.key);
    drop(output.upserts.insert(row.key.clone(), row));
}
