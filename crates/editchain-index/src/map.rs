//! Hash-trie buckets split as they grow, so an append never rewrites a shard
//! whose size grows with the full history.

use crate::Page;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    borrow::Borrow,
    collections::{hash_map, HashMap},
    hash::{Hash, Hasher as _},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "K: Eq + Hash + DeserializeOwned, V: DeserializeOwned"))]
enum Node<K: Eq + Hash, V> {
    Bucket(HashMap<K, V>),
    Branch(Vec<Page<Node<K, V>>>),
}

impl<K: Eq + Hash, V> Default for Node<K, V> {
    fn default() -> Self {
        Self::Bucket(HashMap::new())
    }
}

/// A hash map with lazily loaded, copy-on-write buckets. Traversal is reserved
/// for preparation/export; keyed operations visit only one trie path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "K: Eq + Hash + DeserializeOwned, V: DeserializeOwned"))]
pub struct Map<K: Eq + Hash, V> {
    root: Page<Node<K, V>>,
    len: usize,
}

impl<K: Eq + Hash, V> Default for Map<K, V> {
    fn default() -> Self {
        Self {
            root: Page::default(),
            len: 0,
        }
    }
}

// Fixed FNV-1a routing. Do not use std's deliberately unspecified DefaultHasher
// for persistent page addresses. Cache schema changes fence key-encoding changes.
struct StableHash(u64);
impl std::hash::Hasher for StableHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

fn hash(key: &(impl Hash + ?Sized)) -> u64 {
    let mut state = StableHash(0xcbf2_9ce4_8422_2325);
    key.hash(&mut state);
    state.finish()
}

fn slot(hash: u64, depth: u32) -> usize {
    usize::try_from(hash.checked_shr(depth.saturating_mul(4)).unwrap_or(0) & 15).unwrap_or(0)
}

impl<K: Eq + Hash, V> Node<K, V> {
    fn bucket(&self, hash: u64, depth: u32) -> &HashMap<K, V> {
        match self {
            Self::Bucket(values) => values,
            Self::Branch(children) => children
                .get(slot(hash, depth))
                .map_or_else(invalid_branch, |child| {
                    child.bucket(hash, depth.saturating_add(1))
                }),
        }
    }
    fn bucket_mut(&mut self, hash: u64, depth: u32) -> &mut HashMap<K, V> {
        if depth < 16 && matches!(self, Self::Bucket(values) if values.len() >= 64) {
            let Self::Bucket(values) = std::mem::take(self) else {
                return invalid_branch();
            };
            let mut buckets: Vec<HashMap<K, V>> = (0..16).map(|_| HashMap::new()).collect();
            for (key, value) in values {
                if let Some(bucket) = buckets.get_mut(slot(crate::map::hash(&key), depth)) {
                    drop(bucket.insert(key, value));
                }
            }
            *self = Self::Branch(
                buckets
                    .into_iter()
                    .map(|values| Page::new(Self::Bucket(values)))
                    .collect(),
            );
        }
        match self {
            Self::Bucket(values) => values,
            Self::Branch(children) => children
                .get_mut(slot(hash, depth))
                .map_or_else(invalid_branch, |child| {
                    child.bucket_mut(hash, depth.saturating_add(1))
                }),
        }
    }
}

fn invalid_branch<T>() -> T {
    crate::page::fail(std::io::Error::other("invalid index trie branch"))
}

impl<K: Eq + Hash, V> Map<K, V> {
    /// Create an empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Number of entries without hydrating pages.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }
    /// Whether no keys are retained.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Look up one borrowed value.
    pub fn get<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.root.bucket(hash(key), 0).get(key)
    }
    /// Look up one mutable value, dirtying only its bucket and path.
    pub fn get_mut<Q: Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        self.root.bucket_mut(hash(key), 0).get_mut(key)
    }
    /// Test membership without loading unrelated buckets.
    pub fn contains_key<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(key).is_some()
    }
    /// Insert or replace a single key.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let old = self.root.bucket_mut(hash(&key), 0).insert(key, value);
        if old.is_none() {
            self.len = self.len.saturating_add(1);
        }
        old
    }
    /// Remove a key; old checkpoint pages remain immutable.
    pub fn remove<Q: Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        let old = self.root.bucket_mut(hash(key), 0).remove(key);
        if old.is_some() {
            self.len = self.len.saturating_sub(1);
        }
        old
    }
    /// Entry mutation for a single bucket.
    pub fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        Entry {
            inner: self.root.bucket_mut(hash(&key), 0).entry(key),
            len: &mut self.len,
        }
    }
    /// Drop all keys without scanning their pages.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    /// Full export traversal. Do not use this on the incremental hot path.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        let mut stack = vec![&*self.root];
        let mut bucket = None;
        std::iter::from_fn(move || loop {
            if let Some(pair) = bucket.as_mut().and_then(Iterator::next) {
                return Some(pair);
            }
            match stack.pop()? {
                Node::Bucket(values) => bucket = Some(values.iter()),
                Node::Branch(children) => stack.extend(children.iter().rev().map(|page| &**page)),
            }
        })
    }
    /// Full key traversal for preparation/export.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(key, _)| key)
    }
    /// Full value traversal for preparation/export.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }
}

/// One bucket's occupied/vacant slot, preserving the map's stored cardinality.
#[derive(Debug)]
pub struct Entry<'a, K, V> {
    inner: hash_map::Entry<'a, K, V>,
    len: &'a mut usize,
}
impl<'a, K, V> Entry<'a, K, V> {
    /// Initialize a vacant slot lazily.
    pub fn or_insert_with(self, make: impl FnOnce() -> V) -> &'a mut V {
        match self.inner {
            hash_map::Entry::Occupied(entry) => entry.into_mut(),
            hash_map::Entry::Vacant(entry) => {
                *self.len = self.len.saturating_add(1);
                entry.insert(make())
            }
        }
    }
    /// Initialize a vacant slot.
    pub fn or_insert(self, value: V) -> &'a mut V {
        self.or_insert_with(|| value)
    }
    /// Mutate an existing slot without inserting it.
    #[must_use]
    pub fn and_modify(mut self, update: impl FnOnce(&mut V)) -> Self {
        if let hash_map::Entry::Occupied(entry) = &mut self.inner {
            update(entry.get_mut());
        }
        self
    }
}
impl<'a, K, V: Default> Entry<'a, K, V> {
    /// Initialize an empty slot with its default value.
    pub fn or_default(self) -> &'a mut V {
        self.or_insert_with(V::default)
    }
}

impl<K: Eq + Hash, V> FromIterator<(K, V)> for Map<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut map = Self::new();
        for (key, value) in iter {
            drop(map.insert(key, value));
        }
        map
    }
}
