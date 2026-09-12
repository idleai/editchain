//! Indexed inputs to the established provider relationship resolver. Ordinary
//! appends read only prefix counters and lifecycle evidence for affected threads.

use super::Stream;
use crate::provider::{decode_evidence, resolve_records, RawCorpus};
use editchain_core::provider::{
    CodexLifecycleEvent, CodexSourceEvidence, CodexThreadId, ProviderFact,
};
use editchain_core::{NoteRelationship, Op, OpId, OpKind, ScopeRef};
use editchain_index::{Map, OrderedSet};
use std::collections::BTreeSet;
use std::sync::Arc;
#[cfg(test)]
mod tests;

/// An exact structural edge, before lifting its endpoints to current items.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct RelationEdge {
    /// Child session root or parent completion occurrence.
    pub anchor: OpId,
    /// Exact spawn occurrence or completed child terminal.
    pub target: OpId,
    /// Spawn ancestry replaces inherited session-start Git provenance.
    pub spawn: bool,
}

/// Relationship changes are independent of content changes.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RelationChanges {
    /// Newly resolved edges.
    pub added: Vec<RelationEdge>,
    /// Edges whose evidence or exact endpoint ceased to resolve.
    pub removed: Vec<RelationEdge>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Prefix {
    sequences: OrderedSet<u64>,
    scopes: Vec<(ScopeRef, usize)>,
    irregular: usize,
}

impl Prefix {
    fn edit(&mut self, op: &Op, added: bool) {
        if added {
            let _: bool = self.sequences.insert(op.id.seq);
            if let Some((_, count)) = self.scopes.iter_mut().find(|(scope, _)| *scope == op.scope) {
                *count = count.saturating_add(1);
            } else {
                self.scopes.push((op.scope, 1));
            }
            self.irregular = self
                .irregular
                .saturating_add(usize::from(op.id.seq.trailing_zeros() < 16));
        } else {
            let _: bool = self.sequences.remove(&op.id.seq);
            if let Some((_, count)) = self.scopes.iter_mut().find(|(scope, _)| *scope == op.scope) {
                *count = count.saturating_sub(1);
            }
            self.scopes.retain(|(_, count)| *count > 0);
            self.irregular = self
                .irregular
                .saturating_sub(usize::from(op.id.seq.trailing_zeros() < 16));
        }
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Topology {
    raw: Map<Stream, Prefix>,
    facts: Map<OpId, Arc<Op>>,
    prefixes: Map<Stream, OrderedSet<(u64, OpId)>>,
    observations: Map<CodexThreadId, BTreeSet<OpId>>,
    threads: Map<CodexThreadId, BTreeSet<Stream>>,
    source_threads: Map<Stream, BTreeSet<CodexThreadId>>,
    targets: Map<CodexThreadId, BTreeSet<CodexThreadId>>,
    watchers: Map<CodexThreadId, BTreeSet<CodexThreadId>>,
    published: Map<CodexThreadId, BTreeSet<RelationEdge>>,
    #[serde(skip)]
    pending: BTreeSet<CodexThreadId>,
}

impl Topology {
    pub(super) fn observe(&mut self, op: &Arc<Op>, added: bool) {
        let stream = super::stream(op.id);
        if matches!(op.kind, OpKind::Import(_)) {
            self.raw.entry(stream).or_default().edit(op, added);
        }
        if let Some(record) = decode_evidence(op) {
            match record.payload.fact {
                ProviderFact::CodexSource(meta) => {
                    let source = super::stream(record.payload.source);
                    let prefixes = self.prefixes.entry(source).or_default();
                    let position = (record.payload.source.seq, op.id);
                    if added {
                        let _: bool = prefixes.insert(position);
                    } else {
                        let _: bool = prefixes.remove(&position);
                    }
                    let _: bool = self
                        .threads
                        .entry(meta.thread.clone())
                        .or_default()
                        .insert(source);
                    let _: bool = self
                        .source_threads
                        .entry(source)
                        .or_default()
                        .insert(meta.thread.clone());
                    if let Some(parent) = &meta.parent {
                        self.watch(parent, &meta.thread);
                    }
                    self.dirty(&meta.thread);
                    self.fact(op, added);
                }
                ProviderFact::CodexLifecycle(meta) => {
                    let observations = self.observations.entry(meta.thread.clone()).or_default();
                    if added {
                        let _: bool = observations.insert(op.id);
                    } else {
                        let _: bool = observations.remove(&op.id);
                    }
                    if let CodexLifecycleEvent::Spawn { child, .. }
                    | CodexLifecycleEvent::Completed { child } = &meta.event
                    {
                        self.watch(&meta.thread, child);
                    }
                    let _: bool = self.pending.insert(meta.thread);
                    self.fact(op, added);
                }
                ProviderFact::CodexDerivation(_) | ProviderFact::ClaudeDerivation(_) => {}
            }
        }
        let threads = self
            .source_threads
            .get(&stream)
            .cloned()
            .unwrap_or_default();
        for thread in threads {
            self.dirty(&thread);
        }
    }

    fn fact(&mut self, op: &Arc<Op>, added: bool) {
        if added {
            drop(self.facts.insert(op.id, Arc::clone(op)));
        } else {
            drop(self.facts.remove(&op.id));
        }
    }

    fn watch(&mut self, parent: &CodexThreadId, child: &CodexThreadId) {
        let _: bool = self
            .targets
            .entry(parent.clone())
            .or_default()
            .insert(child.clone());
        let _: bool = self
            .watchers
            .entry(child.clone())
            .or_default()
            .insert(parent.clone());
        let _: bool = self.pending.insert(parent.clone());
    }

    fn dirty(&mut self, thread: &CodexThreadId) {
        let _: bool = self.pending.insert(thread.clone());
        self.pending
            .extend(self.watchers.get(thread).into_iter().flatten().cloned());
    }

    pub(super) fn resolve(&mut self, ops: &Map<OpId, Arc<Op>>) -> RelationChanges {
        let mut changes = RelationChanges::default();
        for thread in std::mem::take(&mut self.pending) {
            let ids = self.evidence(&thread);
            let records: Vec<_> = ids
                .iter()
                .filter_map(|id| self.facts.get(id))
                .filter_map(|op| decode_evidence(op))
                .collect();
            let resolved = resolve_records(
                &records,
                &Corpus {
                    raw: &self.raw,
                    ops,
                },
            );
            let mut edges = BTreeSet::new();
            for op in resolved.notes {
                if let OpKind::Note(note) = op.kind {
                    for anchor in &op.parents {
                        for target in &note.target_ids {
                            let _: bool = edges.insert(RelationEdge {
                                anchor: *anchor,
                                target: *target,
                                spawn: note.relationship == NoteRelationship::SpawnedBy,
                            });
                        }
                    }
                }
            }
            let previous = self.published.get(&thread).cloned().unwrap_or_default();
            changes.added.extend(edges.difference(&previous).copied());
            changes.removed.extend(previous.difference(&edges).copied());
            drop(self.published.insert(thread, edges));
        }
        changes
    }

    fn evidence(&self, thread: &CodexThreadId) -> BTreeSet<OpId> {
        let mut ids = self.observations.get(thread).cloned().unwrap_or_default();
        for target in std::iter::once(thread).chain(self.targets.get(thread).into_iter().flatten())
        {
            for stream in self.threads.get(target).into_iter().flatten() {
                if let Some(prefixes) = self.prefixes.get(stream) {
                    // Conflicting execution identities are rare repair cases.
                    // Keep every claimed identity visible to the shared
                    // resolver's ambiguity fence, including older prefixes.
                    if self
                        .source_threads
                        .get(stream)
                        .is_some_and(|threads| threads.len() > 1)
                    {
                        ids.extend(prefixes.iter().map(|(_, id)| *id));
                        continue;
                    }
                    if let Some((last, _)) = prefixes.last() {
                        ids.extend(
                            prefixes
                                .iter()
                                .rev()
                                .take_while(|(seq, _)| seq == last)
                                .map(|(_, id)| *id),
                        );
                    }
                }
            }
        }
        ids
    }
}

struct Corpus<'a> {
    raw: &'a Map<Stream, Prefix>,
    ops: &'a Map<OpId, Arc<Op>>,
}

impl RawCorpus for Corpus<'_> {
    fn matches(&self, source: OpId, hash: [u8; 32]) -> bool {
        self.ops
            .get(&source)
            .is_some_and(|op| matches!(&op.kind, OpKind::Import(raw) if raw.raw_hash == Some(hash)))
    }
    fn complete(&self, meta: &CodexSourceEvidence, scope: ScopeRef) -> bool {
        self.raw
            .get(&super::stream(meta.first))
            .is_some_and(|prefix| {
                u64::try_from(prefix.sequences.len()).ok() == Some(meta.last.seq >> 16)
                    && prefix.sequences.last() == Some(&meta.last.seq)
                    && prefix.irregular == 0
                    && prefix.scopes.len() == 1
                    && prefix
                        .scopes
                        .first()
                        .is_some_and(|(value, _)| *value == scope)
            })
    }
}
