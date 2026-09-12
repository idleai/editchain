//! Most reverse dependencies have one user. Allocate a set only for fan-out.

use editchain_core::OpId;
use std::collections::{hash_set, HashSet};

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) enum Neighbors {
    #[default]
    Empty,
    One(OpId),
    Many(HashSet<OpId>),
}

impl Neighbors {
    pub(super) fn insert(&mut self, id: OpId) -> bool {
        match self {
            Self::Empty => {
                *self = Self::One(id);
                true
            }
            Self::One(previous) if *previous == id => false,
            Self::One(previous) => {
                *self = Self::Many(HashSet::from([*previous, id]));
                true
            }
            Self::Many(ids) => ids.insert(id),
        }
    }
}

pub(super) enum Iter<'a> {
    One(std::option::IntoIter<&'a OpId>),
    Many(hash_set::Iter<'a, OpId>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a OpId;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::One(ids) => ids.next(),
            Self::Many(ids) => ids.next(),
        }
    }
}

impl<'a> IntoIterator for &'a Neighbors {
    type Item = &'a OpId;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        match self {
            Neighbors::Empty => Iter::One(None.into_iter()),
            Neighbors::One(id) => Iter::One(Some(id).into_iter()),
            Neighbors::Many(ids) => Iter::Many(ids.iter()),
        }
    }
}
