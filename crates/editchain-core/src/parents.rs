use serde::{Deserialize, Serialize};

use crate::ids::OpId;

/// Parent references for causal ordering.
///
/// Operations reference their causal parents to establish a DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ParentSet {
    /// No parents (root operation).
    #[default]
    None,
    /// Single parent.
    One(OpId),
    /// Two parents (e.g. merge of two branches).
    Two(OpId, OpId),
}

impl ParentSet {
    /// Returns an iterator over all referenced `OpId`s.
    #[must_use]
    pub const fn iter(&self) -> ParentIter<'_> {
        ParentIter {
            set: self,
            index: 0,
        }
    }
}

impl<'a> IntoIterator for &'a ParentSet {
    type Item = &'a OpId;
    type IntoIter = ParentIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over parent `OpId`s.
#[derive(Debug)]
pub struct ParentIter<'a> {
    set: &'a ParentSet,
    index: usize,
}

impl<'a> Iterator for ParentIter<'a> {
    type Item = &'a OpId;

    fn next(&mut self) -> Option<Self::Item> {
        match (self.set, self.index) {
            (ParentSet::One(a) | ParentSet::Two(a, _), 0) => {
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
