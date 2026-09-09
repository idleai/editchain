//! Fixed filtering for the VS Code Activity view.
//!
//! Timestamp-less metadata and semantic trace rows are omitted. Parent edges
//! are spliced across omitted rows so the visible graph stays connected, while
//! dated structural relationship endpoints remain visible.

use std::collections::{HashMap, HashSet};

use editchain_core::{Op, OpId};

use crate::taxonomy::Visibility;
use crate::HistoryNode;

/// Apply the fixed Activity-view visibility rules to an ordered node list.
#[must_use]
pub(crate) fn apply(
    nodes: Vec<HistoryNode>,
    links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
    note_map: &HashMap<OpId, Vec<Op>>,
    representative: &HashMap<OpId, OpId>,
    structural_keys: &HashSet<String>,
) -> Vec<HistoryNode> {
    if nodes.is_empty() {
        return nodes;
    }

    let present = crate::row_node_keys(&nodes);
    // The revision-44 fixed view preserves a dated ProducedBy source even
    // when its Git target cannot be resolved. Activity bundling protects only
    // resolved topology, so extend its structural keys for visibility here.
    let mut structural_keys = structural_keys.clone();
    structural_keys.extend(
        links
            .values()
            .flatten()
            .filter(|link| link.kind == editchain_core::GitLinkKind::ProducedBy)
            .filter_map(|link| {
                crate::canonical_parent_key(&link.source.to_string(), representative, &present)
            }),
    );
    let mut parents_of_key = HashMap::with_capacity(nodes.len());
    for node in &nodes {
        let key = node.node_key();
        let parents = crate::canonicalize_parents(
            node.parent_keys(links, note_map),
            representative,
            &present,
            &key,
        );
        drop(parents_of_key.insert(key, parents));
    }

    let hidden: HashSet<String> = nodes
        .iter()
        .filter(|node| {
            node.timestamp_ms() == 0
                || (node.visibility() == Visibility::Trace
                    && !structural_keys.contains(&node.node_key()))
        })
        .map(HistoryNode::node_key)
        .collect();

    let mut result = Vec::with_capacity(nodes.len());
    for mut node in nodes {
        let key = node.node_key();
        if hidden.contains(&key) {
            continue;
        }
        let parents = if hidden.is_empty() {
            node.parent_keys(links, note_map)
        } else {
            nearest_kept_ancestors(&key, &parents_of_key, &hidden)
        };
        node.override_parent_keys(&crate::canonicalize_parents(
            parents,
            representative,
            &present,
            &key,
        ));
        result.push(node);
    }
    result
}

/// Find the nearest visible ancestors of `key` through omitted rows.
#[must_use]
fn nearest_kept_ancestors(
    key: &str,
    parents_of_key: &HashMap<String, Vec<String>>,
    hidden: &HashSet<String>,
) -> Vec<String> {
    #[expect(
        clippy::too_many_arguments,
        reason = "The recursive walk carries shared traversal and output state."
    )]
    fn walk(
        current: &str,
        parents_of_key: &HashMap<String, Vec<String>>,
        hidden: &HashSet<String>,
        visited: &mut HashSet<String>,
        output: &mut Vec<String>,
        seen_output: &mut HashSet<String>,
    ) {
        if !visited.insert(current.to_owned()) {
            return;
        }
        let parents = parents_of_key.get(current).map_or(&[][..], Vec::as_slice);
        for parent in parents {
            if hidden.contains(parent) {
                walk(parent, parents_of_key, hidden, visited, output, seen_output);
            } else if seen_output.insert(parent.clone()) {
                output.push(parent.clone());
            }
        }
    }

    let mut output = Vec::new();
    let mut seen_output = HashSet::new();
    let mut visited = HashSet::new();
    walk(
        key,
        parents_of_key,
        hidden,
        &mut visited,
        &mut output,
        &mut seen_output,
    );
    output
}
