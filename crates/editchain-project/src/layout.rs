//! Deterministic graph layout for rendering unified history.
//!
//! Produces enough geometry for a webview to draw a git-graph-style
//! visualization over unified history (`EditChain` ops + git commits): each
//! node gets a lane index and each edge gets an ordered path of grid points so
//! lines stay continuous across rows. This is a pure function of node order
//! and parent edges — no filesystem or process dependencies — so it can later
//! target WASM.
//!
//! Two entry points are provided:
//!
//! - [`compute_lanes`] — per-row lane assignment over [`OpId`] nodes (node →
//!   lane plus active lanes per row). Kept for compatibility with existing
//!   callers.
//! - [`compute_graph_layout`] — a branch-aware layout over opaque string node
//!   keys that additionally produces ordered edge paths (child → parent,
//!   through every intermediate grid point). This is what the webview uses to
//!   draw continuous git-style lines across rows.

use std::collections::HashMap;

use editchain_core::OpId;

/// A single row in the [`compute_lanes`] layout.
#[derive(Debug, Clone)]
pub struct LaneRow {
    /// The node this row represents.
    pub node: OpId,
    /// The lane this node occupies.
    pub lane: usize,
    /// The lanes active at this row (for drawing vertical connectors).
    pub active_lanes: Vec<usize>,
}

/// Compute a per-row lane layout for a set of nodes given their parent edges.
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
                let pl = find_spare_lane(&active);
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

/// Find a spare lane index for [`compute_lanes`].
fn find_spare_lane(active: &[Option<OpId>]) -> usize {
    if let Some(i) = active.iter().position(Option::is_none) {
        return i;
    }
    active.len()
}

// ---------------------------------------------------------------------------
// Branch-aware graph layout over string node keys
// ---------------------------------------------------------------------------

/// A grid point in the graph: a row index and a lane index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridPoint {
    /// Row index (0 = newest).
    pub row: usize,
    /// Lane index.
    pub lane: usize,
}

/// A single edge in the graph, from a child node down to one of its parents.
///
/// `points` is ordered child → parent and includes every intermediate grid
/// point the edge passes through (the child's own point first, the parent's
/// point last). This lets the webview draw one continuous path per edge rather
/// than fragmented per-row segments.
#[derive(Debug, Clone)]
pub struct LaneEdge {
    /// The child node key (the newer end of the edge).
    pub child: String,
    /// The parent node key (the older end of the edge).
    pub parent: String,
    /// Ordered grid points from child to parent.
    pub points: Vec<GridPoint>,
}

/// A single graph row in [`compute_graph_layout`].
#[derive(Debug, Clone)]
pub struct GraphRow {
    /// The node key this row represents.
    pub node: String,
    /// The lane this node occupies.
    pub lane: usize,
}

/// The full graph layout for a set of nodes.
#[derive(Debug, Clone)]
pub struct GraphLayout {
    /// Per-row assignment (row index → node key + lane).
    pub rows: Vec<GraphRow>,
    /// All edges (child → parent), each with its ordered point path.
    pub edges: Vec<LaneEdge>,
}

/// Compute a graph layout for a set of nodes given their parents.
///
/// `nodes` are in display order (newest-first). `parents_of` returns the
/// parent node keys for a given node key. Node keys are opaque strings (`OpId`
/// strings or git OID hex), so this works uniformly over both domains.
///
/// The algorithm mirrors git-log's `determinePath`: it walks newest → oldest,
/// assigning each node a lane via spare-lane search and records every
/// intermediate grid point each child→parent edge passes through so lines stay
/// continuous across rows.
#[must_use]
pub fn compute_graph_layout(
    nodes: &[String],
    parents_of: impl Fn(&str) -> Vec<String>,
) -> GraphLayout {
    let ctx = LayoutContext::new(nodes, &parents_of);
    let edges = ctx.edges_for_window(0, nodes.len());
    GraphLayout {
        rows: ctx.lanes,
        edges,
    }
}

/// Build a node key → row index map over a node list.
#[must_use]
pub fn build_row_of(nodes: &[String]) -> HashMap<String, usize> {
    nodes
        .iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), i))
        .collect()
}

/// Build a node key → lane map over a lane assignment.
#[must_use]
pub fn build_lane_at(lanes: &[GraphRow]) -> HashMap<String, usize> {
    lanes.iter().map(|r| (r.node.clone(), r.lane)).collect()
}

/// A cached layout context for one filter state.
///
/// Bundles the O(V) derived data (node keys, row index map, lane map, lane
/// assignment) so that per-window edge computation is O(window) rather than
/// O(V). Built once per filter state and reused across scrolls/resizes.
#[derive(Debug, Clone)]
pub struct LayoutContext {
    /// Node keys in canonical newest-first order.
    pub keys: Vec<String>,
    /// Node key → row index.
    pub row_of: HashMap<String, usize>,
    /// Node key → lane.
    pub lane_at: HashMap<String, usize>,
    /// Per-row lane assignment.
    pub lanes: Vec<GraphRow>,
    /// Node key → parent node keys.
    pub parents: HashMap<String, Vec<String>>,
}

impl LayoutContext {
    /// Build a context from a node list and a parents closure.
    #[must_use]
    pub fn new(nodes: &[String], parents_of: &impl Fn(&str) -> Vec<String>) -> Self {
        let lanes = compute_lane_assignment(nodes, parents_of);
        let row_of = build_row_of(nodes);
        let lane_at = build_lane_at(&lanes);
        let parents = nodes.iter().map(|k| (k.clone(), parents_of(k))).collect();
        Self {
            keys: nodes.to_vec(),
            row_of,
            lane_at,
            lanes,
            parents,
        }
    }

    /// Compute edge geometry for a bounded window of rows `[offset, offset+limit)`.
    ///
    /// Only edges whose child falls inside the window are emitted; an edge whose
    /// parent lies outside the window is drawn down to the window bottom (the
    /// webview extends it). Cost is proportional to the visible slice.
    #[expect(
        clippy::indexing_slicing,
        reason = "row is bounded by end which is clamped to keys.len()"
    )]
    #[must_use]
    pub fn edges_for_window(&self, offset: usize, limit: usize) -> Vec<LaneEdge> {
        let end = offset.saturating_add(limit).min(self.keys.len());
        let mut edges: Vec<LaneEdge> = Vec::new();
        for row in offset..end {
            let key = &self.keys[row];
            let my_lane = *self.lane_at.get(key).unwrap_or(&0);
            let node_parents = self.parents.get(key).map_or(&[][..], Vec::as_slice);
            for parent in node_parents {
                // Only draw an edge when the parent appears below us in the list.
                match self.row_of.get(parent).copied() {
                    Some(parent_row) if parent_row > row => {
                        let p_lane = *self.lane_at.get(parent).unwrap_or(&my_lane);
                        // Clamp the edge to the window: if the parent lies outside
                        // (below) the window, draw only down to the window bottom
                        // and let the webview extend it. This keeps per-edge point
                        // cost proportional to the window, not the whole graph.
                        let draw_to = parent_row.min(end);
                        edges.push(LaneEdge {
                            child: key.clone(),
                            parent: parent.clone(),
                            points: build_edge_points(row, my_lane, draw_to, p_lane),
                        });
                    }
                    _ => {}
                }
            }
        }
        edges
    }
}

/// Compute the per-row lane assignment (no edges).
///
/// This is a linear pass over `nodes` (newest-first): each node is assigned a
/// lane via spare-lane search, and its first parent inherits that lane while
/// secondary parents get fresh lanes. The result is stable across viewport
/// sizes and scroll positions — it depends only on the graph topology — so it
/// can be computed once and cached.
#[expect(
    clippy::indexing_slicing,
    clippy::let_underscore_untyped,
    reason = "Lane indices are bounds-checked; HashMap insert returns Option which is discarded"
)]
#[must_use]
pub fn compute_lane_assignment(
    nodes: &[String],
    parents_of: &impl Fn(&str) -> Vec<String>,
) -> Vec<GraphRow> {
    // Map each node key to its assigned lane.
    let mut lane_of: HashMap<String, usize> = HashMap::new();
    // Active lanes: which node key currently occupies each lane.
    let mut active: Vec<Option<String>> = Vec::new();

    // First pass (newest-first): assign lanes by walking parents.
    for key in nodes {
        let parents = parents_of(key);
        let my_lane = if let Some(&l) = lane_of.get(key) {
            l
        } else {
            let l = active.len();
            active.push(Some(key.clone()));
            let _ = lane_of.insert(key.clone(), l);
            l
        };
        // Remove this node from its own lane.
        active[my_lane] = None;
        // Place parents; first parent inherits this lane.
        if let Some(first_parent) = parents.first() {
            active[my_lane] = Some(first_parent.clone());
            let _ = lane_of.entry(first_parent.clone()).or_insert(my_lane);
        }
        for parent in parents.iter().skip(1) {
            if !lane_of.contains_key(parent) {
                let pl = find_spare_lane_str(&active);
                if pl < active.len() {
                    active[pl] = Some(parent.clone());
                } else {
                    active.push(Some(parent.clone()));
                }
                let _ = lane_of.insert(parent.clone(), pl);
            }
        }
    }

    // Build per-row GraphRow entries.
    nodes
        .iter()
        .map(|key| GraphRow {
            node: key.clone(),
            lane: *lane_of.get(key).unwrap_or(&0),
        })
        .collect()
}

/// Build the ordered grid points for an edge from `(child_row, child_lane)` down
/// to `(parent_row, parent_lane)`.
///
/// The path starts at the child's point and ends at the parent's point. Between
/// them it runs vertically on the child's own lane down to just above the
/// parent's row; if the lanes differ it jogs horizontally onto the parent's
/// lane at that final intermediate step before landing on the parent's point.
/// This yields continuous vertical runs plus one horizontal transition per edge.
fn build_edge_points(
    child_row: usize,
    child_lane: usize,
    parent_row: usize,
    parent_lane: usize,
) -> Vec<GridPoint> {
    debug_assert!(parent_row > child_row, "parent must be below child");

    // Start at the child's own point.
    let mut points = Vec::with_capacity(parent_row.saturating_sub(child_row).saturating_add(2));
    points.push(GridPoint {
        row: child_row,
        lane: child_lane,
    });

    // Vertical run on the child's own lane down to just above the transition
    // step (`..` excludes `parent_row - 1`, which we add explicitly).
    let run_end = parent_row.saturating_sub(1);
    for r in (child_row.saturating_add(1))..run_end {
        points.push(GridPoint {
            row: r,
            lane: child_lane,
        });
    }

    // If lanes differ, jog horizontally onto the parent's lane just above it.
    if child_lane != parent_lane {
        points.push(GridPoint {
            row: run_end,
            lane: child_lane,
        });
        points.push(GridPoint {
            row: run_end,
            lane: parent_lane,
        });
    }

    // Land on the parent's point.
    points.push(GridPoint {
        row: parent_row,
        lane: parent_lane,
    });
    points
}

/// Find a spare lane index for string-keyed nodes.
fn find_spare_lane_str(active: &[Option<String>]) -> usize {
    if let Some(i) = active.iter().position(Option::is_none) {
        return i;
    }
    active.len()
}
