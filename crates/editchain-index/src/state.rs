//! Checkpoint state contains only facts derivable from canonical storage.

use std::{collections::BTreeSet, io, path::Path};

use editchain_core::{Op, OpId};
use editchain_store::{read_op_at, BlobSource, ChainDelta, IndexedTail, OpRecordLocation};
use serde::{Deserialize, Serialize};

use crate::{
    content::ContentIndex, keys::keys, references::references, IndexKey, Map, OrderedMap,
    OrderedSet,
};

pub(crate) const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct State {
    pub(crate) version: u32,
    pub(crate) tail: IndexedTail,
    pub(crate) records: OrderedMap<OpId, OpRecordLocation>,
    pub(crate) postings: Map<IndexKey, OrderedSet<OpId>>,
    pub(crate) content: ContentIndex,
}

impl State {
    pub(crate) fn build(root: &Path, blobs: &impl BlobSource) -> io::Result<Self> {
        let tail = IndexedTail::open(root)?;
        let mut state = Self {
            version: VERSION,
            tail,
            records: OrderedMap::new(),
            postings: Map::new(),
            content: ContentIndex::default(),
        };
        let mut reads = 0;
        let operations: Vec<_> = state.tail.chain().shared_ops().collect();
        for op in operations {
            let location = state
                .tail
                .chain()
                .record_locations(op.id)
                .next()
                .ok_or_else(|| invalid("accepted operation has no record location"))?;
            state.add(&op, location, blobs, &mut reads)?;
        }
        Ok(state)
    }

    pub(crate) fn apply(
        &mut self,
        root: &Path,
        delta: &ChainDelta,
        blobs: &impl BlobSource,
        reads: &mut u64,
    ) -> io::Result<BTreeSet<OpId>> {
        for id in &delta.removed {
            if let Some(location) = self.records.remove(id) {
                self.remove(&read_op_at(root, location)?);
            }
        }
        let changed = self.content.refresh(blobs, reads)?;
        for (op, location) in delta.added.values() {
            self.add(op, *location, blobs, reads)?;
        }
        Ok(changed)
    }

    fn add(
        &mut self,
        op: &Op,
        location: OpRecordLocation,
        blobs: &impl BlobSource,
        reads: &mut u64,
    ) -> io::Result<()> {
        let references = references(op);
        for reference in &references {
            self.content.add(*reference, op.id, blobs, reads)?;
        }
        for key in keys(op, &references) {
            let _inserted = self.postings.entry(key).or_default().insert(op.id);
        }
        let _previous = self.records.insert(op.id, location);
        Ok(())
    }

    fn remove(&mut self, op: &Op) {
        let references = references(op);
        for key in keys(op, &references) {
            if let Some(owners) = self.postings.get_mut(&key) {
                let _removed = owners.remove(&op.id);
                if owners.is_empty() {
                    drop(self.postings.remove(&key));
                }
            }
        }
        for reference in references {
            self.content.remove(&reference, op.id);
        }
    }

    pub(crate) fn matches(&self, expected: &Self) -> bool {
        self.version == expected.version
            && self.records.len() == expected.records.len()
            && self.records.iter().eq(expected.records.iter())
            && self.postings.len() == expected.postings.len()
            && self.postings.iter().all(|(key, ids)| {
                expected
                    .postings
                    .get(key)
                    .is_some_and(|other| ids.iter().eq(other.iter()))
            })
            && self.content.matches(&expected.content)
            && self.admission_matches(expected)
    }

    fn admission_matches(&self, expected: &Self) -> bool {
        let actual = self.tail.chain();
        let expected = expected.tail.chain();
        actual.stats() == expected.stats()
            && actual.identities().count() == expected.identities().count()
            && expected.identities().all(|id| {
                actual.get(id) == expected.get(id)
                    && actual
                        .record_locations(id)
                        .eq(expected.record_locations(id))
            })
    }
}

pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
