//! Ordered index adapters over the persistent rank tree.

use crate::rank::{Measure, RankTree};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    borrow::Borrow,
    ops::{Bound, RangeBounds},
};

/// A copy-on-write ordered map with logarithmic range seeks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "K: DeserializeOwned, V: DeserializeOwned"))]
pub struct OrderedMap<K, V>(RankTree<K, V>);

impl<K, V> Default for OrderedMap<K, V> {
    fn default() -> Self {
        Self(RankTree::default())
    }
}

impl<K: Ord + Clone, V> OrderedMap<K, V> {
    /// Create an empty ordered index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Stored cardinality, without a scan.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::try_from(self.0.measure().expanded).unwrap_or(usize::MAX)
    }
    /// Test whether this index has no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Read one value.
    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.0.get(key)
    }
    /// Mutate one value.
    pub fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        self.0.get_mut(key)
    }
    /// Test membership.
    pub fn contains_key<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(key).is_some()
    }
    /// Insert/replace one value.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.0.insert(
            key,
            value,
            Measure {
                expanded: 1,
                visible: 1,
            },
        )
    }
    /// Remove a key.
    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        self.0.remove(key)
    }
    /// First entry.
    pub fn first_key_value(&self) -> Option<(&K, &V)> {
        self.0.bound(Bound::Unbounded, false)
    }
    /// Last entry.
    pub fn last_key_value(&self) -> Option<(&K, &V)> {
        self.0.bound(Bound::Unbounded, true)
    }
    /// Mutable entry initializer.
    pub fn entry(&mut self, key: K) -> OrderedEntry<'_, K, V> {
        OrderedEntry { map: self, key }
    }
    /// Discard all keys without loading them.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    /// Ordered traversal for preparation and export.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&K, &V)> {
        self.range(..)
    }
    /// Traverse a bounded range, starting with logarithmic seeks.
    pub fn range(&self, bounds: impl RangeBounds<K>) -> Range<'_, K, V> {
        Range {
            tree: &self.0,
            start: bounds.start_bound().cloned(),
            end: bounds.end_bound().cloned(),
        }
    }
    /// Value traversal.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }
    /// Key traversal.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(key, _)| key)
    }
}

/// Entry initializer for an ordered index.
#[derive(Debug)]
pub struct OrderedEntry<'a, K, V> {
    map: &'a mut OrderedMap<K, V>,
    key: K,
}
impl<'a, K: Ord + Clone, V> OrderedEntry<'a, K, V> {
    /// Initialize a vacant slot lazily.
    pub fn or_insert_with(self, make: impl FnOnce() -> V) -> &'a mut V {
        if !self.map.contains_key(&self.key) {
            drop(self.map.insert(self.key.clone(), make()));
        }
        self.map.get_mut(&self.key).unwrap_or_else(|| {
            crate::page::fail(std::io::Error::other("missing inserted index entry"))
        })
    }
    /// Initialize a vacant slot.
    pub fn or_insert(self, value: V) -> &'a mut V {
        self.or_insert_with(|| value)
    }
}
impl<'a, K: Ord + Clone, V: Default> OrderedEntry<'a, K, V> {
    /// Initialize a vacant slot with its default.
    pub fn or_default(self) -> &'a mut V {
        self.or_insert_with(V::default)
    }
}

/// A range cursor that hydrates only the pages it crosses.
#[derive(Debug)]
pub struct Range<'a, K, V> {
    tree: &'a RankTree<K, V>,
    start: Bound<K>,
    end: Bound<K>,
}
impl<'a, K: Ord + Clone, V> Iterator for Range<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        let pair = self.tree.bound(self.start.as_ref(), false)?;
        if !below(pair.0, &self.end) {
            return None;
        }
        self.start = Bound::Excluded(pair.0.clone());
        Some(pair)
    }
}
impl<K: Ord + Clone, V> DoubleEndedIterator for Range<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let pair = self.tree.bound(self.end.as_ref(), true)?;
        let accepted = match &self.start {
            Bound::Unbounded => true,
            Bound::Included(key) => pair.0 >= key,
            Bound::Excluded(key) => pair.0 > key,
        };
        if !accepted {
            return None;
        }
        self.end = Bound::Excluded(pair.0.clone());
        Some(pair)
    }
}
fn below<K: Ord>(key: &K, end: &Bound<K>) -> bool {
    match end {
        Bound::Unbounded => true,
        Bound::Included(end) => key <= end,
        Bound::Excluded(end) => key < end,
    }
}

/// An ordered set whose elements live in independently persisted tree pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "K: DeserializeOwned"))]
pub struct OrderedSet<K>(OrderedMap<K, ()>);
impl<K> Default for OrderedSet<K> {
    fn default() -> Self {
        Self(OrderedMap::default())
    }
}
impl<K: Ord + Clone> OrderedSet<K> {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Cardinality without traversal.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Insert one key.
    pub fn insert(&mut self, key: K) -> bool {
        self.0.insert(key, ()).is_none()
    }
    /// Remove one key.
    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.0.remove(key).is_some()
    }
    /// Test membership.
    pub fn contains<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.0.contains_key(key)
    }
    /// First key.
    pub fn first(&self) -> Option<&K> {
        self.0.first_key_value().map(|(key, ())| key)
    }
    /// Last key.
    pub fn last(&self) -> Option<&K> {
        self.0.last_key_value().map(|(key, ())| key)
    }
    /// Ordered traversal.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &K> {
        self.0.iter().map(|(key, ())| key)
    }
    /// Seek a range of keys.
    pub fn range(&self, bounds: impl RangeBounds<K>) -> impl DoubleEndedIterator<Item = &K> {
        self.0.range(bounds).map(|(key, ())| key)
    }
    /// Move the suffix beginning at a key into a new set.
    #[must_use]
    pub fn split_off(&mut self, at: &K) -> Self {
        let keys: Vec<_> = self.range(at.clone()..).cloned().collect();
        for key in &keys {
            let _removed = self.remove(key);
        }
        keys.into_iter().collect()
    }
    /// Empty the set without a traversal.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}
impl<K: Ord + Clone> FromIterator<K> for OrderedSet<K> {
    fn from_iter<T: IntoIterator<Item = K>>(iter: T) -> Self {
        let mut set = Self::new();
        set.extend(iter);
        set
    }
}
impl<K: Ord + Clone> Extend<K> for OrderedSet<K> {
    fn extend<T: IntoIterator<Item = K>>(&mut self, iter: T) {
        for key in iter {
            let _inserted = self.insert(key);
        }
    }
}

impl<'a, K: Ord + Clone> IntoIterator for &'a OrderedSet<K> {
    type Item = &'a K;
    type IntoIter = Box<dyn Iterator<Item = &'a K> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<'a, K: Ord + Clone, V> IntoIterator for &'a OrderedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Range<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.range(..)
    }
}
