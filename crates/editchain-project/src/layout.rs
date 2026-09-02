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

use std::collections::{HashMap, HashSet};

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
    /// Cross-lane edges whose bend belongs in the parent node's row. True
    /// forks (a parent with multiple children) and operation→Git session
    /// anchors use this orientation so an above-right branch forms the visual
    /// bottom-right corner before entering the anchor. Merge-only edges retain
    /// child-side bends.
    pub parent_anchored_edges: HashSet<(String, String)>,
    /// Node key → connected-component id. Used to detect open chains that span
    /// across a query window so pass-through edges are still drawn.
    pub comp_id: HashMap<String, usize>,
    /// Node key → minimum row index among all members of its component.
    pub comp_min: HashMap<String, usize>,
    /// Node key → maximum row index among all members of its component.
    pub comp_max: HashMap<String, usize>,
    /// Lane → sorted list of `(component id, min, max)` row spans during which
    /// that lane is actually OCCUPIED by a node of that component (not just
    /// touched by the component). Lets us find open chains spanning a query
    /// window even when none of their nodes fall inside it (sparse chains),
    /// without iterating window rows — while still refusing to draw a line on a
    /// lane that has no node in the window.
    pub lane_spans: HashMap<usize, Vec<(usize, usize, usize)>>,
    /// Per-row lanes with a vertical segment in the TOP half of that row's cell
    /// (lines entering from above). Splitting above/below lets tips (newest nodes,
    /// no children) draw no line above their dot and roots (no parents) draw no
    /// line below — no dangling segments. Static, shipped per row.
    pub row_above: Vec<Vec<usize>>,
    /// Per-row lanes with a vertical segment in the BOTTOM half of that row's cell
    /// (lines leaving downward). See [`Self::row_above`].
    pub row_below: Vec<Vec<usize>>,
    /// Per-row cross-lane transitions: for each row index, the list of
    /// `(from_lane, to_lane)` bends that occur there. Fork/session-anchor bends
    /// live in the parent row; merge-only bends remain at the child or final
    /// pre-parent row. Also static and shipped per row.
    pub row_transitions: Vec<Vec<(usize, usize)>>,
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
        // Iterate `nodes` (canonical display order) rather than the `parents`
        // HashMap: HashMap iteration order is process-random, so children must
        // be pushed in a stable order for identical edge emission across runs.
        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        for key in nodes {
            if let Some(ps) = parents.get(key) {
                for p in ps {
                    children_of.entry(p.clone()).or_default().push(key.clone());
                }
            }
        }
        // A branch visually originates at its shared parent, so its cross-lane
        // bend belongs in that parent's row. Cross-domain operation→Git edges
        // are exact session-start anchors and follow the same rule even when
        // the commit currently has only that one visible child. Merge-only
        // edges stay child-anchored so multiple parents still fan out from the
        // merge node rather than appearing to fork later in history.
        let mut parent_anchored_edges: HashSet<(String, String)> = HashSet::new();
        for child in nodes {
            let child_is_git = is_git(child);
            if let Some(node_parents) = parents.get(child) {
                for parent in node_parents {
                    let is_fork = children_of
                        .get(parent)
                        .is_some_and(|children| children.len() > 1);
                    let is_session_git_anchor = !child_is_git && is_git(parent);
                    if is_fork || is_session_git_anchor {
                        let _: bool = parent_anchored_edges.insert((child.clone(), parent.clone()));
                    }
                }
            }
        }
        // Precompute connected components so open chains that span across a query
        // window (pass-through edges) can be detected without iterating window rows.
        let (comp_id, comp_min, comp_max, _) = compute_components(nodes, &row_of, &parents);
        // Build per-lane OCCUPANCY RUNS keyed by component: for each lane,
        // record every CONTIGUOUS run of rows where that component actually has
        // geometry on that lane — node dots plus the exact edge runs
        // `build_edge_points` emits (same-lane runs, parent-anchored fork runs,
        // adjacent merge jog halves, and non-adjacent merge source/destination
        // runs).
        //
        // A component may touch several lanes (merges), but a lane is only
        // "open" where the component's geometry actually crosses it. Tracking
        // exact runs (instead of a per-component min/max) matters because
        // in-component lane compaction merges DISJOINT branch runs onto one
        // lane: the merged lane then has real segments on both sides of a
        // window with nothing inside it, and a single [min,max] span would
        // draw a false pass-through line through that gap. Keying by component
        // and merging touching runs still catches genuinely sparse chains —
        // nodes far apart on one lane whose same-lane edges cross the whole
        // window — because those edges produce one continuous run.
        let mut lane_spans: HashMap<usize, Vec<(usize, usize, usize)>> = HashMap::new();
        for (row, key) in nodes.iter().enumerate() {
            let lane = *lane_at.get(key).unwrap_or(&0);
            let cid = *comp_id.get(key).unwrap_or(&usize::MAX);
            lane_spans.entry(lane).or_default().push((cid, row, row));
            let node_parents = parents.get(key).map_or(&[][..], Vec::as_slice);
            for parent in node_parents {
                let Some(parent_row) = row_of.get(parent).copied() else {
                    continue;
                };
                if parent_row <= row {
                    continue; // not a downward edge
                }
                let p_lane = *lane_at.get(parent).unwrap_or(&lane);
                let parent_anchored =
                    parent_anchored_edges.contains(&(key.clone(), parent.clone()));
                if lane == p_lane {
                    lane_spans
                        .entry(lane)
                        .or_default()
                        .push((cid, row, parent_row));
                } else if parent_anchored {
                    // Parent-anchored fork: the source lane stays live through
                    // the top half of the parent row, where the convex curve
                    // terminates at the parent dot. The parent lane's node
                    // occupancy was already recorded above. `lane_spans`
                    // models FULL pass-through runs, so stop one row earlier:
                    // the source touches only the parent row's top half and
                    // must not bridge to a later reused-lane run below it.
                    lane_spans.entry(lane).or_default().push((
                        cid,
                        row,
                        parent_row.saturating_sub(1),
                    ));
                } else if parent_row == row.saturating_add(1) {
                    // Adjacent cross-lane: the jog starts at the child's
                    // midpoint, so the destination lane carries the two
                    // endpoint halves only.
                    lane_spans
                        .entry(p_lane)
                        .or_default()
                        .push((cid, row, parent_row));
                } else {
                    // Non-adjacent cross-lane: source run down to parent_row -
                    // 1, jog, then destination run parent_row - 1..=parent_row.
                    lane_spans.entry(lane).or_default().push((
                        cid,
                        row,
                        parent_row.saturating_sub(1),
                    ));
                    lane_spans.entry(p_lane).or_default().push((
                        cid,
                        parent_row.saturating_sub(1),
                        parent_row,
                    ));
                }
            }
        }
        // Merge overlapping or touching intervals of the SAME component on a
        // lane into maximal contiguous runs, so the pass-through check sees one
        // span per real continuous segment (deterministic: sorted by (cid, lo,
        // hi), never HashMap iteration order).
        for list in lane_spans.values_mut() {
            list.sort_unstable();
            let mut merged: Vec<(usize, usize, usize)> = Vec::with_capacity(list.len());
            for &(cid, lo, hi) in list.iter() {
                if let Some(last) = merged.last_mut() {
                    if last.0 == cid && lo <= last.2.saturating_add(1) {
                        last.2 = last.2.max(hi);
                        continue;
                    }
                }
                merged.push((cid, lo, hi));
            }
            *list = merged;
        }

        // Compute per-row ABOVE/BELOW lanes and cross-lane TRANSITIONS by walking
        // every edge's geometry. An edge from (child_row, child_lane) to
        // (parent_row, parent_lane):
        //   - same lane: vertical on child_lane from child down to parent;
        //   - parent-anchored fork/session edge: vertical on child_lane into the
        //     parent row, then a transition ending at the parent dot;
        //   - merge-only different-lane edge: transition at the child row when
        //     adjacent, otherwise at parent_row-1 with a short destination run.
        // Splitting into above/below halves means a TIP (newest node, no children)
        // draws no line above its dot and a ROOT (no parents) draws no line below —
        // no dangling segments. All static, shipped per row.
        let mut row_above: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
        let mut row_below: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
        let mut row_transitions: Vec<Vec<(usize, usize)>> = vec![Vec::new(); nodes.len()];
        for (row, key) in nodes.iter().enumerate() {
            let my_lane = *lane_at.get(key).unwrap_or(&0);
            let node_parents = parents.get(key).map_or(&[][..], Vec::as_slice);
            for parent in node_parents {
                let Some(parent_row) = row_of.get(parent).copied() else {
                    continue;
                };
                if parent_row <= row {
                    continue; // parent above or same row — not a downward edge
                }
                let p_lane = *lane_at.get(parent).unwrap_or(&my_lane);
                let parent_anchored =
                    parent_anchored_edges.contains(&(key.clone(), parent.clone()));
                if my_lane == p_lane {
                    // Same-lane edge: vertical on my_lane from `row` down to
                    // `parent_row`. Bottom half at the child's own row, top half at
                    // the parent's row, both halves in between.
                    if let Some(below) = row_below.get_mut(row) {
                        add_unique(below, my_lane);
                    }
                    if let Some(above) = row_above.get_mut(parent_row) {
                        add_unique(above, my_lane);
                    }
                    for r in (row.saturating_add(1))..parent_row {
                        if let Some(above) = row_above.get_mut(r) {
                            add_unique(above, my_lane);
                        }
                        if let Some(below) = row_below.get_mut(r) {
                            add_unique(below, my_lane);
                        }
                    }
                } else if parent_anchored {
                    // A true fork (or exact operation→Git session anchor)
                    // bends in the PARENT row. Run the child lane down through
                    // every preceding row, enter the parent row from above,
                    // then terminate the transition at the parent's dot. Any
                    // independent edge leaving that parent contributes its own
                    // bottom half on the parent lane.
                    if let Some(below) = row_below.get_mut(row) {
                        add_unique(below, my_lane);
                    }
                    for r in (row.saturating_add(1))..parent_row {
                        if let Some(above) = row_above.get_mut(r) {
                            add_unique(above, my_lane);
                        }
                        if let Some(below) = row_below.get_mut(r) {
                            add_unique(below, my_lane);
                        }
                    }
                    if let Some(above) = row_above.get_mut(parent_row) {
                        add_unique(above, my_lane);
                    }
                    if let Some(transitions) = row_transitions.get_mut(parent_row) {
                        add_unique(transitions, (my_lane, p_lane));
                    }
                } else if parent_row == row.saturating_add(1) {
                    // Adjacent cross-lane edge: the jog originates at the child
                    // node's own midpoint, so the edge has NO source-lane run.
                    // Adding a source-lane top/bottom half at the child row
                    // would create a dangling boundary stub; the transition
                    // starts at the child node itself instead.
                    // Emit the transition at the child row plus the two
                    // destination-lane halves only; any source-lane halves at
                    // this row come from other edges.
                    if let Some(transitions) = row_transitions.get_mut(row) {
                        add_unique(transitions, (my_lane, p_lane));
                    }
                    if let Some(below) = row_below.get_mut(row) {
                        add_unique(below, p_lane);
                    }
                    if let Some(above) = row_above.get_mut(parent_row) {
                        add_unique(above, p_lane);
                    }
                } else {
                    // Non-adjacent different-lane edge: vertical on my_lane down
                    // to parent_row-1, jog to p_lane at parent_row-1, then
                    // vertical on p_lane down to parent_row.
                    if let Some(below) = row_below.get_mut(row) {
                        add_unique(below, my_lane);
                    }
                    for r in (row.saturating_add(1))..parent_row.saturating_sub(1) {
                        if let Some(above) = row_above.get_mut(r) {
                            add_unique(above, my_lane);
                        }
                        if let Some(below) = row_below.get_mut(r) {
                            add_unique(below, my_lane);
                        }
                    }
                    let jog_row = parent_row.saturating_sub(1);
                    if let Some(above) = row_above.get_mut(jog_row) {
                        add_unique(above, my_lane);
                    }
                    if let Some(transitions) = row_transitions.get_mut(jog_row) {
                        add_unique(transitions, (my_lane, p_lane));
                    }
                    if let Some(below) = row_below.get_mut(jog_row) {
                        add_unique(below, p_lane);
                    }
                    if let Some(above) = row_above.get_mut(parent_row) {
                        add_unique(above, p_lane);
                    }
                }
            }
        }
        for list in &mut row_above {
            list.sort_unstable();
        }
        for list in &mut row_below {
            list.sort_unstable();
        }

        Self {
            keys: nodes.to_vec(),
            row_of,
            lane_at,
            lanes,
            parents,
            children_of,
            parent_anchored_edges,
            comp_id,
            comp_min,
            comp_max,
            lane_spans,
            row_above,
            row_below,
            row_transitions,
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
                        let parent_anchored = self
                            .parent_anchored_edges
                            .contains(&(key.clone(), parent.clone()));
                        let parent_visible = parent_row < end;
                        let transition_at_parent = parent_visible && parent_anchored;
                        // A parent-anchored edge does not bend early merely
                        // because its real anchor is below this window. Keep
                        // the visible/clamped segment on the branch lane; the
                        // parent-row curve appears once that row is visible.
                        let draw_lane = if parent_anchored && !parent_visible {
                            my_lane
                        } else {
                            p_lane
                        };
                        edges.push(LaneEdge {
                            child: key.clone(),
                            parent: parent.clone(),
                            points: build_edge_points(
                                row,
                                my_lane,
                                draw_to,
                                draw_lane,
                                transition_at_parent,
                            ),
                        });
                    }
                    _ => {}
                }
            }
        }

        // Emit edges whose PARENT is inside the window but whose child is above
        // it (already scrolled past). These lines enter from offscreen above and
        // must still be drawn through the visible slice. This includes a parent
        // exactly on the window top row (`row == offset`) whose child sits
        // immediately above the window (`child_row == offset - 1`): the child
        // and parent collapse onto the same visible row once clamped, so the
        // path degenerates to the top-row jog.
        for row in offset..end {
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
                            let points = if row == offset {
                                // Parent on the clamp line: the clamped start
                                // lands on the parent's own row, so the visible
                                // path is just the jog onto the parent's lane at
                                // the window top (single point when lanes match).
                                let mut pts = Vec::with_capacity(2);
                                pts.push(GridPoint {
                                    row: offset,
                                    lane: c_lane,
                                });
                                if c_lane != my_lane {
                                    pts.push(GridPoint {
                                        row: offset,
                                        lane: my_lane,
                                    });
                                }
                                pts
                            } else {
                                let transition_at_parent = self
                                    .parent_anchored_edges
                                    .contains(&(child.clone(), key.clone()));
                                build_edge_points(
                                    draw_from,
                                    c_lane,
                                    row,
                                    my_lane,
                                    transition_at_parent,
                                )
                            };
                            edges.push(LaneEdge {
                                child: child.clone(),
                                parent: key.clone(),
                                points,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        // Emit PASS-THROUGH edges: open chains whose component spans across the
        // window — nodes both above `offset` and below `end` — even when none of
        // their nodes fall inside `[offset,end)`. The two loops above only emit
        // edges with an endpoint in the window, so a sparse chain with a gap in
        // the visible slice would otherwise flicker in/out as you scroll. We use
        // the precomputed per-lane OCCUPANCY spans so a chain is detected even
        // when it has NO node inside the window. Emit one vertical line per
        // affected lane from window top to bottom so every open chain stays
        // continuous.
        //
        // A lane qualifies only if it is OCCUPIED both above `offset` and below
        // `end` — i.e. ONE contiguous run of the chain's geometry sits on that
        // lane, crossing the whole window without interruption. A window inside
        // a gap between two compaction-merged runs therefore draws nothing,
        // while a sparse chain whose same-lane edges cross the window still
        // draws its line. This is what keeps a merge branch that only exists
        // far below (or above) the viewport from drawing a spurious line
        // through an otherwise-empty lane.
        // Collect candidate lanes in sorted order so edge emission is stable
        // across processes (HashMap iteration order is process-random).
        let mut pass_through_lanes: Vec<usize> = Vec::new();
        for (lane, spans) in &self.lane_spans {
            // Spans are sorted and merged into contiguous runs; find any run
            // covering [offset,end). Each run is (component id, lo, hi) where
            // [lo,hi] is a continuous segment of that component's geometry on
            // THIS lane — so a lane qualifies only when one component's real
            // segment crosses from above `offset` to below `end` without a gap.
            let covers = spans.iter().any(|&(_, lo, hi)| lo < offset && hi >= end);
            if covers && !pass_through_lanes.contains(lane) {
                pass_through_lanes.push(*lane);
            }
        }
        pass_through_lanes.sort_unstable();
        for lane in pass_through_lanes {
            edges.push(LaneEdge {
                child: format!("__pass_through_{lane}"),
                parent: format!("__pass_through_{lane}"),
                points: vec![
                    GridPoint { row: offset, lane },
                    GridPoint {
                        row: end.saturating_sub(1),
                        lane,
                    },
                ],
            });
        }

        edges
    }
}

/// Compute connected components over undirected edges (parents + children).
///
/// Returns:
///   - `comp_id`: node key → connected-component id,
///   - `comp_min`: node key → minimum row index among its component's members,
///   - `comp_max`: node key → maximum row index among its component's members,
///   - `lane_spans`: lane → sorted list of `(min,max)` row spans of the
///     components occupying that lane. Used to detect open chains that span
///     across a query window even when none of their nodes fall inside it.
type ComponentData = (
    HashMap<String, usize>,
    HashMap<String, usize>,
    HashMap<String, usize>,
    HashMap<usize, Vec<(usize, usize)>>,
);

/// Add an undirected edge between two nodes (both directions).
fn add_undirected_edge(adj: &mut HashMap<String, Vec<String>>, a: &str, b: &str) {
    adj.entry(a.to_string()).or_default().push(b.to_string());
    adj.entry(b.to_string()).or_default().push(a.to_string());
}

/// A component whose span covers `[offset,end)` is an "open chain" passing
/// through that window even when none of its nodes fall inside it — so we can
/// draw continuous vertical lines for it regardless of how sparse its nodes are.
#[must_use]
#[expect(
    clippy::let_underscore_untyped,
    reason = "HashMap insert returns Option which is discarded"
)]
fn compute_components(
    keys: &[String],
    row_of: &HashMap<String, usize>,
    parents: &HashMap<String, Vec<String>>,
) -> ComponentData {
    use std::collections::VecDeque;

    // Undirected adjacency.
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for key in keys {
        let _ = adj.entry(key.clone()).or_default();
        if let Some(ps) = parents.get(key) {
            for p in ps {
                add_undirected_edge(&mut adj, key, p);
            }
        }
    }

    // Flood-fill components; record each component's [lo,hi] row span.
    let mut comp_id: HashMap<String, usize> = HashMap::with_capacity(keys.len());
    let mut comp_span: Vec<(usize, usize)> = Vec::new(); // per comp id -> (lo,hi)
    let mut seen: HashSet<String> = HashSet::with_capacity(keys.len());
    for seed in keys {
        if seen.contains(seed) {
            continue;
        }
        let _ = seen.insert(seed.clone());
        let mut queue: VecDeque<String> = VecDeque::from([seed.clone()]);
        let mut lo = *row_of.get(seed).unwrap_or(&usize::MAX);
        let mut hi = *row_of.get(seed).unwrap_or(&0);
        while let Some(k) = queue.pop_front() {
            lo = lo.min(*row_of.get(&k).unwrap_or(&usize::MAX));
            hi = hi.max(*row_of.get(&k).unwrap_or(&0));
            let _ = comp_id.insert(k.clone(), comp_span.len());
            if let Some(nbrs) = adj.get(&k) {
                for nbr in nbrs {
                    if !seen.contains(nbr) {
                        let _ = seen.insert(nbr.clone());
                        queue.push_back(nbr.clone());
                    }
                }
            }
        }
        comp_span.push((lo, hi));
    }

    // Expand spans to per-node maps.
    let mut comp_min: HashMap<String, usize> = HashMap::with_capacity(keys.len());
    let mut comp_max: HashMap<String, usize> = HashMap::with_capacity(keys.len());
    for key in keys {
        let cid = *comp_id.get(key).unwrap_or(&0);
        if let Some((lo, hi)) = comp_span.get(cid) {
            let _ = comp_min.insert(key.clone(), *lo);
            let _ = comp_max.insert(key.clone(), *hi);
        }
    }

    // Build per-lane sorted list of component spans. A component may occupy
    // several lanes (merges); record its span under each lane it touches.
    // We need lane info per node — recover from the caller's lane_at via keys.
    // To avoid threading lane_at through here, we build lane_spans from the
    // caller by scanning keys; see LayoutContext::new.
    (comp_id, comp_min, comp_max, HashMap::new())
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
    use std::collections::VecDeque;
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
    // Seed the BFS queue from `nodes` in input order (not HashMap iteration
    // order, which is process-random): roots are then emitted in a stable,
    // meaningful tie-break (newest-first) and the whole ordering is
    // reproducible across processes.
    let mut queue: VecDeque<String> = nodes
        .iter()
        .filter(|key| indegree.get(*key) == Some(&0))
        .cloned()
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
    // Track, per parent, the lanes already taken by its children. Used to give
    // a fork's sibling branches distinct lanes (branch-out), so they don't all
    // collapse onto the shared parent's column.
    let mut child_lanes: HashMap<String, Vec<usize>> = HashMap::new();
    for key in topo {
        let parents = parents_of(key);
        // Inherit the first parent's lane if it already has one; otherwise take
        // a fresh lane. If the first parent already has another child on that
        // lane (a fork), take a fresh spare lane instead so the branches stay
        // on distinct columns.
        let my_lane = if let Some(&l) = lane_of.get(key) {
            l
        } else if let Some(first_parent) = parents.first() {
            let inherited = *lane_of.get(first_parent).unwrap_or(&0);
            if child_lanes
                .get(first_parent)
                .is_some_and(|ls| ls.contains(&inherited))
            {
                // The parent already has a child on this lane — fork branch.
                let pl = find_spare_lane_str(&active);
                if pl < active.len() {
                    active[pl] = Some(key.clone());
                } else {
                    active.push(Some(key.clone()));
                }
                pl
            } else {
                inherited
            }
        } else {
            let l = active.len();
            active.push(Some(key.clone()));
            let _: Option<usize> = lane_of.insert(key.clone(), l);
            l
        };
        // Record this node's lane.
        let _: Option<usize> = lane_of.insert(key.clone(), my_lane);
        // Record that this node occupies `my_lane` as a child of its first parent.
        if let Some(first_parent) = parents.first() {
            let ls = child_lanes.entry(first_parent.clone()).or_default();
            if !ls.contains(&my_lane) {
                ls.push(my_lane);
            }
        }
        // Secondary parents get fresh lanes (for merges).
        //
        // A secondary parent needs its own lane whenever it does not yet have
        // one, OR when its existing lane collides with this node's own lane
        // (the first parent's lane). The collision case is a fork-then-merge
        // diamond: two branches (B and C) both inherit the same root's lane,
        // so without reassignment they'd share one column and the merge would
        // be invisible. Reassigning the second branch to a fresh spare lane
        // keeps the two incoming edges on distinct columns so the merge jog
        // renders.
        for parent in parents.iter().skip(1) {
            let existing = lane_of.get(parent).copied();
            if existing.is_none() || existing == Some(my_lane) {
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
/// The path starts at the child's point and ends at the parent's point. A
/// parent-anchored fork remains on the child lane through the parent row, then
/// turns into the parent dot there. Other cross-lane edges retain the merge
/// geometry: they jog onto the parent lane at the child row when adjacent or at
/// the final intermediate row when non-adjacent.
fn build_edge_points(
    child_row: usize,
    child_lane: usize,
    parent_row: usize,
    parent_lane: usize,
    transition_at_parent: bool,
) -> Vec<GridPoint> {
    debug_assert!(parent_row > child_row, "parent must be below child");

    // Start at the child's own point.
    let mut points = Vec::with_capacity(parent_row.saturating_sub(child_row).saturating_add(2));
    points.push(GridPoint {
        row: child_row,
        lane: child_lane,
    });

    if child_lane != parent_lane && transition_at_parent {
        // A fork bends in the parent's own row: keep the branch lane vertical
        // through the top half of that row, then enter the parent dot from its
        // right/left side. This is the grid equivalent of the webview's smooth
        // bottom-right/bottom-left quadratic.
        for r in child_row.saturating_add(1)..=parent_row {
            points.push(GridPoint {
                row: r,
                lane: child_lane,
            });
        }
        points.push(GridPoint {
            row: parent_row,
            lane: parent_lane,
        });
        return points;
    }

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
    // For an ADJACENT cross-lane edge (`parent_row == child_row + 1`) the jog
    // begins at the child's own row, so its source-lane point would duplicate
    // the child start point — skip it and keep a single transition point.
    if child_lane != parent_lane {
        if run_end > child_row {
            points.push(GridPoint {
                row: run_end,
                lane: child_lane,
            });
        }
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
    use std::collections::VecDeque;

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

    // --- Phase 2: compute each component's compact local geometry -----------------
    // Local geometry must be known BEFORE global interval coloring: a component
    // with an active fork occupies more than its base lane. Coloring only bases
    // lets a later disconnected component collide with that still-live branch.
    let git_present = comp_is_git.iter().any(|&g| g);
    let mut members_by_component: Vec<Vec<String>> = vec![Vec::new(); comp_start_end.len()];
    for key in nodes_newest_first {
        if let Some(cid) = comp_id_of_key.get(key).copied() {
            members_by_component[cid].push(key.clone());
        }
    }

    let mut local_lanes_by_component: Vec<HashMap<String, usize>> =
        Vec::with_capacity(comp_start_end.len());
    let mut op_lane_rank_by_component: Vec<HashMap<usize, usize>> =
        Vec::with_capacity(comp_start_end.len());
    let mut comp_op_width: Vec<usize> = Vec::with_capacity(comp_start_end.len());
    for members in &members_by_component {
        let local = if members.is_empty() {
            HashMap::new()
        } else {
            let topo = topological_order(members, parents_of);
            let uncompacted = compute_lane_map(&topo, parents_of);
            // `compute_lane_map` never frees a lane, so sequential branches
            // inside one component would each keep a permanent column. Compact
            // only disjoint geometry; overlapping branches remain distinct.
            compact_component_lanes(members, &uncompacted, parents_of, &row_of_key, is_git)
        };

        // Git is globally remapped to lane 0. Rank only the operation lanes so
        // every component's operation block is dense even when a local lane was
        // occupied solely by Git.
        let mut op_local_lanes: Vec<usize> = members
            .iter()
            .filter(|key| !is_git(key))
            .filter_map(|key| local.get(key).copied())
            .collect();
        op_local_lanes.sort_unstable();
        op_local_lanes.dedup();
        let mut rank_by_lane = HashMap::with_capacity(op_local_lanes.len());
        for (rank, lane) in op_local_lanes.iter().copied().enumerate() {
            let _: Option<usize> = rank_by_lane.insert(lane, rank);
        }
        comp_op_width.push(op_local_lanes.len());
        op_lane_rank_by_component.push(rank_by_lane);
        local_lanes_by_component.push(local);
    }

    // --- Phase 3: width-aware greedy interval coloring ----------------------------
    // Components are inclusive display-row intervals. Allocate each active
    // component's entire dense operation-lane block, releasing the block only
    // after its last row. This permits exact reuse for sequential sessions while
    // preventing a narrow component from landing on an active component's fork
    // lane. Git itself is pinned separately to final lane 0.
    let mut comp_ids_sorted_by_start: Vec<usize> = comp_start_end
        .iter()
        .enumerate()
        .map(|(id, _)| id)
        .collect();
    comp_ids_sorted_by_start.sort_by_key(|&id| comp_start_end[id]);

    // Active blocks are `(inclusive_end_row, base_lane, width)` in final lane
    // space. Lane 0 is reserved exactly once when Git is present.
    let minimum_op_lane = usize::from(git_present);
    let mut active_blocks: Vec<(usize, usize, usize)> = Vec::new();
    let mut comp_base_lane: Vec<usize> = vec![minimum_op_lane; comp_start_end.len()];

    for &cid in &comp_ids_sorted_by_start {
        let start = comp_start_end[cid].0;
        let end = comp_start_end[cid].1;
        active_blocks.retain(|(active_end, _, _)| *active_end >= start);

        let width = comp_op_width[cid];
        if width == 0 {
            continue;
        }

        let mut occupied: Vec<(usize, usize)> = active_blocks
            .iter()
            .map(|(_, base, active_width)| (*base, base.saturating_add(*active_width)))
            .collect();
        occupied.sort_unstable();
        let mut base = minimum_op_lane;
        for (occupied_start, occupied_end) in occupied {
            if base.saturating_add(width) <= occupied_start {
                break;
            }
            if base < occupied_end {
                base = occupied_end;
            }
        }
        comp_base_lane[cid] = base;
        active_blocks.push((end, base, width));
    }

    // --- Phase 4: map compact local lanes into their allocated global blocks -------
    let mut lane_of: HashMap<String, usize> = HashMap::with_capacity(nodes_newest_first.len());
    for (cid, members) in members_by_component.iter().enumerate() {
        let local = &local_lanes_by_component[cid];
        let rank_by_lane = &op_lane_rank_by_component[cid];
        let base = comp_base_lane[cid];
        for key in members {
            if is_git(key) {
                let _ = lane_of.insert(key.clone(), 0);
            } else {
                let local_lane = *local.get(key).unwrap_or(&0);
                let rank = *rank_by_lane.get(&local_lane).unwrap_or(&0);
                let _ = lane_of.insert(key.clone(), base.saturating_add(rank));
            }
        }
    }

    lane_of
}

/// Merge lanes inside ONE component whose rendered row usage never overlaps.
///
/// [`compute_lane_map`] gives every root, fork branch, and colliding merge
/// parent its own fresh lane and never frees them, so sequential branches in a
/// single component — branches joined by explicit Git links, repeated fork
/// diamonds, sequential subagent forks — each claim a permanent column even
/// though their vertical segments occupy disjoint display rows. This pass
/// computes each lane's exact rendered row usage (node dots plus edge runs,
/// mirroring the above/below/transition geometry rules)
/// and collapses a lane into the first earlier lane whose usage-row span is
/// strictly disjoint, so a finished branch's lane becomes reusable.
///
/// The merge check is conservative and exact: two lanes merge only when their
/// usage-row spans are disjoint, so a merged column never carries two different
/// branches' segments at the same row (no crossing lines on one lane). Fork
/// siblings and merge parents whose branches overlap in time stay on distinct
/// lanes. After merging, the surviving lanes are renumbered in ascending
/// survivor order so the compacted local lane ids are contiguous (0..=width-1)
/// instead of leaving a gap at every collapsed lane. When no lane can merge,
/// every lane survives in order, so the assignment is byte-identical to the
/// uncompacted one.
///
/// Deterministic: `members` is the component in canonical newest-first display
/// order, spans are folded from fixed edges, and merging walks lane indices in
/// ascending order — no `HashMap` iteration order leaks into the mapping.
#[expect(
    clippy::indexing_slicing,
    reason = "lane indices are bounded by the component's lane/row counts"
)]
fn compact_component_lanes(
    members: &[String],
    lane_of: &HashMap<String, usize>,
    parents_of: &impl Fn(&str) -> Vec<String>,
    row_of_key: &HashMap<String, usize>,
    is_git: &impl Fn(&str) -> bool,
) -> HashMap<String, usize> {
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for key in members {
        for parent in parents_of(key) {
            let count = child_counts.entry(parent).or_default();
            *count = count.saturating_add(1);
        }
    }
    // Per-lane used-row span (lo, hi): initialize one entry per lane in use.
    let mut lane_spans: Vec<(usize, usize)> = Vec::new();
    for key in members {
        let lane = *lane_of.get(key).unwrap_or(&0);
        while lane_spans.len() <= lane {
            lane_spans.push((usize::MAX, 0));
        }
    }
    // Fold node dots and every edge run into the lanes they touch, using the
    // same geometry `LayoutContext` emits (same-lane run, parent-anchored fork
    // run, adjacent merge jog with no source run, or non-adjacent merge jog at
    // parent_row - 1).
    for key in members {
        let my_lane = *lane_of.get(key).unwrap_or(&0);
        let Some(child_row) = row_of_key.get(key).copied() else {
            continue;
        };
        lane_spans[my_lane] = fold_span(lane_spans[my_lane], child_row);
        for parent in parents_of(key) {
            let Some(parent_row) = row_of_key.get(&parent).copied() else {
                continue;
            };
            if parent_row <= child_row {
                continue; // not a downward edge
            }
            let p_lane = *lane_of.get(&parent).unwrap_or(&my_lane);
            let parent_anchored = child_counts.get(&parent).copied().unwrap_or(0) > 1
                || (!is_git(key) && is_git(&parent));
            if my_lane == p_lane {
                lane_spans[my_lane] = fold_span(lane_spans[my_lane], parent_row);
            } else if parent_anchored {
                // Parent-anchored fork: the source lane remains live through
                // the parent row's top half; the destination lane is already
                // occupied there by the parent node itself.
                lane_spans[my_lane] = fold_span(lane_spans[my_lane], parent_row);
            } else if parent_row == child_row.saturating_add(1) {
                // Adjacent cross-lane: the jog starts at the child's midpoint,
                // so the source lane carries no run; the destination lane gets
                // the two endpoint halves only.
                lane_spans[p_lane] = fold_span(lane_spans[p_lane], child_row);
                lane_spans[p_lane] = fold_span(lane_spans[p_lane], parent_row);
            } else {
                // Non-adjacent cross-lane: source run down to parent_row - 1,
                // jog, then destination run parent_row - 1..=parent_row.
                lane_spans[my_lane] = fold_span(lane_spans[my_lane], parent_row.saturating_sub(1));
                lane_spans[p_lane] = fold_span(lane_spans[p_lane], parent_row.saturating_sub(1));
                lane_spans[p_lane] = fold_span(lane_spans[p_lane], parent_row);
            }
        }
    }

    // Merge in ascending lane order: a lane collapses into the first earlier
    // lane whose accumulated usage span is strictly disjoint, extending that
    // target's span so later lanes check against everything merged so far.
    let mut remap: Vec<usize> = (0..lane_spans.len()).collect();
    for lane in 1..lane_spans.len() {
        let (lo, hi) = lane_spans[lane];
        for earlier in 0..lane {
            let target = remap[earlier];
            let (elo, ehi) = lane_spans[target];
            if hi < elo || ehi < lo {
                lane_spans[target] = fold_span(lane_spans[target], lo);
                lane_spans[target] = fold_span(lane_spans[target], hi);
                remap[lane] = target;
                break;
            }
        }
    }

    // Densify the survivors: merging collapses lanes onto earlier targets, so
    // the surviving target ids are ascending but sparse (every collapsed lane
    // leaves a gap). Renumber the survivors in ascending order to 0..=width-1
    // so the compacted local lane ids are contiguous again. This is a pure
    // relabeling — relative lane order, overlap disjointness, and the merged
    // spans are unchanged.
    let mut survivors: Vec<usize> = remap.clone();
    survivors.sort_unstable();
    survivors.dedup();
    let mut dense_of_survivor: HashMap<usize, usize> = HashMap::with_capacity(survivors.len());
    for (dense, survivor) in survivors.iter().copied().enumerate() {
        let _: Option<usize> = dense_of_survivor.insert(survivor, dense);
    }

    members
        .iter()
        .map(|key| {
            let lane = *lane_of.get(key).unwrap_or(&0);
            (
                key.clone(),
                *dense_of_survivor.get(&remap[lane]).unwrap_or(&0),
            )
        })
        .collect()
}

/// Fold a row index into a running `(min, max)` span.
fn fold_span(span: (usize, usize), row: usize) -> (usize, usize) {
    (span.0.min(row), span.1.max(row))
}

/// Push `v` into `list` if not already present (dedup for per-row lane sets).
fn add_unique<T: PartialEq>(list: &mut Vec<T>, v: T) {
    if !list.contains(&v) {
        list.push(v);
    }
}
