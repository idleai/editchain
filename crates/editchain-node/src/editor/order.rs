//! Replay captured parent order rather than sorting independent recorder UUIDs.

use editchain_core::{Op, OpId};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn sources(operations: Vec<Op>) -> super::Result<Vec<Op>> {
    let mut pending: BTreeMap<_, _> = operations.into_iter().map(|op| (op.id, op)).collect();
    let mut remaining = BTreeMap::new();
    let mut children = BTreeMap::<OpId, Vec<OpId>>::new();
    let mut ready = BTreeSet::new();
    for (id, op) in &pending {
        let parents: Vec<_> = op
            .parents
            .iter()
            .filter(|parent| pending.contains_key(parent))
            .copied()
            .collect();
        let _previous = remaining.insert(*id, parents.len());
        if parents.is_empty() {
            let _inserted = ready.insert(*id);
        }
        for parent in parents {
            children.entry(parent).or_default().push(*id);
        }
    }
    let mut ordered = Vec::with_capacity(pending.len());
    while let Some(id) = ready.pop_first() {
        if let Some(op) = pending.remove(&id) {
            ordered.push(op);
        }
        for child in children.remove(&id).into_iter().flatten() {
            let count = remaining
                .get_mut(&child)
                .ok_or("missing editor replay dependency")?;
            *count = count.saturating_sub(1);
            if *count == 0 {
                let _inserted = ready.insert(child);
            }
        }
    }
    if !pending.is_empty() {
        return Err("editor recording parent cycle".into());
    }
    Ok(ordered)
}
