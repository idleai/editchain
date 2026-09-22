//! Parent-first export order over an already consent-filtered snapshot.

use crate::{invalid, RecordKey};
use editchain_store::format::decode_op;
use std::io;

pub(crate) fn parent_first(records: &crate::evidence::Records) -> io::Result<Vec<RecordKey>> {
    let keys: Vec<_> = records.keys().copied().collect();
    let mut state = vec![0u8; keys.len()];
    let mut ordered = Vec::with_capacity(keys.len());
    let mut pending = Vec::new();
    for start in 0..keys.len() {
        pending.push((start, false));
        while let Some((index, finish)) = pending.pop() {
            let key = *keys
                .get(index)
                .ok_or_else(|| invalid("missing inventory key"))?;
            let seen = state
                .get_mut(index)
                .ok_or_else(|| invalid("missing inventory state"))?;
            if finish {
                *seen = 2;
                ordered.push(key);
            } else if *seen == 0 {
                *seen = 1;
                pending.push((index, true));
                let bytes = records
                    .get(&key)
                    .ok_or_else(|| invalid("missing inventory record"))?;
                let op = decode_op(bytes).map_err(io::Error::other)?;
                for parent in &op.parents {
                    // All conflicting variants precede descendants. Missing or
                    // withheld parents stay missing; ordering grants no export authority.
                    let first = keys.partition_point(|key| key.id < *parent);
                    let end = keys.partition_point(|key| key.id <= *parent);
                    pending.extend((first..end).rev().map(|index| (index, false)));
                }
            }
            // A visiting ancestor is a cycle, not a reason to recurse forever
            // or discard conflict evidence. Stable DFS breaks only that back edge.
        }
    }
    Ok(ordered)
}
