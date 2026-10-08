//! Checkpoint state contains only facts derivable from canonical storage.

use std::{collections::BTreeSet, io, path::Path};

use editchain_core::{Op, OpId};
use editchain_store::{read_op_at, BlobSource, ChainDelta, IndexedTail, OpRecordLocation};
use serde::{Deserialize, Serialize};

use crate::{
    content::ContentIndex, keys::keys, references::references, IndexKey, Map, OrderedMap,
    OrderedSet,
};

pub(crate) const VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct State {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) changes: crate::changes::ChangeLog,
    pub(crate) tail: IndexedTail,
    pub(crate) records: OrderedMap<OpId, OpRecordLocation>,
    pub(crate) identities: OrderedSet<OpId>,
    pub(crate) aliases: OrderedMap<OpId, OrderedSet<OpId>>,
    pub(crate) items: OrderedSet<OpId>,
    pub(crate) postings: Map<IndexKey, OrderedSet<OpId>>,
    pub(crate) content: ContentIndex,
}

impl State {
    pub(crate) fn build(root: &Path, blobs: &impl BlobSource) -> io::Result<Self> {
        let tail = IndexedTail::open(root)?;
        let mut state = Self {
            version: VERSION,
            changes: crate::changes::ChangeLog::default(),
            tail,
            records: OrderedMap::new(),
            identities: OrderedSet::new(),
            aliases: OrderedMap::new(),
            items: OrderedSet::new(),
            postings: Map::new(),
            content: ContentIndex::default(),
        };
        state.identities.extend(state.tail.chain().identities());
        let _initialized = state.changes.initialize()?;
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
        state.remember_conflicts(
            root,
            &state
                .tail
                .chain()
                .identities()
                .filter(|id| state.tail.chain().get(*id).is_none())
                .collect::<Vec<_>>(),
        )?;
        Ok(state)
    }

    pub(crate) fn apply(
        &mut self,
        root: &Path,
        delta: &ChainDelta,
        blobs: &impl BlobSource,
        reads: &mut u64,
    ) -> io::Result<BTreeSet<OpId>> {
        self.identities.extend(delta.removed.iter().copied());
        self.identities.extend(delta.added.keys().copied());
        for id in &delta.removed {
            if let Some(location) = self.records.remove(id) {
                self.remove(&read_op_at(root, location)?);
            }
        }
        self.remember_conflicts(root, &delta.removed.iter().copied().collect::<Vec<_>>())?;
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
        self.remember(op);
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

    fn remember_conflicts(&mut self, root: &Path, ids: &[OpId]) -> io::Result<()> {
        for id in ids {
            let locations: Vec<_> = self.tail.chain().record_locations(*id).collect();
            for location in locations {
                self.remember(&read_op_at(root, location)?);
            }
        }
        Ok(())
    }

    fn remember(&mut self, op: &Op) {
        if let editchain_core::OpKind::Activity(record) = &op.kind {
            let _inserted = self.items.insert(record.item.0);
            self.items.extend(
                record
                    .session
                    .into_iter()
                    .chain(record.turn)
                    .chain(record.author)
                    .map(|id| id.0),
            );
            if let Some(legacy) = &record.legacy {
                let _inserted = self
                    .aliases
                    .entry(legacy.operation)
                    .or_default()
                    .insert(op.id);
                for id in &legacy.folded {
                    let _inserted = self.aliases.entry(*id).or_default().insert(op.id);
                }
            }
        }
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
            && self.items.iter().eq(expected.items.iter())
            && self.aliases.len() == expected.aliases.len()
            && self.aliases.iter().all(|(key, ids)| {
                expected
                    .aliases
                    .get(key)
                    .is_some_and(|other| ids.iter().eq(other.iter()))
            })
            && self.identities.iter().eq(expected.identities.iter())
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
