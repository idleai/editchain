use serde::{Deserialize, Serialize};

use crate::ids::OpId;
use crate::payload::BlobRef;

/// Parent references for causal ordering.
///
/// Operations reference their causal parents to establish a DAG.
/// `Many` uses a blob reference for large parent sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParentSet {
    /// No parents (root operation).
    None,
    /// Single parent.
    One(OpId),
    /// Two parents (e.g. merge of two branches).
    Two(OpId, OpId),
    /// Many parents stored as an external blob.
    Many(BlobRef),
}

impl Default for ParentSet {
    fn default() -> Self {
        ParentSet::None
    }
}

impl ParentSet {
    /// Returns an iterator over all referenced OpIds.
    pub fn iter(&self) -> ParentIter<'_> {
        ParentIter {
            set: self,
            index: 0,
        }
    }
}

/// Iterator over parent OpIds.
pub struct ParentIter<'a> {
    set: &'a ParentSet,
    index: usize,
}

impl<'a> Iterator for ParentIter<'a> {
    type Item = &'a OpId;

    fn next(&mut self) -> Option<Self::Item> {
        match (self.set, self.index) {
            (ParentSet::None, _) => None,
            (ParentSet::One(a), 0) => {
                self.index = 1;
                Some(a)
            }
            (ParentSet::Two(a, _b), 0) => {
                self.index = 1;
                Some(a)
            }
            (ParentSet::Two(_, b), 1) => {
                self.index = 2;
                Some(b)
            }
            (ParentSet::Many(_), _) => None, // external blob — not iterated inline
            _ => None,
        }
    }
}