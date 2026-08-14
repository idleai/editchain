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
    is_git: &impl Fn(&str) -> bool,
) -> GraphLayout {
    let ctx = LayoutContext::new(nodes, &parents_of, is_git);
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
    /// Node key → child node keys (reverse of `parents`). Used to emit edges
    /// whose child lies above the window so lines entering from offscreen above
    /// are still drawn through the visible slice.
    pub children_of: HashMap<String, Vec<String>>,
}

impl LayoutContext {
    /// Build a context from a node list and a parents closure.
    #[must_use]
    pub fn new(
        nodes: &[String],
        parents_of: &impl Fn(&str) -> Vec<String>,
        is_git: &impl Fn(&str) -> bool,
    ) -> Self {
        // Compute lanes from a TOPOLOGICAL ordering of the nodes (parents before
        // children), so each causal chain gets contiguous lanes regardless of the
        // row order. This decouples lane assignment from time-sorting: time-sort
        // only changes which row a node occupies, never its lane.
        // Assign lanes with freed-lane reuse so disconnected sequential chains
        // (e.g. separate sessions) share columns instead of each claiming a
        // permanent fresh lane. `nodes` are newest-first, which is the display
        // order the reuse algorithm needs to detect non-overlapping intervals.
        let lane_of = compute_lane_map_reuse(nodes, parents_of, is_git);
        // Per-row lanes in the given (possibly time-sorted) node order.
        let lanes: Vec<GraphRow> = nodes
            .iter()
            .map(|k| GraphRow {
                node: k.clone(),
                lane: *lane_of.get(k).unwrap_or(&0),
            })
            .collect();
        let row_of = build_row_of(nodes);
        let lane_at = build_lane_at(&lanes);
        let parents: HashMap<String, Vec<String>> =
            nodes.iter().map(|k| (k.clone(), parents_of(k))).collect();
        // Build reverse adjacency (parent -> children) for boundary-edge lookup.
        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        for (key, ps) in &parents {
            for p in ps {
                children_of.entry(p.clone()).or_default().push(key.clone());
            }
        }
        Self {
            keys: nodes.to_vec(),
            row_of,
            lane_at,
            lanes,
            parents,
            children_of,
        }
    }

    /// Compute edge geometry for a bounded window of rows `[offset, offset+limit)`.
    ///
    /// An edge is emitted if its child OR its parent falls inside the window.
    /// This way a line that passes *through* the visible slice is drawn even
    /// when its origin (child) or destination (parent) lies offscreen — so
    /// scrolling deep doesn't leave lines missing at the top or bottom of the
    /// viewport. Edges whose endpoint lies outside the window are clamped to
    /// the window edge and extended by the webview. Cost stays proportional to
    /// the visible slice plus its boundary edges.
    #[expect(
        clippy::indexing_slicing,
        reason = "row is bounded by end which is clamped to keys.len()"
    )]
    #[must_use]
    pub fn edges_for_window(&self, offset: usize, limit: usize) -> Vec<LaneEdge> {
        let end = offset.saturating_add(limit).min(self.keys.len());
        let mut edges: Vec<LaneEdge> = Vec::new();

        // Emit edges whose child is inside the window (child -> parent below).
        for row in offset..end {
            let key = &self.keys[row];
            let my_lane = *self.lane_at.get(key).unwrap_or(&0);
            let node_parents = self.parents.get(key).map_or(&[][..], Vec::as_slice);
            for parent in node_parents {
                match self.row_of.get(parent).copied() {
                    Some(parent_row) if parent_row > row => {
                        let p_lane = *self.lane_at.get(parent).unwrap_or(&my_lane);
                        // Clamp to window bottom if the parent lies below it.
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

        // Emit edges whose PARENT is inside the window but whose child is above
        // it (already scrolled past). These lines enter from offscreen above and
        // must still be drawn through the visible slice. Only when the parent is
        // strictly below the window top (row > offset) so the clamped start
        // point stays above the parent.
        for row in offset.saturating_add(1)..end {
            let key = &self.keys[row];
            let my_lane = *self.lane_at.get(key).unwrap_or(&0);
            // Find children of this node that appear above the window.
            if let Some(children) = self.children_of.get(key) {
                for child in children {
                    match self.row_of.get(child).copied() {
                        Some(child_row) if child_row < offset => {
                            let c_lane = *self.lane_at.get(child).unwrap_or(&my_lane);
                            // Clamp to window top; webview extends up from here.
                            let draw_from = offset;
                            edges.push(LaneEdge {
                                child: child.clone(),
                                parent: key.clone(),
                                points: build_edge_points(draw_from, c_lane, row, my_lane),
                            });
                        }
                        _ => {}
                    }
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
    is_git: &impl Fn(&str) -> bool,
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

    // Git-leftmost post-pass: if any git node exists, shift every non-git lane
    // up by 1 and pin git nodes to lane 0. This keeps git commits on the
    // leftmost column regardless of the base assignment.
    if nodes.iter().any(|k| is_git(k)) {
        for (key, l) in &mut lane_of {
            if is_git(key) {
                *l = 0;
            } else {
                *l = l.saturating_add(1);
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

/// Compute a topological ordering of `nodes` (parents before children).
///
/// Used to assign lanes independently of row order, so time-sorting rows does
/// not fragment a causal chain across lanes. Nodes with no present parents are
/// emitted first; remaining nodes (cycles) are appended in input order.
#[must_use]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::let_underscore_untyped,
    reason = "In-degree counters are bounded by the number of present parents; HashMap insert returns Option which is discarded"
)]
fn topological_order(nodes: &[String], parents_of: &impl Fn(&str) -> Vec<String>) -> Vec<String> {
    use std::collections::{HashSet, VecDeque};
    let present: HashSet<String> = nodes.iter().cloned().collect();
    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut indegree: HashMap<String, usize> = HashMap::new();
    for key in nodes {
        let _ = indegree.entry(key.clone()).or_insert(0);
        for parent in parents_of(key) {
            if present.contains(&parent) {
                children_of.entry(parent).or_default().push(key.clone());
                *indegree.entry(key.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut queue: VecDeque<String> = indegree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(k, _)| k.clone())
        .collect();
    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    while let Some(key) = queue.pop_front() {
        order.push(key.clone());
        if let Some(children) = children_of.get(&key) {
            for child in children {
                if let Some(deg) = indegree.get_mut(child) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }
    }
    // Append any remaining (cyclic) nodes in input order.
    let emitted: HashSet<String> = order.iter().cloned().collect();
    for key in nodes {
        if !emitted.contains(key) {
            order.push(key.clone());
        }
    }
    order
}

/// Compute a node-key → lane map over a topological ordering.
///
/// Walks parents-before-children so each node inherits its first parent's lane,
/// keeping a causal chain contiguous on one lane regardless of row order.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    clippy::let_underscore_untyped,
    reason = "Lane indices are bounds-checked; HashMap insert returns Option which is discarded"
)]
fn compute_lane_map(
    topo: &[String],
    parents_of: &impl Fn(&str) -> Vec<String>,
) -> HashMap<String, usize> {
    let mut lane_of: HashMap<String, usize> = HashMap::new();
    // Track which lanes are currently occupied (by a node awaiting its parent).
    let mut active: Vec<Option<String>> = Vec::new();
    for key in topo {
        let parents = parents_of(key);
        // Inherit the first parent's lane if it already has one; otherwise take
        // a fresh lane.
        let my_lane = if let Some(&l) = lane_of.get(key) {
            l
        } else if let Some(first_parent) = parents.first() {
            *lane_of.get(first_parent).unwrap_or(&0)
        } else {
            let l = active.len();
            active.push(Some(key.clone()));
            let _: Option<usize> = lane_of.insert(key.clone(), l);
            l
        };
        // Record this node's lane.
        let _: Option<usize> = lane_of.insert(key.clone(), my_lane);
        // Secondary parents get fresh lanes (for merges).
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
    lane_of
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

// ---------------------------------------------------------------------------
// Lane reuse across disconnected chains
// ---------------------------------------------------------------------------

/// Compute a node-key → lane map with **freed-lane reuse** across disconnected
/// chains.
///
/// Unlike [`compute_lane_map`], which gives every root node its own permanent
/// fresh lane, this assigns lanes so two disconnected chains whose display-row
/// ranges do *not* overlap share one base lane instead of consuming separate
/// permanent ones. This keeps long histories readable when many sequential,
/// non-overlapping sessions would otherwise each claim their own column.
///
/// The approach treats each connected component of the graph as an *interval*
/// over display rows (`nodes` are newest-first; row 0 is newest). Components are
/// greedily colored by interval so overlapping components get distinct base
/// colors while non-overlapping ones may share — this is exactly git-log-style
/// column packing and yields minimal base columns for sequential sessions.
/// Within each component the existing branch logic runs unchanged relative to
/// that base color (`compute_lane_map` semantics), so merges still span extra
/// lanes above their base column.
///
/// Because same-color components have disjoint row intervals by construction,
/// their internal branch activity never temporally overlaps another same-color
/// component's region — reused columns never carry crossing edges.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    clippy::let_underscore_untyped,
    reason = "Lane indices are bounds-checked against the active vector length"
)]
fn compute_lane_map_reuse(
    nodes_newest_first: &[String],
    parents_of: &impl Fn(&str) -> Vec<String>,
    is_git: &impl Fn(&str) -> bool,
) -> HashMap<String, usize> {
    use std::collections::{HashSet, VecDeque};

    // --- Phase 0/1: connected components over undirected edges ------------------
    // Build undirected adjacency so we can flood-fill components regardless of
    // edge direction.
    let mut adj_undirected: HashMap<String, Vec<String>> = HashMap::new();
    for key in nodes_newest_first {
        let _: &mut Vec<String> = adj_undirected.entry(key.clone()).or_default();
        for parent in parents_of(key) {
            let _: &mut Vec<String> = adj_undirected.entry(parent.clone()).or_default();
            if let Some(neighbors) = adj_undirected.get_mut(&parent) {
                neighbors.push(key.clone());
            }
            let _: &mut Vec<String> = adj_undirected.entry(key.clone()).or_default();
            if let Some(neighbors) = adj_undirected.get_mut(key) {
                neighbors.push(parent.clone());
            }
        }
    }

    // Row index per key within `nodes_newest_first`.
    let mut row_of_key: HashMap<String, usize> = HashMap::with_capacity(nodes_newest_first.len());
    for (i, k) in nodes_newest_first.iter().enumerate() {
        let _ = row_of_key.insert(k.clone(), i);
    }

    // Flood-fill components; record each component's [start,end] row span where
    // start = smallest row index (= newest member), end = largest (= oldest).
    // Also record whether each component contains any git node, so git commits
    // can be pinned to the leftmost lane (0).
    let mut comp_id_of_key: HashMap<String, usize> = HashMap::new();
    let mut comp_start_end: Vec<(usize, usize)> = Vec::new(); // per comp id -> span
    let mut comp_is_git: Vec<bool> = Vec::new(); // per comp id -> contains a git node
    let mut seen_keys: HashSet<String> = HashSet::with_capacity(nodes_newest_first.len());
    for seed in nodes_newest_first {
        if seen_keys.contains(seed) {
            continue;
        }
        let _: bool = seen_keys.insert(seed.clone());
        let mut queue_local: VecDeque<String> = VecDeque::from([seed.clone()]);
        let mut members_start_end = (
            *row_of_key.get(seed).unwrap_or(&usize::MAX),
            *row_of_key.get(seed).unwrap_or(&usize::MAX),
        );
        let mut any_git = is_git(seed);
        while let Some(k) = queue_local.pop_front() {
            members_start_end = fold_span(
                members_start_end,
                *row_of_key.get(&k).unwrap_or(&usize::MAX),
            );
            if is_git(&k) {
                any_git = true;
            }
            let _ = comp_id_of_key.insert(k.clone(), comp_start_end.len());
            if let Some(neighbors) = adj_undirected.get(&k) {
                for nbr in neighbors {
                    if !seen_keys.contains(nbr) {
                        let _: bool = seen_keys.insert(nbr.clone());
                        queue_local.push_back(nbr.clone());
                    }
                }
            }
        }
        comp_start_end.push(members_start_end);
        comp_is_git.push(any_git);
    }

    // --- Phase 2/3: greedy interval coloring --------------------------------------
    // Sort component ids by start row ascending so non-overlapping intervals get
    // colored greedily; release colors when an interval ends so later disjoint
    // intervals can reuse them.
    //
    // Git components are pinned to lane 0 (the leftmost column) so git commits
    // always render on the far-left lane. Op components are colored greedily but
    // offset by +1 (when git is present) so they never collide with the git lane.
    let git_present = comp_is_git.iter().any(|&g| g);
    let mut comp_ids_sorted_by_start: Vec<usize> = comp_start_end
        .iter()
        .enumerate()
        .map(|(id, _)| id)
        .collect();
    comp_ids_sorted_by_start.sort_by_key(|&id| comp_start_end[id]);

    // Per-color list of currently-open component ids ending latest; used to know
    // when a color becomes reusable again. Index 0 is reserved for git when any
    // git component exists.
    let mut color_open_end_max: Vec<usize> = Vec::new(); // color -> max end among open comps
    let mut comp_color_by_id: Vec<usize> = vec![usize::MAX; comp_start_end.len()]; // comp id -> base color/lane

    for &cid in &comp_ids_sorted_by_start {
        let start = comp_start_end[cid].0;
        let end = comp_start_end[cid].1;
        if comp_is_git[cid] {
            // Git components always occupy lane 0 (leftmost). They are disjoint
            // in time (a git chain), so they share the column.
            if color_open_end_max.is_empty() {
                color_open_end_max.push(end);
            }
            comp_color_by_id[cid] = 0;
            color_open_end_max[0] = color_open_end_max[0].max(end);
            continue;
        }
        // Op components: find a reusable column. When git is present, skip lane
        // 0 (reserved for git); otherwise start from lane 0 as before.
        let skip = usize::from(git_present);
        let mut chosen_color = None;
        for (c, &open_end) in color_open_end_max.iter().enumerate().skip(skip) {
            if open_end < start {
                chosen_color = Some(c);
                break;
            }
        }
        let color = chosen_color.unwrap_or_else(|| {
            // New column: its open interval ends at this component's end.
            color_open_end_max.push(end);
            color_open_end_max.len().saturating_sub(1)
        });
        comp_color_by_id[cid] = color;
        // Track the latest end among components currently open on this column.
        color_open_end_max[color] = color_open_end_max[color].max(end);
    }

    // --- Phase 4: assign lanes within each component -----------------------------
    // Run the existing branch-aware lane assignment per component, offset by the
    // component's base color so different components never collide on a column.
    // We reuse `compute_lane_map` on the component's own topological order, then
    // shift every lane by `base`.
    let mut lane_of: HashMap<String, usize> = HashMap::with_capacity(nodes_newest_first.len());
    for (cid, &base) in comp_color_by_id.iter().enumerate() {
        // Collect this component's members.
        let members: Vec<String> = comp_id_of_key
            .iter()
            .filter(|&(_, &c)| c == cid)
            .map(|(k, _)| k.clone())
            .collect();
        if members.is_empty() {
            continue;
        }
        // Topological order of just this component (parents before children).
        let topo = topological_order(&members, parents_of);
        let local = compute_lane_map(&topo, parents_of);
        for (key, l) in local {
            let _ = lane_of.insert(key, base.saturating_add(l));
        }
    }

    // Git-leftmost global remap: force every git node onto lane 0 and shift all
    // op lanes up by 1. This guarantees git commits always render on the
    // leftmost column, even inside mixed components (git linked to ops). Ops
    // shift uniformly so their relative lane reuse is preserved; no collision
    // occurs because ops move off lane 0 while git takes it.
    if git_present {
        for (key, l) in &mut lane_of {
            if is_git(key) {
                *l = 0;
            } else {
                *l = l.saturating_add(1);
            }
        }
    }

    lane_of
}

/// Fold a row index into a running `(min, max)` span.
fn fold_span(span: (usize, usize), row: usize) -> (usize, usize) {
    (span.0.min(row), span.1.max(row))
}
