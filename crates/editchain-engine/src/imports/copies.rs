//! Prove copied Codex prefixes without changing any recorded evidence.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::provider::{CodexDerivationEvidence, CodexLogicalChange, CodexThreadId};
use crate::{Op, OpId, OpKind, ParentSet};

use super::{source_key, SourceKey};

type Records<'a> = Vec<(OpId, &'a CodexDerivationEvidence)>;

pub(super) fn equivalents(
    selected: &BTreeMap<OpId, &CodexDerivationEvidence>,
    ops: &HashMap<OpId, &Op>,
    blocked: &HashSet<SourceKey>,
    incomplete: &HashSet<OpId>,
) -> BTreeMap<OpId, OpId> {
    let mut equivalents = BTreeMap::new();
    let mut sources: BTreeMap<SourceKey, Records<'_>> = BTreeMap::new();
    for (id, meta) in selected {
        if !blocked.contains(&source_key(*id)) {
            sources
                .entry(source_key(*id))
                .or_default()
                .push((*id, meta));
        }
    }
    // Longest complete source wins, with a deterministic identity tie-breaker.
    // A shorter copy can disappear only when every one of its records and
    // derived operations matches the corresponding prefix of that source.
    let mut ordered: Vec<_> = sources.into_iter().collect();
    ordered.sort_by_key(|(key, records)| (std::cmp::Reverse(records.len()), *key));
    let mut retained: BTreeMap<&CodexThreadId, Vec<&Records<'_>>> = BTreeMap::new();
    for (_, records) in &ordered {
        let Some((_, first)) = records.first() else {
            continue;
        };
        let candidates = retained.entry(&first.thread).or_default();
        let equivalent = candidates
            .iter()
            .find_map(|candidate| equivalent_prefix(records, candidate, ops, incomplete));
        if let Some(mapping) = equivalent {
            equivalents.extend(mapping);
        } else {
            candidates.push(records);
        }
    }
    equivalents
}

fn equivalent_prefix(
    copy: &Records<'_>,
    original: &Records<'_>,
    ops: &HashMap<OpId, &Op>,
    incomplete: &HashSet<OpId>,
) -> Option<BTreeMap<OpId, OpId>> {
    if copy.len() > original.len() {
        return None;
    }
    let mut mapping = BTreeMap::new();
    for ((copy_id, copy_meta), (id, meta)) in copy.iter().zip(original) {
        if copy_id.seq != id.seq || copy_meta.outputs.len() != meta.outputs.len() {
            return None;
        }
        let _previous = mapping.insert(*copy_id, *id);
        mapping.extend(
            copy_meta
                .outputs
                .iter()
                .copied()
                .zip(meta.outputs.iter().copied()),
        );
    }
    // Exact operations, including payload references, clocks, scopes and all
    // known edges, must match after rebinding only the proven occurrence IDs.
    for (copy, original) in &mapping {
        if incomplete.contains(copy)
            || incomplete.contains(original)
            || rebound_op(ops.get(copy)?, &mapping) != **ops.get(original)?
        {
            return None;
        }
    }
    for ((_, copy), (_, original)) in copy.iter().zip(original) {
        let mut rebound = (*copy).clone();
        remap_ids(&mut rebound.outputs, &mapping);
        for change in &mut rebound.changes {
            if let CodexLogicalChange::Upsert {
                incarnation,
                outputs,
                ..
            } = change
            {
                *incarnation = mapped(*incarnation, &mapping);
                remap_ids(outputs, &mapping);
            }
        }
        if rebound != **original {
            return None;
        }
    }
    Some(mapping)
}

fn mapped(id: OpId, mapping: &BTreeMap<OpId, OpId>) -> OpId {
    mapping.get(&id).copied().unwrap_or(id)
}

fn remap_ids(ids: &mut [OpId], mapping: &BTreeMap<OpId, OpId>) {
    for id in ids {
        *id = mapped(*id, mapping);
    }
}

fn rebound_op(op: &Op, mapping: &BTreeMap<OpId, OpId>) -> Op {
    let mut rebound = op.clone();
    rebound.id = mapped(op.id, mapping);
    rebound.parents = match op.parents {
        ParentSet::None => ParentSet::None,
        ParentSet::One(parent) => ParentSet::One(mapped(parent, mapping)),
        ParentSet::Two(left, right) => {
            ParentSet::Two(mapped(left, mapping), mapped(right, mapping))
        }
    };
    if let OpKind::Note(note) = &mut rebound.kind {
        remap_ids(&mut note.target_ids, mapping);
    }
    rebound
}
