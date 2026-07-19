use serde::{Deserialize, Serialize};

use crate::ids::OpId;

/// Parent references for causal ordering.
///
/// Operations reference their causal parents to establish a DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum ParentSet {
    /// No parents (root operation).
    #[default]
    None,
    /// Single parent.
    One(OpId),
    /// Two parents (e.g. merge of two branches).
    Two(OpId, OpId),
    // /// Many parents stored as an external blob.
    // /// Commented out — blob resolution layer not yet implemented.
    // Many(BlobRef),
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
            _ => None,
        }
    }
}