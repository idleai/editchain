//! Fixed filtering for the VS Code Activity view.
//!
//! Timestamp-less metadata and semantic trace rows are omitted. Parent edges
//! are spliced across omitted rows so the visible graph stays connected, while
//! dated structural relationship endpoints remain visible.

use std::collections::HashSet;

use crate::taxonomy::Visibility;
use crate::{HistoryNode, NodeKey, ResolvedGraph};

/// Apply the fixed Activity-view visibility rules to an ordered node list.
#[must_use]
pub(crate) fn apply(
    nodes: Vec<HistoryNode>,
    graph: &ResolvedGraph,
    structural_keys: &HashSet<NodeKey>,
) -> Vec<HistoryNode> {
    if nodes.is_empty() {
        return nodes;
    }

    let hidden: HashSet<NodeKey> = nodes
        .iter()
        .filter(|node| {
            node.timestamp_ms() == 0
                || (node.visibility() == Visibility::Trace
                    && !structural_keys.contains(&node.key()))
        })
        .map(HistoryNode::key)
        .collect();

    let mut result = Vec::with_capacity(nodes.len());
    for mut node in nodes {
        let key = node.key();
        if hidden.contains(&key) {
            continue;
        }
        let parents = if hidden.is_empty() {
            graph.parents(key).to_vec()
        } else {
            nearest_kept_ancestors(key, graph, &hidden)
        };
        node.override_parents(&parents);
        result.push(node);
    }
    result
}

/// Find nearest visible ancestors iteratively, preserving depth-first parent
/// order while bounding stack use on long hidden chains and malformed cycles.
#[must_use]
fn nearest_kept_ancestors(
    key: NodeKey,
    graph: &ResolvedGraph,
    hidden: &HashSet<NodeKey>,
) -> Vec<NodeKey> {
    let mut output = Vec::new();
    let mut seen_output = HashSet::new();
    let mut visited = HashSet::from([key]);
    let mut pending: Vec<NodeKey> = graph.parents(key).iter().rev().copied().collect();
    while let Some(parent) = pending.pop() {
        if !hidden.contains(&parent) {
            if parent != key && seen_output.insert(parent) {
                output.push(parent);
            }
        } else if visited.insert(parent) {
            pending.extend(graph.parents(parent).iter().rev().copied());
        }
    }
    output
}
