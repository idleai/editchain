//! Content dependencies are keyed by both recorded address and optional length.

use std::{collections::BTreeSet, io};

use editchain_core::{BlobRef, ContentId, OpId};
use editchain_store::{BlobResolution, BlobSource};
use serde::{Deserialize, Serialize};

use crate::{Map, OrderedSet};

/// An immutable content reference, retaining its recorded length when present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentReference {
    /// Exact recorded identity, including local or truncated identities.
    pub id: ContentId,
    /// Declared blob length; file base/after identities do not carry a length.
    pub len: Option<u32>,
}

impl From<BlobRef> for ContentReference {
    fn from(reference: BlobRef) -> Self {
        Self {
            id: reference.id,
            len: Some(reference.len),
        }
    }
}

impl From<ContentId> for ContentReference {
    fn from(id: ContentId) -> Self {
        Self { id, len: None }
    }
}

impl ContentReference {
    /// Resolve exact bytes through a storage adapter, preserving IO errors.
    ///
    /// # Errors
    /// Returns backend access errors; absence and corruption are explicit results.
    pub fn resolve(self, source: &impl BlobSource) -> io::Result<BlobResolution> {
        match self.len {
            Some(len) => source.read_blob(&BlobRef { id: self.id, len }),
            None => source.read_content(self.id),
        }
    }

    pub(crate) fn state(self, source: &impl BlobSource) -> io::Result<ContentState> {
        Ok(match self.resolve(source)? {
            BlobResolution::Found(_) => ContentState::Available,
            BlobResolution::Missing => ContentState::Missing,
            BlobResolution::Corrupt => ContentState::Corrupt,
            BlobResolution::Unresolvable => ContentState::Unresolvable,
        })
    }
}

/// Verified availability at the index's last successful refresh or rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentState {
    /// Full address and any recorded length were verified.
    Available,
    /// Referenced content has not arrived.
    Missing,
    /// Stored bytes do not match their address or recorded length.
    Corrupt,
    /// The storage adapter cannot resolve this identity.
    Unresolvable,
}

/// Availability of one reference; no content identity is substituted or inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentStatus {
    /// Original reference.
    pub reference: ContentReference,
    /// Most recently verified availability.
    pub state: ContentState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Dependency {
    state: ContentState,
    owners: OrderedSet<OpId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ContentIndex {
    entries: Map<ContentReference, Dependency>,
    pending: Map<ContentReference, ()>,
}

impl ContentIndex {
    pub(crate) fn add(
        &mut self,
        reference: ContentReference,
        owner: OpId,
        source: &impl BlobSource,
        reads: &mut u64,
    ) -> io::Result<()> {
        if !self.entries.contains_key(&reference) {
            let state = reference.state(source)?;
            *reads = reads.saturating_add(1);
            drop(self.entries.insert(
                reference,
                Dependency {
                    state,
                    owners: OrderedSet::new(),
                },
            ));
            if state != ContentState::Available {
                let _old = self.pending.insert(reference, ());
            }
        }
        if let Some(entry) = self.entries.get_mut(&reference) {
            let _inserted = entry.owners.insert(owner);
        }
        Ok(())
    }

    pub(crate) fn remove(&mut self, reference: &ContentReference, owner: OpId) {
        if let Some(entry) = self.entries.get_mut(reference) {
            let _removed = entry.owners.remove(&owner);
            if entry.owners.is_empty() {
                drop(self.entries.remove(reference));
                let _removed = self.pending.remove(reference);
            }
        }
    }

    pub(crate) fn status(&self, reference: ContentReference) -> Option<ContentStatus> {
        self.entries.get(&reference).map(|entry| ContentStatus {
            reference,
            state: entry.state,
        })
    }

    pub(crate) fn refresh(
        &mut self,
        source: &impl BlobSource,
        reads: &mut u64,
    ) -> io::Result<BTreeSet<OpId>> {
        let mut changed = BTreeSet::new();
        let pending: Vec<_> = self.pending.keys().copied().collect();
        for reference in pending {
            let state = reference.state(source)?;
            *reads = reads.saturating_add(1);
            if let Some(entry) = self.entries.get_mut(&reference) {
                if entry.state != state {
                    entry.state = state;
                    changed.extend(entry.owners.iter().copied());
                }
            }
            if state == ContentState::Available {
                let _removed = self.pending.remove(&reference);
            }
        }
        Ok(changed)
    }

    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self.entries.iter().all(|(reference, entry)| {
                other.entries.get(reference).is_some_and(|expected| {
                    entry.state == expected.state && entry.owners.iter().eq(expected.owners.iter())
                })
            })
            && self.pending.len() == other.pending.len()
            && self
                .pending
                .keys()
                .all(|reference| other.pending.contains_key(reference))
    }
}
