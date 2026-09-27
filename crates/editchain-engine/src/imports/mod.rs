//! Reconcile imported derivations and logical items from accepted immutable records.
//!
//! This is factual replay over provider evidence. Callers retain the original
//! operations and choose how to present copies, revisions and incomplete sources.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::provider::{CodexThreadId, ProviderFact};
use crate::{Op, OpId, OpKind};
use serde::{Deserialize, Serialize};

mod copies;
mod evidence;
mod selection;

pub use evidence::{decode_evidence, EvidenceRecord};
use selection::{
    apply_changes, complete_outputs, incomplete_sources, reaches_source, select, valid_source,
    Derivation,
};
pub use selection::{complete_derivation, selected_codex, OpLookup};

/// Current state of a Codex logical item, rebuilt from immutable occurrences.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CodexLogicalItem {
    /// Full owning provider execution identity.
    pub thread: CodexThreadId,
    /// Full provider turn identity within this execution.
    pub turn: String,
    /// Full provider item identity within this turn.
    pub item: String,
    /// First occurrence since the most recent turn removal.
    pub incarnation: OpId,
    /// Physical occurrence that last revised this item.
    pub source: OpId,
    /// Complete materialized operations for this revision.
    pub outputs: Vec<OpId>,
}

type SourceKey = (u64, u32);
type LogicalTurns = BTreeMap<String, BTreeMap<String, CodexLogicalItem>>;

/// Validated outputs and selected derivation for one source occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDerivation {
    /// Raw imported occurrence supporting the derivation.
    pub source: OpId,
    /// All validated outputs, including superseded derivation versions.
    pub outputs: Vec<OpId>,
    /// Complete selected Claude or Codex derivation, or none when ambiguous/incomplete.
    pub selected: Option<ProviderFact>,
}

/// An operation occurrence proven equivalent to another under source-ID rebinding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportCopy {
    /// Copied physical operation; its canonical bytes remain stored.
    pub operation: OpId,
    /// Deterministically selected equivalent occurrence.
    pub representative: OpId,
}

/// Derived import state; canonical occurrences, revisions and conflicts are not rewritten.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportState {
    /// Source-ordered derivation selections and validated output ownership.
    pub derivations: Vec<ImportDerivation>,
    /// Current Codex logical items after explicit upserts/removals and proven copies.
    pub codex_items: Vec<CodexLogicalItem>,
    /// Exact copy equivalences in operation-ID order, including raw and derived records.
    pub copies: Vec<ImportCopy>,
    /// Present raw occurrences in streams with incomplete or ambiguous derivation coverage.
    /// Raw-only sources can appear here; no logical state is guessed for them.
    pub incomplete_sources: Vec<OpId>,
}

impl ImportState {
    /// Reconcile accepted canonical operations, retaining their full payload references.
    /// Quarantined identities must be excluded by canonical admission before this call.
    #[must_use]
    pub fn from_ops(ops: &[Op]) -> Self {
        Self::from_partial_ops(ops, &HashSet::new())
    }

    /// Reconcile with explicit IDs whose payloads were shortened or are incomplete.
    /// Such records cannot prove copy equivalence. Missing records and ambiguous
    /// derivations also prevent a source stream from supplying logical state.
    #[must_use]
    pub fn from_partial_ops(ops: &[Op], incomplete: &HashSet<OpId>) -> Self {
        let by_id: HashMap<OpId, &Op> = ops.iter().map(|op| (op.id, op)).collect();
        let records: Vec<_> = ops.iter().filter_map(decode_evidence).collect();
        let mut by_source: BTreeMap<OpId, Vec<&EvidenceRecord<'_>>> = BTreeMap::new();
        for record in &records {
            if Derivation::from_fact(&record.payload.fact).is_some() && valid_source(record, &by_id)
            {
                by_source
                    .entry(record.payload.source)
                    .or_default()
                    .push(record);
            }
        }
        let mut state = Self::default();
        let mut logical: BTreeMap<SourceKey, LogicalTurns> = BTreeMap::new();
        let mut blocked = incomplete_sources(&by_source, &by_id);
        let mut selected_codex = BTreeMap::new();
        for (source, records) in &by_source {
            let selected = select(records).filter(|meta| complete_outputs(*meta, *source, &by_id));
            let fact = match selected {
                Some(Derivation::Codex(meta)) => {
                    let _previous = selected_codex.insert(*source, meta);
                    apply_changes(
                        logical.entry(source_key(*source)).or_default(),
                        *source,
                        meta,
                    );
                    Some(ProviderFact::CodexDerivation(meta.clone()))
                }
                Some(Derivation::Claude(meta)) => {
                    Some(ProviderFact::ClaudeDerivation(meta.clone()))
                }
                None => {
                    let _inserted = blocked.insert(source_key(*source));
                    None
                }
            };
            state.derivations.push(ImportDerivation {
                source: *source,
                outputs: tracked_outputs(*source, records, &by_id),
                selected: fact,
            });
        }
        let equivalents = copies::equivalents(&selected_codex, &by_id, &blocked, incomplete);
        state.codex_items = logical
            .into_iter()
            .filter(|(source, _)| !blocked.contains(source))
            .flat_map(|(_, turns)| turns.into_values())
            .flat_map(BTreeMap::into_values)
            .filter(|item| !equivalents.contains_key(&item.source))
            .collect();
        state.copies = equivalents
            .into_iter()
            .map(|(operation, representative)| ImportCopy {
                operation,
                representative,
            })
            .collect();
        state.incomplete_sources = ops
            .iter()
            .filter(|op| {
                matches!(op.kind, OpKind::Import(_)) && blocked.contains(&source_key(op.id))
            })
            .map(|op| op.id)
            .collect();
        state.incomplete_sources.sort_unstable();
        state
    }
}

fn tracked_outputs(
    source: OpId,
    records: &[&EvidenceRecord<'_>],
    by_id: &HashMap<OpId, &Op>,
) -> Vec<OpId> {
    let mut tracked = std::collections::BTreeSet::new();
    for record in records {
        if let Some(meta) = Derivation::from_fact(&record.payload.fact) {
            let outputs = meta.outputs().iter().copied().collect();
            tracked.extend(
                meta.outputs()
                    .iter()
                    .filter(|output| reaches_source(**output, source, &outputs, by_id))
                    .copied(),
            );
        }
    }
    tracked.into_iter().collect()
}

fn source_key(source: OpId) -> SourceKey {
    (source.node.0, source.boot)
}
