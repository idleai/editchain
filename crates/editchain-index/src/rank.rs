//! A copy-on-write AVL sequence with lazy pages and two subtree weights.

use crate::Page;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::borrow::Borrow;

/// Expanded and currently visible row counts for a presentation block.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Measure {
    /// Fully expanded slots.
    pub expanded: u64,
    /// Slots remaining after local disclosure.
    pub visible: u64,
}

impl Measure {
    fn plus(self, other: Self) -> Self {
        Self {
            expanded: self.expanded.saturating_add(other.expanded),
            visible: self.visible.saturating_add(other.visible),
        }
    }
    fn along(self, axis: Axis) -> u64 {
        match axis {
            Axis::Expanded => self.expanded,
            Axis::Visible => self.visible,
        }
    }
}

/// Coordinate space used by rank/select queries.
#[derive(Debug, Clone, Copy)]
pub enum Axis {
    /// Fully expanded history slots.
    Expanded,
    /// Currently visible history slots.
    Visible,
}

type Link<K, V> = Option<Page<Node<K, V>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "K: DeserializeOwned, V: DeserializeOwned"))]
struct Node<K, V> {
    key: K,
    value: V,
    weight: Measure,
    sum: Measure,
    height: u32,
    left: Link<K, V>,
    right: Link<K, V>,
}

impl<K, V> Node<K, V> {
    fn fix(&mut self) {
        self.height = height(&self.left)
            .max(height(&self.right))
            .saturating_add(1);
        self.sum = measure(&self.left)
            .plus(self.weight)
            .plus(measure(&self.right));
    }
}

/// Keyed, weighted sequence. All edits and rank/select queries are logarithmic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "K: DeserializeOwned, V: DeserializeOwned"))]
pub struct RankTree<K, V> {
    root: Link<K, V>,
}

impl<K, V> Default for RankTree<K, V> {
    fn default() -> Self {
        Self { root: None }
    }
}

impl<K: Ord, V> RankTree<K, V> {
    /// Aggregate row counts, available without walking the sequence.
    #[must_use]
    pub fn measure(&self) -> Measure {
        measure(&self.root)
    }

    /// Insert or replace one block, returning any old value.
    pub fn insert(&mut self, key: K, value: V, weight: Measure) -> Option<V> {
        let (root, previous) = insert(self.root.take(), key, value, weight);
        self.root = Some(root);
        previous
    }

    /// Remove a block without changing the identity of any later block.
    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        let (root, previous) = remove(self.root.take(), key);
        self.root = root;
        previous
    }

    /// Find a value by stable order key.
    #[must_use]
    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        let mut next = self.root.as_deref();
        while let Some(node) = next {
            match key.cmp(node.key.borrow()) {
                std::cmp::Ordering::Less => next = node.left.as_deref(),
                std::cmp::Ordering::Greater => next = node.right.as_deref(),
                std::cmp::Ordering::Equal => return Some(&node.value),
            }
        }
        None
    }

    /// Mutate one value without changing its subtree weight.
    pub fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        let mut next = self.root.as_deref_mut();
        while let Some(node) = next {
            match key.cmp(node.key.borrow()) {
                std::cmp::Ordering::Less => next = node.left.as_deref_mut(),
                std::cmp::Ordering::Greater => next = node.right.as_deref_mut(),
                std::cmp::Ordering::Equal => return Some(&mut node.value),
            }
        }
        None
    }

    /// First key satisfying a lower bound, or last key satisfying an upper bound.
    pub fn bound(&self, bound: std::ops::Bound<&K>, reverse: bool) -> Option<(&K, &V)> {
        use std::ops::Bound;
        let mut next = self.root.as_deref();
        let mut result = None;
        while let Some(node) = next {
            let accepted = match bound {
                Bound::Unbounded => true,
                Bound::Included(key) => {
                    if reverse {
                        &node.key <= key
                    } else {
                        &node.key >= key
                    }
                }
                Bound::Excluded(key) => {
                    if reverse {
                        &node.key < key
                    } else {
                        &node.key > key
                    }
                }
            };
            if accepted {
                result = Some((&node.key, &node.value));
            }
            next = if accepted == reverse {
                node.right.as_deref()
            } else {
                node.left.as_deref()
            };
        }
        result
    }

    /// Prefix weights before a named block.
    #[must_use]
    pub fn rank(&self, key: &K) -> Option<Measure> {
        let mut sum = Measure::default();
        let mut next = self.root.as_deref();
        while let Some(node) = next {
            match key.cmp(&node.key) {
                std::cmp::Ordering::Less => next = node.left.as_deref(),
                std::cmp::Ordering::Greater => {
                    sum = sum.plus(measure(&node.left)).plus(node.weight);
                    next = node.right.as_deref();
                }
                std::cmp::Ordering::Equal => return Some(sum.plus(measure(&node.left))),
            }
        }
        None
    }

    /// Prefix weights strictly before any key, including absent keys.
    #[must_use]
    pub fn prefix(&self, key: &K) -> Measure {
        let mut sum = Measure::default();
        let mut next = self.root.as_deref();
        while let Some(node) = next {
            if key <= &node.key {
                next = node.left.as_deref();
            } else {
                sum = sum.plus(measure(&node.left)).plus(node.weight);
                next = node.right.as_deref();
            }
        }
        sum
    }

    /// The block containing an offset and its prefix weights in both spaces.
    #[must_use]
    pub fn select(&self, offset: u64, axis: Axis) -> Option<(&K, &V, Measure)> {
        let mut sum = Measure::default();
        let mut next = self.root.as_deref();
        while let Some(node) = next {
            let start = sum.plus(measure(&node.left));
            if offset < start.along(axis) {
                next = node.left.as_deref();
            } else if offset < start.plus(node.weight).along(axis) {
                return Some((&node.key, &node.value, start));
            } else {
                sum = start.plus(node.weight);
                next = node.right.as_deref();
            }
        }
        None
    }

    /// Ordered traversal, used only for bootstrap/export, never for an edit.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        let mut stack = Vec::new();
        let mut next = self.root.as_deref();
        std::iter::from_fn(move || {
            while let Some(node) = next {
                stack.push(node);
                next = node.left.as_deref();
            }
            let node = stack.pop()?;
            next = node.right.as_deref();
            Some((&node.key, &node.value))
        })
    }
}

fn height<K, V>(link: &Link<K, V>) -> u32 {
    link.as_ref().map_or(0, |node| node.height)
}
fn measure<K, V>(link: &Link<K, V>) -> Measure {
    link.as_ref().map_or(Measure::default(), |node| node.sum)
}

fn left<K, V>(root: &mut Page<Node<K, V>>) {
    let Some(mut pivot) = root.right.take() else {
        return;
    };
    std::mem::swap(root, &mut pivot);
    pivot.right = root.left.take();
    pivot.fix();
    root.left = Some(pivot);
    root.fix();
}

fn right<K, V>(root: &mut Page<Node<K, V>>) {
    let Some(mut pivot) = root.left.take() else {
        return;
    };
    std::mem::swap(root, &mut pivot);
    pivot.left = root.right.take();
    pivot.fix();
    root.right = Some(pivot);
    root.fix();
}

fn balance<K, V>(root: &mut Page<Node<K, V>>) {
    root.fix();
    if height(&root.left) > height(&root.right).saturating_add(1) {
        if let Some(child) = &mut root.left {
            if height(&child.right) > height(&child.left) {
                left(child);
            }
        }
        right(root);
    } else if height(&root.right) > height(&root.left).saturating_add(1) {
        if let Some(child) = &mut root.right {
            if height(&child.left) > height(&child.right) {
                right(child);
            }
        }
        left(root);
    }
}

fn insert<K: Ord, V>(
    link: Link<K, V>,
    key: K,
    value: V,
    weight: Measure,
) -> (Page<Node<K, V>>, Option<V>) {
    let Some(mut node) = link else {
        return (
            Page::new(Node {
                key,
                value,
                weight,
                sum: weight,
                height: 1,
                left: None,
                right: None,
            }),
            None,
        );
    };
    let previous = match key.cmp(&node.key) {
        std::cmp::Ordering::Less => {
            let (child, previous) = insert(node.left.take(), key, value, weight);
            node.left = Some(child);
            previous
        }
        std::cmp::Ordering::Greater => {
            let (child, previous) = insert(node.right.take(), key, value, weight);
            node.right = Some(child);
            previous
        }
        std::cmp::Ordering::Equal => {
            node.weight = weight;
            Some(std::mem::replace(&mut node.value, value))
        }
    };
    balance(&mut node);
    (node, previous)
}

type Extraction<K, V> = (Link<K, V>, Page<Node<K, V>>);

fn pop_min<K, V>(mut node: Page<Node<K, V>>) -> Extraction<K, V> {
    let Some(child) = node.left.take() else {
        return (node.right.take(), node);
    };
    let (child, smallest) = pop_min(child);
    node.left = child;
    balance(&mut node);
    (Some(node), smallest)
}

fn remove<K: Ord + Borrow<Q>, V, Q: Ord + ?Sized>(
    link: Link<K, V>,
    key: &Q,
) -> (Link<K, V>, Option<V>) {
    let Some(mut node) = link else {
        return (None, None);
    };
    let previous = match key.cmp(node.key.borrow()) {
        std::cmp::Ordering::Less => {
            let (child, old) = remove(node.left.take(), key);
            node.left = child;
            old
        }
        std::cmp::Ordering::Greater => {
            let (child, old) = remove(node.right.take(), key);
            node.right = child;
            old
        }
        std::cmp::Ordering::Equal => {
            let Some(child) = node.right.take() else {
                return (node.left.take(), Some(node.into_inner().value));
            };
            let (child, mut successor) = pop_min(child);
            successor.left = node.left.take();
            successor.right = child;
            balance(&mut successor);
            return (Some(successor), Some(node.into_inner().value));
        }
    };
    balance(&mut node);
    (Some(node), previous)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn mixed_edits_and_both_coordinate_spaces_match_a_sorted_oracle() {
        let mut tree = RankTree::default();
        let mut oracle = BTreeMap::new();
        let mut state = 42_u64;
        for step in 0..4000_u64 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let key = state.rem_euclid(113);
            if step.rem_euclid(4) == 0 {
                assert_eq!(tree.remove(&key), oracle.remove(&key));
            } else {
                let weight = Measure {
                    expanded: key.rem_euclid(9).saturating_add(1),
                    visible: key.rem_euclid(4),
                };
                assert_eq!(tree.insert(key, weight, weight), oracle.insert(key, weight));
            }
            let mut prefix = Measure::default();
            for (key, weight) in &oracle {
                assert_eq!(tree.rank(key), Some(prefix));
                for axis in [Axis::Expanded, Axis::Visible] {
                    for slot in 0..weight.along(axis) {
                        assert_eq!(
                            tree.select(prefix.along(axis).saturating_add(slot), axis),
                            Some((key, weight, prefix))
                        );
                    }
                }
                prefix = prefix.plus(*weight);
            }
            assert_eq!(tree.measure(), prefix);
            assert_eq!(tree.select(prefix.expanded, Axis::Expanded), None);
            assert_eq!(
                tree.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
                oracle.keys().copied().collect::<Vec<_>>()
            );
            assert!(height(&tree.root) <= 10);
        }
    }
}
