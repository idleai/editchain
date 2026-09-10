//! Resolve provider endpoints from the complete admitted operation corpus.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use editchain_core::provider::{
    CodexLifecycleEvent, CodexLifecycleEvidence, CodexSourceEvidence, CodexSpawnSignal,
    CodexThreadId, ProviderEvidence, ProviderFact,
};
use editchain_core::{NoteRelationship, Op, OpId, OpKind, ParentSet, Payload, Tags};

type SourceKey = (u64, u32);

#[derive(Debug, Default)]
pub(super) struct ProviderRelations {
    pub(super) notes: Vec<Op>,
    covered: HashSet<SourceKey>,
}

impl ProviderRelations {
    pub(super) fn replaces_legacy_note(&self, op: &Op) -> bool {
        let OpKind::Note(note) = &op.kind else {
            return false;
        };
        if !matches!(
            note.relationship,
            NoteRelationship::SpawnedBy | NoteRelationship::ReconnectsTo
        ) || !op
            .parents
            .iter()
            .chain(&note.target_ids)
            .any(|id| self.covered.contains(&source_key(*id)))
        {
            return false;
        }
        let Payload::Inline(content) = &note.content else {
            return false;
        };
        serde_json::from_slice::<serde_json::Value>(content)
            .ok()
            .is_some_and(|value| {
                value.get("provider").and_then(serde_json::Value::as_str) == Some("codex")
                    && value.get("resolver").and_then(serde_json::Value::as_str)
                        == Some("codex-topology-v2")
            })
    }
}

#[derive(Debug)]
struct EvidenceRecord<'a> {
    op: &'a Op,
    payload: ProviderEvidence,
}

#[derive(Debug)]
struct Source<'a> {
    meta: &'a CodexSourceEvidence,
    proof: &'a Op,
}

#[derive(Debug)]
struct Lifecycle<'a> {
    meta: &'a CodexLifecycleEvidence,
    source: OpId,
    proof: &'a Op,
}

pub(super) fn resolve(ops: &[Op]) -> ProviderRelations {
    let raw: BTreeMap<OpId, &Op> = ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::Import(_)))
        .map(|op| (op.id, op))
        .collect();
    let records: Vec<_> = ops.iter().filter_map(decode_evidence).collect();
    let mut prefixes: BTreeMap<SourceKey, Vec<&EvidenceRecord<'_>>> = BTreeMap::new();
    for record in &records {
        if matches!(record.payload.fact, ProviderFact::CodexSource(_)) {
            prefixes
                .entry(source_key(record.payload.source))
                .or_default()
                .push(record);
        }
    }
    let mut resolved = ProviderRelations {
        covered: prefixes.keys().copied().collect(),
        notes: Vec::new(),
    };
    let mut blocked = BTreeSet::new();
    let mut sources = Vec::new();
    for prefixes in prefixes.values() {
        if let Some(source) = select_source(prefixes, &raw) {
            sources.push(source);
        } else {
            for record in prefixes {
                if let ProviderFact::CodexSource(meta) = &record.payload.fact {
                    let _: bool = blocked.insert(meta.thread.clone());
                }
            }
        }
    }
    let lifecycle: Vec<_> = records
        .iter()
        .filter_map(|record| {
            let ProviderFact::CodexLifecycle(meta) = &record.payload.fact else {
                return None;
            };
            let source = record.payload.source;
            (valid_occurrence(record, &raw)
                && sources.iter().any(|candidate| {
                    source_key(candidate.meta.first) == source_key(source)
                        && candidate.meta.thread == meta.thread
                        && source.seq <= candidate.meta.last.seq
                }))
            .then_some(Lifecycle {
                meta,
                source,
                proof: record.op,
            })
        })
        .collect();
    resolve_spawns(&sources, &lifecycle, &blocked, &mut resolved.notes);
    resolve_completions(&sources, &lifecycle, &blocked, &mut resolved.notes);
    resolved
        .notes
        .sort_by_key(|note| (note.parents.iter().next().copied(), note.id));
    resolved.notes.dedup();
    resolved
}

fn decode_evidence(op: &Op) -> Option<EvidenceRecord<'_>> {
    let OpKind::Note(note) = &op.kind else {
        return None;
    };
    if note.relationship != NoteRelationship::ProviderEvidence
        || !note.target_ids.is_empty()
        || !op.tags.matches_all(Tags::META | Tags::IMPORT)
    {
        return None;
    }
    let Payload::Inline(content) = &note.content else {
        return None;
    };
    let payload: ProviderEvidence = serde_json::from_slice(content).ok()?;
    (op.parents == ParentSet::One(payload.source)).then_some(EvidenceRecord { op, payload })
}

fn source_key(id: OpId) -> SourceKey {
    (id.node.0, id.boot)
}

fn valid_occurrence(record: &EvidenceRecord<'_>, raw: &BTreeMap<OpId, &Op>) -> bool {
    raw.get(&record.payload.source).is_some_and(|op| {
        matches!(&op.kind, OpKind::Import(import) if import.raw_hash == Some(record.payload.raw_hash))
    })
}

fn select_source<'a>(
    prefixes: &[&'a EvidenceRecord<'_>],
    raw: &BTreeMap<OpId, &Op>,
) -> Option<Source<'a>> {
    let latest = prefixes
        .iter()
        .map(|record| record.payload.source.seq)
        .max()?;
    let mut candidates = prefixes
        .iter()
        .copied()
        .filter(|record| record.payload.source.seq == latest);
    let first = candidates.next()?;
    if candidates.any(|candidate| candidate.payload != first.payload)
        || !valid_occurrence(first, raw)
    {
        return None;
    }
    let ProviderFact::CodexSource(meta) = &first.payload.fact else {
        return None;
    };
    if meta.thread.0.is_empty()
        || meta.last != first.payload.source
        || meta.first.seq != 1 << 16
        || meta.last.seq.trailing_zeros() < 16
        || source_key(meta.first) != source_key(meta.last)
    {
        return None;
    }
    let present: Vec<_> = raw
        .range(
            meta.first..=OpId {
                seq: u64::MAX,
                ..meta.last
            },
        )
        .filter(|(id, _)| source_key(**id) == source_key(meta.first))
        .collect();
    // A missing/conflicted record or a partially appended later prefix cannot
    // turn an older source extent into an apparently current child terminal.
    if u64::try_from(present.len()).ok()? != meta.last.seq >> 16
        || present.last().map(|(id, _)| **id) != Some(meta.last)
        || present
            .iter()
            .any(|(id, raw)| id.seq.trailing_zeros() < 16 || raw.scope != first.op.scope)
    {
        return None;
    }
    Some(Source {
        meta,
        proof: first.op,
    })
}

fn unique_source<'a>(
    thread: &CodexThreadId,
    sources: &'a [Source<'_>],
    blocked: &BTreeSet<CodexThreadId>,
) -> Option<&'a Source<'a>> {
    if blocked.contains(thread) {
        return None;
    }
    let mut candidates = sources
        .iter()
        .filter(|source| source.meta.thread == *thread);
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

fn resolve_spawns(
    sources: &[Source<'_>],
    lifecycle: &[Lifecycle<'_>],
    blocked: &BTreeSet<CodexThreadId>,
    notes: &mut Vec<Op>,
) {
    for child in sources {
        let Some(parent) = &child.meta.parent else {
            continue;
        };
        if unique_source(&child.meta.thread, sources, blocked).is_none() || blocked.contains(parent)
        {
            continue;
        }
        let candidates: Vec<_> = lifecycle.iter().filter(|observation| {
            observation.meta.thread == *parent
                && matches!(&observation.meta.event, CodexLifecycleEvent::Spawn { child: target, .. } if *target == child.meta.thread)
        }).collect();
        let legacy = candidates.iter().any(|candidate| {
            matches!(
                candidate.meta.event,
                CodexLifecycleEvent::Spawn {
                    signal: CodexSpawnSignal::SubagentActivity,
                    ..
                }
            )
        });
        let mut activations: BTreeMap<OpId, Vec<&Op>> = BTreeMap::new();
        for candidate in candidates {
            if let CodexLifecycleEvent::Spawn {
                activation, signal, ..
            } = candidate.meta.event
            {
                if (!legacy || signal == CodexSpawnSignal::SubagentActivity)
                    && source_key(activation) == source_key(candidate.source)
                    && activation.seq > 0
                    && activation.seq <= candidate.source.seq
                    && activation.seq.trailing_zeros() >= 16
                {
                    activations
                        .entry(activation)
                        .or_default()
                        .push(candidate.proof);
                }
            }
        }
        if activations.len() == 1 {
            if let Some((activation, proof)) = activations.into_iter().next() {
                add_relation(
                    notes,
                    child.meta.first,
                    activation,
                    NoteRelationship::SpawnedBy,
                    proof.into_iter().chain(std::iter::once(child.proof)),
                );
            }
        }
    }
}

fn resolve_completions(
    sources: &[Source<'_>],
    lifecycle: &[Lifecycle<'_>],
    blocked: &BTreeSet<CodexThreadId>,
    notes: &mut Vec<Op>,
) {
    for completion in lifecycle {
        let (child, activation_proof) = match &completion.meta.event {
            CodexLifecycleEvent::Completed { child } => (child, Vec::new()),
            CodexLifecycleEvent::LegacyCompleted { agent_path } => {
                let Some((child, proof)) = legacy_child(completion, agent_path, lifecycle) else {
                    continue;
                };
                (child, proof)
            }
            CodexLifecycleEvent::Spawn { .. } => continue,
        };
        let Some(child) = unique_source(child, sources, blocked) else {
            continue;
        };
        add_relation(
            notes,
            completion.source,
            child.meta.last,
            NoteRelationship::ReconnectsTo,
            [completion.proof, child.proof]
                .into_iter()
                .chain(activation_proof),
        );
    }
}

fn legacy_child<'a>(
    completion: &Lifecycle<'_>,
    path: &str,
    lifecycle: &'a [Lifecycle<'_>],
) -> Option<(&'a CodexThreadId, Vec<&'a Op>)> {
    let mut candidates: BTreeMap<&CodexThreadId, Vec<&Op>> = BTreeMap::new();
    for spawn in lifecycle {
        if source_key(spawn.source) != source_key(completion.source) {
            continue;
        }
        if let CodexLifecycleEvent::Spawn {
            child,
            agent_path: Some(agent_path),
            ..
        } = &spawn.meta.event
        {
            if agent_path == path {
                candidates.entry(child).or_default().push(spawn.proof);
            }
        }
    }
    (candidates.len() == 1)
        .then(|| candidates.into_iter().next())
        .flatten()
}

fn add_relation<'a>(
    notes: &mut Vec<Op>,
    anchor: OpId,
    target: OpId,
    relationship: NoteRelationship,
    evidence: impl IntoIterator<Item = &'a Op>,
) {
    for proof in evidence {
        // These clones are projection annotations. The admitted operation and
        // its ID still retain their original source, payload, and envelope.
        let mut annotation = proof.clone();
        annotation.parents = ParentSet::One(anchor);
        if let OpKind::Note(note) = &mut annotation.kind {
            note.relationship = relationship;
            note.target_ids = vec![target];
        }
        notes.push(annotation);
    }
}
