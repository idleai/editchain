use serde::{Deserialize, Serialize};

use crate::ids::OpId;

/// Parent references for causal ordering.
///
/// Operations reference their causal parents to establish a DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ParentSet<I = OpId> {
    /// No parents (root operation).
    #[default]
    None,
    /// Single parent.
    One(I),
    /// Two parents (e.g. merge of two branches).
    Two(I, I),
}

impl<I> ParentSet<I> {
    /// Returns an iterator over all referenced `OpId`s.
    #[must_use]
    pub const fn iter(&self) -> ParentIter<'_, I> {
        ParentIter {
            set: self,
            index: 0,
        }
    }
}

impl<'a, I> IntoIterator for &'a ParentSet<I> {
    type Item = &'a I;
    type IntoIter = ParentIter<'a, I>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over parent `OpId`s.
#[derive(Debug)]
pub struct ParentIter<'a, I = OpId> {
    set: &'a ParentSet<I>,
    index: usize,
}

impl<'a, I> Iterator for ParentIter<'a, I> {
    type Item = &'a I;

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
