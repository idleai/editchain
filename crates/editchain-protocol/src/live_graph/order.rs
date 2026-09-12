//! Monotone causal clocks. Only a new constraint and its affected descendants
//! move; appending an ordinary leaf never re-sorts the historical graph.

use super::{LiveBlockMeta, LiveGraph};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

impl LiveGraph {
    /// Retain previous positions and raise a child's ordering clock only when
    /// needed to keep every present parent below it. Display timestamps stay
    /// on the physical rows and are never changed by this scheduling clock.
    ///
    /// # Errors
    /// Returns an error for a causal cycle or exhausted ordering clock.
    pub fn causal_updates(&self, upserts: &[LiveBlockMeta]) -> Result<Vec<LiveBlockMeta>, String> {
        let mut staged: BTreeMap<_, _> = upserts
            .iter()
            .map(|node| (node.key.clone(), node.clone()))
            .collect();
        let mut pending: VecDeque<_> = self.validate_causal_updates(&staged)?.into();
        let mut queued: BTreeSet<_> = pending.iter().cloned().collect();
        let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for node in staged.values_mut() {
            if let Some(old) = self.nodes.get(&node.key) {
                node.sort_time = node.sort_time.max(old.sort_time);
            }
            for parent in &node.parents {
                let _: bool = children
                    .entry(parent.clone())
                    .or_default()
                    .insert(node.key.clone());
            }
        }
        while let Some(key) = pending.pop_front() {
            let _: bool = queued.remove(&key);
            let Some(mut node) = staged.get(&key).cloned() else {
                continue;
            };
            for parent in &node.parents {
                if let Some(parent) = staged.get(parent).or_else(|| self.nodes.get(parent)) {
                    node.sort_time = node.sort_time.max(
                        parent
                            .sort_time
                            .checked_add(1)
                            .ok_or("causal ordering clock exhausted")?,
                    );
                }
            }
            drop(staged.insert(key.clone(), node.clone()));
            let followers: BTreeSet<_> = self
                .incoming
                .get(&key)
                .into_iter()
                .flatten()
                .chain(children.get(&key).into_iter().flatten())
                .cloned()
                .collect();
            for child in followers {
                let Some(child) = staged.get(&child).or_else(|| self.nodes.get(&child)) else {
                    continue;
                };
                if child.parents.contains(&key) && child.sort_time <= node.sort_time {
                    let child = child.clone();
                    if queued.insert(child.key.clone()) {
                        pending.push_back(child.key.clone());
                    }
                    drop(staged.insert(child.key.clone(), child));
                }
            }
        }
        Ok(staged.into_values().collect())
    }

    fn validate_causal_updates(
        &self,
        staged: &BTreeMap<String, LiveBlockMeta>,
    ) -> Result<Vec<String>, String> {
        // New leaves cannot close a cycle through retained nodes. Repairs to
        // old edges or previously missing endpoints also inspect old ancestry.
        let repair = staged.values().any(|node| {
            self.nodes.get(&node.key).map_or_else(
                || {
                    self.incoming
                        .get(&node.key)
                        .is_some_and(|children| !children.is_empty())
                },
                |old| old.parents != node.parents,
            )
        });
        let mut done = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        let mut order = Vec::new();
        for key in staged.keys() {
            let mut stack = vec![(key.clone(), false)];
            while let Some((key, finish)) = stack.pop() {
                if done.contains(&key) {
                    continue;
                }
                if finish {
                    let _: bool = visiting.remove(&key);
                    if staged.contains_key(&key) {
                        order.push(key.clone());
                    }
                    let _: bool = done.insert(key);
                    continue;
                }
                let Some(node) = staged
                    .get(&key)
                    .or_else(|| repair.then(|| self.nodes.get(&key)).flatten())
                else {
                    continue;
                };
                if !visiting.insert(key.clone()) {
                    return Err(format!("causal history cycle at {key}"));
                }
                stack.push((key, true));
                stack.extend(node.parents.iter().map(|parent| (parent.clone(), false)));
            }
        }
        Ok(order)
    }
}
