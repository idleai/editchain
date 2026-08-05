//! Deterministic lane layout for graph rendering.
//!
//! Produces, for each history node, a lane index and the set of active lanes
//! at that row — enough for a webview to draw a git-graph-style visualization.
//! This is a pure function of node order and parent edges.

use std::collections::HashMap;

use editchain_core::OpId;

/// A single row in the lane layout.
#[derive(Debug, Clone)]
pub struct LaneRow {
    /// The node this row represents.
    pub node: OpId,
    /// The lane this node occupies.
    pub lane: usize,
    /// The lanes active at this row (for drawing vertical connectors).
    pub active_lanes: Vec<usize>,
}

/// Compute a lane layout for a set of nodes given their parent edges.
///
/// `nodes` are in display order (newest-first). `parents_of` returns the
/// parent node IDs for a given node. The algorithm assigns each node a lane
/// and tracks active lanes, mirroring git-log lane rendering.
#[expect(
    clippy::indexing_slicing,
    clippy::let_underscore_untyped,
    reason = "Lane layout uses bounds-checked lane indices; HashMap insert returns Option which is discarded"
)]
#[must_use]
pub fn compute_lanes(nodes: &[OpId], parents_of: impl Fn(&OpId) -> Vec<OpId>) -> Vec<LaneRow> {
    // Map each node to its lane.
    let mut lane_of: HashMap<OpId, usize> = HashMap::new();
    // Active lanes: which node currently occupies each lane.
    let mut active: Vec<Option<OpId>> = Vec::new();

    // First pass (newest-first): assign lanes by walking parents.
    for &node in nodes {
        let parents = parents_of(&node);
        let lane = if let Some(&l) = lane_of.get(&node) {
            l
        } else {
            let l = active.len();
            active.push(Some(node));
            let _ = lane_of.insert(node, l);
            l
        };
        // Remove this node from its lane.
        active[lane] = None;
        // Insert parents into lanes.
        if let Some(&first_parent) = parents.first() {
            active[lane] = Some(first_parent);
            let _ = lane_of.entry(first_parent).or_insert(lane);
        }
        for parent in parents.iter().skip(1) {
            if !lane_of.contains_key(parent) {
                let pl = find_spare_lane(&active, lane);
                if pl < active.len() {
                    active[pl] = Some(*parent);
                } else {
                    active.push(Some(*parent));
                }
                let _ = lane_of.insert(*parent, pl);
            }
        }
    }

    // Second pass (newest-first): record active lanes per row.
    let mut forward_active: Vec<Option<OpId>> = Vec::new();
    let mut rows = Vec::with_capacity(nodes.len());
    for &node in nodes {
        let parents = parents_of(&node);
        let op_lane = lane_of.get(&node).copied().unwrap_or(0);
        while forward_active.len() <= op_lane {
            forward_active.push(None);
        }
        // Record active lanes (indices that are occupied).
        let active_lanes: Vec<usize> = forward_active
            .iter()
            .enumerate()
            .filter_map(|(i, o)| o.map_or(None, |_| Some(i)))
            .collect();
        rows.push(LaneRow {
            node,
            lane: op_lane,
            active_lanes,
        });
        // Advance: remove this node, add parents.
        forward_active[op_lane] = None;
        if let Some(&first_parent) = parents.first() {
            if let Some(&pl) = lane_of.get(&first_parent) {
                while forward_active.len() <= pl {
                    forward_active.push(None);
                }
                forward_active[pl] = Some(first_parent);
            }
        }
        for parent in parents.iter().skip(1) {
            if let Some(&pl) = lane_of.get(parent) {
                while forward_active.len() <= pl {
                    forward_active.push(None);
                }
                forward_active[pl] = Some(*parent);
            }
        }
    }

    rows
}

/// Find a spare lane index, preferring one near `preferred`.
#[expect(
    clippy::indexing_slicing,
    reason = "preferred is bounds-checked before indexing"
)]
fn find_spare_lane(active: &[Option<OpId>], preferred: usize) -> usize {
    if preferred < active.len() && active[preferred].is_none() {
        return preferred;
    }
    if let Some(i) = active.iter().position(Option::is_none) {
        return i;
    }
    active.len()
}
