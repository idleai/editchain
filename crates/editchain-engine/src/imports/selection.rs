//! Select validated derivations and replay explicit provider changes.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{
    decode_evidence, source_key, CodexLogicalItem, EvidenceRecord, LogicalTurns, SourceKey,
};
use crate::provider::{
    ClaudeDerivationEvidence, CodexDerivationContract, CodexDerivationEvidence, CodexLogicalChange,
    ProviderFact,
};
use crate::{Op, OpId, OpKind, ParentSet};
use editchain_core::SourceId;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Derivation<'a> {
    Codex(&'a CodexDerivationEvidence),
    Claude(&'a ClaudeDerivationEvidence),
}

impl<'a> Derivation<'a> {
    pub(super) fn from_fact(fact: &'a ProviderFact) -> Option<Self> {
        match fact {
            ProviderFact::CodexDerivation(meta) => Some(Self::Codex(meta)),
            ProviderFact::ClaudeDerivation(meta) => Some(Self::Claude(meta)),
            ProviderFact::CodexSource(_) | ProviderFact::CodexLifecycle(_) => None,
        }
    }

    pub(super) fn outputs(self) -> &'a [SourceId] {
        match self {
            Self::Codex(meta) => &meta.outputs,
            Self::Claude(meta) => &meta.outputs,
        }
    }

    fn includes_thinking(self) -> bool {
        match self {
            Self::Codex(meta) => meta.includes_thinking,
            Self::Claude(meta) => meta.includes_thinking,
        }
    }

    fn revision(self) -> u8 {
        match self {
            Self::Codex(meta) if meta.contract == CodexDerivationContract::OccurrencesV2 => 2,
            Self::Codex(_) | Self::Claude(_) => 1,
        }
    }
}

/// Accepted operation lookup used to validate provider derivations.
pub trait OpLookup {
    /// Look up one accepted operation by its immutable identity.
    fn get(&self, id: &OpId) -> Option<&Op>;
}

impl<S: std::hash::BuildHasher> OpLookup for HashMap<OpId, &Op, S> {
    fn get(&self, id: &OpId) -> Option<&Op> {
        Self::get(self, id).copied()
    }
}

impl OpLookup for editchain_index::Map<OpId, std::sync::Arc<Op>> {
    fn get(&self, id: &OpId) -> Option<&Op> {
        Self::get(self, id).map(AsRef::as_ref)
    }
}

/// Select a complete, unambiguous Codex derivation for one raw occurrence.
#[must_use]
pub fn selected_codex<'a>(
    source: OpId,
    facts: impl Iterator<Item = &'a Op>,
    by_id: &impl OpLookup,
) -> Option<CodexDerivationEvidence> {
    let records: Vec<_> = facts
        .filter_map(decode_evidence)
        .filter(|record| record.payload.source.id() == source && valid_source(record, by_id))
        .collect();
    let references: Vec<_> = records.iter().collect();
    match select(&references).filter(|meta| complete_outputs(*meta, source, by_id))? {
        Derivation::Codex(meta) => Some(meta.clone()),
        Derivation::Claude(_) => None,
    }
}

/// Whether the occurrence has a complete, unambiguous provider derivation.
#[must_use]
pub fn complete_derivation<'a>(
    source: OpId,
    facts: impl Iterator<Item = &'a Op>,
    by_id: &impl OpLookup,
) -> bool {
    let records: Vec<_> = facts
        .filter_map(decode_evidence)
        .filter(|record| record.payload.source.id() == source && valid_source(record, by_id))
        .collect();
    let references: Vec<_> = records.iter().collect();
    select(&references).is_some_and(|meta| complete_outputs(meta, source, by_id))
}

pub(super) fn incomplete_sources(
    records: &BTreeMap<SourceId, Vec<&EvidenceRecord<'_>>>,
    by_id: &HashMap<OpId, &Op>,
) -> HashSet<SourceKey> {
    let mut coverage: BTreeMap<SourceKey, (u64, u64)> = BTreeMap::new();
    let mut blocked = HashSet::new();
    for op in by_id
        .values()
        .filter(|op| matches!(op.kind, OpKind::Import(_)))
    {
        let Some(source) = op.source else {
            continue;
        };
        let key = source_key(source);
        let (count, last) = coverage.entry(key).or_default();
        *count = count.saturating_add(1);
        *last = (*last).max(source.seq >> 16);
        if !records.contains_key(&source) {
            let _: bool = blocked.insert(key);
        }
    }
    for (key, (count, last)) in coverage {
        if count != last {
            let _: bool = blocked.insert(key);
        }
    }
    blocked
}

pub(super) fn valid_source(record: &EvidenceRecord<'_>, by_id: &impl OpLookup) -> bool {
    record.payload.source.seq > 0
        && record.payload.source.seq.trailing_zeros() >= 16
        && by_id.get(&record.payload.source.id()).is_some_and(|raw| {
        record.op.scope == raw.scope
            && matches!(&raw.kind, OpKind::Import(import) if import.raw_hash == Some(record.payload.raw_hash))
    })
}

pub(super) fn select<'a>(records: &[&'a EvidenceRecord<'_>]) -> Option<Derivation<'a>> {
    let candidates: Vec<_> = records
        .iter()
        .filter_map(|record| Derivation::from_fact(&record.payload.fact))
        .collect();
    let first = candidates.first()?;
    if candidates
        .iter()
        .any(|candidate| std::mem::discriminant(candidate) != std::mem::discriminant(first))
    {
        return None;
    }
    // Disabling capture cannot erase already captured reasoning.
    let includes_thinking = candidates.iter().any(|meta| meta.includes_thinking());
    let revision = candidates
        .iter()
        .filter(|meta| meta.includes_thinking() == includes_thinking)
        .map(|meta| meta.revision())
        .max()?;
    let mut eligible = candidates.into_iter().filter(|meta| {
        meta.revision() == revision && meta.includes_thinking() == includes_thinking
    });
    let first = eligible.next()?;
    eligible.all(|meta| meta == first).then_some(first)
}

pub(super) fn complete_outputs(meta: Derivation<'_>, source: OpId, by_id: &impl OpLookup) -> bool {
    let outputs: HashSet<OpId> = meta.outputs().iter().map(|source| source.id()).collect();
    if outputs.len() != meta.outputs().len() || outputs.contains(&source) {
        return false;
    }
    if !outputs
        .iter()
        .all(|output| reaches_source(*output, source, &outputs, by_id))
    {
        return false;
    }
    match meta {
        Derivation::Claude(_) => true,
        Derivation::Codex(meta) => {
            !meta.thread.0.is_empty() && valid_changes(meta, source, &outputs, by_id)
        }
    }
}

fn valid_changes(
    meta: &CodexDerivationEvidence,
    source: OpId,
    outputs: &HashSet<OpId>,
    by_id: &impl OpLookup,
) -> bool {
    let Some(source) = by_id.get(&source).and_then(|op| op.source) else {
        return false;
    };
    meta.changes.iter().all(|change| match change {
        CodexLogicalChange::RemoveTurn { turn } => !turn.is_empty(),
        CodexLogicalChange::Upsert {
            turn,
            item,
            incarnation,
            outputs: item_outputs,
        } => {
            !turn.is_empty()
                && !item.is_empty()
                && source_key(*incarnation) == source_key(source)
                && incarnation.seq > 0
                && incarnation.seq.trailing_zeros() >= 16
                && incarnation.seq <= source.seq
                // The incarnation is a stable identity, not content needed by
                // this revision. Consent can exclude its original occurrence.
                // A present contradictory record must still reject the proof.
                && by_id
                    .get(&incarnation.id())
                    .is_none_or(|op| matches!(op.kind, OpKind::Import(_)))
                && item_outputs.iter().all(|output| outputs.contains(&output.id()))
        }
    })
}

pub(super) fn reaches_source(
    mut id: OpId,
    source: OpId,
    outputs: &HashSet<OpId>,
    by_id: &impl OpLookup,
) -> bool {
    let Some(origin) = by_id.get(&id).and_then(|op| op.source) else {
        return false;
    };
    let Some(raw) = by_id.get(&source).and_then(|op| op.source) else {
        return false;
    };
    if origin.node == raw.node || origin.boot != raw.boot || origin.seq >> 16 != raw.seq >> 16 {
        return false;
    }
    let mut seen = HashSet::new();
    while id != source {
        if !outputs.contains(&id) || !seen.insert(id) {
            return false;
        }
        let Some(op) = by_id.get(&id) else {
            return false;
        };
        if matches!(op.kind, OpKind::Import(_)) || decode_evidence(op).is_some() {
            return false;
        }
        let ParentSet::One(parent) = op.parents else {
            return false;
        };
        id = parent;
    }
    true
}

pub(super) fn apply_changes(
    turns: &mut LogicalTurns,
    source: SourceId,
    meta: &CodexDerivationEvidence,
) {
    for change in &meta.changes {
        match change {
            CodexLogicalChange::RemoveTurn { turn } => {
                drop(turns.remove(turn));
            }
            CodexLogicalChange::Upsert {
                turn,
                item,
                incarnation,
                outputs,
            } => {
                drop(turns.entry(turn.clone()).or_default().insert(
                    item.clone(),
                    CodexLogicalItem {
                        thread: meta.thread.clone(),
                        turn: turn.clone(),
                        item: item.clone(),
                        incarnation: incarnation.id(),
                        source: source.id(),
                        outputs: outputs.iter().map(|source| source.id()).collect(),
                    },
                ));
            }
        }
    }
}
