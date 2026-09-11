//! Select complete occurrence materializations and replay their logical changes.

use std::collections::{BTreeMap, HashMap, HashSet};

use editchain_core::provider::{
    ClaudeDerivationEvidence, CodexDerivationEvidence, CodexLogicalChange, CodexThreadId,
    ProviderFact,
};
use editchain_core::{Op, OpId, OpKind, ParentSet};

use crate::provider::{decode_evidence, EvidenceRecord};

/// Current state of a Codex logical item, rebuilt from immutable occurrences.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Derivation<'a> {
    Codex(&'a CodexDerivationEvidence),
    Claude(&'a ClaudeDerivationEvidence),
}

impl<'a> Derivation<'a> {
    fn from_fact(fact: &'a ProviderFact) -> Option<Self> {
        match fact {
            ProviderFact::CodexDerivation(meta) => Some(Self::Codex(meta)),
            ProviderFact::ClaudeDerivation(meta) => Some(Self::Claude(meta)),
            ProviderFact::CodexSource(_) | ProviderFact::CodexLifecycle(_) => None,
        }
    }

    fn outputs(self) -> &'a [OpId] {
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
}

#[derive(Debug, Clone, Default)]
pub(super) struct Materialization {
    pub(super) hidden: HashSet<OpId>,
    pub(super) representatives: HashMap<OpId, OpId>,
    pub(super) output_order: HashMap<OpId, usize>,
    pub(super) items: Vec<CodexLogicalItem>,
}

impl Materialization {
    pub(super) fn from_ops(ops: &[Op]) -> Self {
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
        let mut result = Self::default();
        let mut logical: BTreeMap<SourceKey, LogicalTurns> = BTreeMap::new();
        let mut blocked = HashSet::new();
        for (source, records) in &by_source {
            result.track_outputs(*source, records, &by_id);
            let selected = select(records).filter(|meta| complete_outputs(*meta, *source, &by_id));
            if let Some(meta) = selected {
                for (index, output) in meta.outputs().iter().enumerate() {
                    let _: bool = result.hidden.remove(output);
                    let _: Option<usize> = result.output_order.insert(*output, index);
                }
                if let Derivation::Codex(meta) = meta {
                    apply_changes(
                        logical.entry(source_key(*source)).or_default(),
                        *source,
                        meta,
                    );
                }
            } else {
                let _: bool = blocked.insert(source_key(*source));
            }
        }
        blocked.extend(incomplete_sources(&by_source, &by_id));
        // A covered record's legacy numeric lanes remain stored and
        // addressable, but their cursor-dependent fold no longer supplies
        // display content. Incomplete replacements do not revive stale data.
        for op in ops {
            if op.id.seq.trailing_zeros() >= 16 {
                continue;
            }
            let raw = OpId {
                seq: op.id.seq & !0xffff,
                ..op.id
            };
            if by_source.contains_key(&raw) {
                let _: bool = result.hidden.insert(op.id);
                let _: Option<OpId> = result.representatives.insert(op.id, raw);
            }
        }
        result.items = logical
            .into_iter()
            .filter(|(source, _)| !blocked.contains(source))
            .flat_map(|(_, turns)| turns.into_values())
            .flat_map(BTreeMap::into_values)
            .collect();
        result
    }

    fn track_outputs(
        &mut self,
        source: OpId,
        records: &[&EvidenceRecord<'_>],
        by_id: &HashMap<OpId, &Op>,
    ) {
        for record in records {
            if let Some(meta) = Derivation::from_fact(&record.payload.fact) {
                let outputs = meta.outputs().iter().copied().collect();
                for output in meta.outputs() {
                    if !reaches_source(*output, source, &outputs, by_id) {
                        continue;
                    }
                    let _: bool = self.hidden.insert(*output);
                    let _: Option<OpId> = self.representatives.insert(*output, source);
                }
            }
        }
    }
}

fn source_key(source: OpId) -> SourceKey {
    (source.node.0, source.boot)
}

fn incomplete_sources(
    records: &BTreeMap<OpId, Vec<&EvidenceRecord<'_>>>,
    by_id: &HashMap<OpId, &Op>,
) -> HashSet<SourceKey> {
    let mut coverage: BTreeMap<SourceKey, (u64, u64)> = BTreeMap::new();
    let mut blocked = HashSet::new();
    for op in by_id
        .values()
        .filter(|op| matches!(op.kind, OpKind::Import(_)))
    {
        let key = source_key(op.id);
        let (count, last) = coverage.entry(key).or_default();
        *count = count.saturating_add(1);
        *last = (*last).max(op.id.seq >> 16);
        if !records.contains_key(&op.id) {
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

fn valid_source(record: &EvidenceRecord<'_>, by_id: &HashMap<OpId, &Op>) -> bool {
    record.payload.source.seq > 0
        && record.payload.source.seq.trailing_zeros() >= 16
        && by_id.get(&record.payload.source).is_some_and(|raw| {
        record.op.scope == raw.scope
            && matches!(&raw.kind, OpKind::Import(import) if import.raw_hash == Some(record.payload.raw_hash))
    })
}

fn select<'a>(records: &[&'a EvidenceRecord<'_>]) -> Option<Derivation<'a>> {
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
    let mut eligible = candidates
        .into_iter()
        .filter(|meta| meta.includes_thinking() == includes_thinking);
    let first = eligible.next()?;
    eligible.all(|meta| meta == first).then_some(first)
}

fn complete_outputs(meta: Derivation<'_>, source: OpId, by_id: &HashMap<OpId, &Op>) -> bool {
    let outputs: HashSet<OpId> = meta.outputs().iter().copied().collect();
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
    by_id: &HashMap<OpId, &Op>,
) -> bool {
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
                && incarnation.seq <= source.seq
                && by_id
                    .get(incarnation)
                    .is_some_and(|op| matches!(op.kind, OpKind::Import(_)))
                && item_outputs.iter().all(|output| outputs.contains(output))
        }
    })
}

fn reaches_source(
    mut id: OpId,
    source: OpId,
    outputs: &HashSet<OpId>,
    by_id: &HashMap<OpId, &Op>,
) -> bool {
    if id.node == source.node || id.boot != source.boot || id.seq >> 16 != source.seq >> 16 {
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

fn apply_changes(turns: &mut LogicalTurns, source: OpId, meta: &CodexDerivationEvidence) {
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
                        incarnation: *incarnation,
                        source,
                        outputs: outputs.clone(),
                    },
                ));
            }
        }
    }
}
