//! Canonical admission of immutable operation bytes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::OpId;

/// Outcome of adding one record to the evidence set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The ID has exactly one known byte representation and is accepted.
    Accepted,
    /// These exact bytes were already retained, including for a conflicted ID.
    Duplicate,
    /// New conflicting bytes were retained; all versions of the ID are inert.
    Conflict,
}

/// Grow-only evidence keyed by operation identity and exact encoded bytes.
///
/// An ID is accepted only while it has exactly one distinct representation.
/// Conflicts retain every version and exclude the entire ID from the accepted
/// view. Evidence survives merges, including merges into an empty replica.
/// Replaying any previously seen version cannot restore a conflicted ID.
///
/// This in-memory state is separate from the persisted [`crate::Op`] schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpSet {
    evidence: BTreeMap<OpId, BTreeSet<Vec<u8>>>,
}

impl OpSet {
    /// Create an empty evidence set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            evidence: BTreeMap::new(),
        }
    }

    /// Retain one distinct record and update its ID's admission status.
    pub fn insert(&mut self, id: OpId, encoded: Vec<u8>) -> Admission {
        let variants = self.evidence.entry(id).or_default();
        if !variants.insert(encoded) {
            Admission::Duplicate
        } else if variants.len() == 1 {
            Admission::Accepted
        } else {
            Admission::Conflict
        }
    }

    /// Whether the ID is accepted (exactly one known representation).
    #[must_use]
    pub fn contains(&self, id: &OpId) -> bool {
        self.evidence
            .get(id)
            .is_some_and(|variants| variants.len() == 1)
    }

    /// Number of accepted operation identities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Whether there are no accepted operations; conflict evidence may remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    /// Accepted ID/byte pairs in deterministic key order.
    pub fn iter(&self) -> impl Iterator<Item = (&OpId, &[u8])> {
        self.evidence.iter().filter_map(|(id, variants)| {
            if variants.len() == 1 {
                variants.first().map(|bytes| (id, bytes.as_slice()))
            } else {
                None
            }
        })
    }

    /// Every distinct record, accepted or conflicted, in ID/byte order.
    pub fn evidence(&self) -> impl Iterator<Item = (&OpId, &[u8])> {
        self.evidence
            .iter()
            .flat_map(|(id, variants)| variants.iter().map(move |bytes| (id, bytes.as_slice())))
    }

    /// Conflicted IDs with all retained byte representations in sorted order.
    pub fn conflicts(&self) -> impl Iterator<Item = (&OpId, &BTreeSet<Vec<u8>>)> {
        self.evidence
            .iter()
            .filter(|(_, variants)| variants.len() > 1)
    }

    /// Merge all evidence by set union, including every quarantined variant.
    ///
    /// Returns insertion outcomes `(accepted, duplicates, conflicts)`. A later
    /// conflict in this merge can invalidate an earlier acceptance; [`Self::len`]
    /// reports the final accepted count.
    pub fn merge(&mut self, other: &Self) -> (usize, usize, usize) {
        let mut counts = (0usize, 0usize, 0usize);
        for (id, bytes) in other.evidence() {
            match self.insert(*id, bytes.to_vec()) {
                Admission::Accepted => counts.0 = counts.0.saturating_add(1),
                Admission::Duplicate => counts.1 = counts.1.saturating_add(1),
                Admission::Conflict => counts.2 = counts.2.saturating_add(1),
            }
        }
        counts
    }
}

impl Default for OpSet {
    fn default() -> Self {
        Self::new()
    }
}
