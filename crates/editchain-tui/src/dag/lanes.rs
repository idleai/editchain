use editchain_core::OpId;
use crate::data::header::OpOrdinal;

/// A single cell in a DAG row — what to draw at one column position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneCell {
    Empty,
    Vertical,
    Node,
    #[allow(dead_code)]
    SelectedNode,
    #[allow(dead_code)]
    Horizontal,
    #[allow(dead_code)]
    BranchLeft,
    #[allow(dead_code)]
    BranchRight,
    #[allow(dead_code)]
    MergeLeft,
    #[allow(dead_code)]
    MergeRight,
    #[allow(dead_code)]
    Crossing,
    #[allow(dead_code)]
    ManyParents,
}

/// One row in the DAG log — corresponds to one operation.
#[derive(Debug, Clone)]
pub struct DagRow {
    /// The ordinal of the operation this row represents.
    #[allow(dead_code)]
    pub op: OpOrdinal,
    /// Lane cells for this row (one per active column).
    pub cells: Vec<LaneCell>,
}

/// State for computing DAG lanes.
///
/// Implements a git-log-style lane rendering algorithm.
///
/// Processing order: newest-first (reverse chronological).
/// For each operation:
/// 1. Find which lane it occupies.
/// 2. Draw the operation marker.
/// 3. Remove it from its lane.
/// 4. Insert parent(s) into lanes.
/// 5. Draw connecting lines.
#[derive(Debug, Clone)]
pub struct LaneState {
    /// Active lanes — each slot holds the OpId currently occupying that column.
    pub active: Vec<Option<OpId>>,
}

impl LaneState {
    pub fn new() -> Self {
        Self { active: Vec::new() }
    }

    /// Compute DAG rows for a set of operations given their parent relationships.
    ///
    /// `ordinals`: operations in display order (oldest-first).
    /// `op_id_of`: function to get OpId from an ordinal.
    /// `parents_of`: function to get parent ordinals for an ordinal.
    pub fn compute_rows(
        ordinals: &[OpOrdinal],
        op_id_of: impl Fn(OpOrdinal) -> OpId,
        parents_of: impl Fn(OpOrdinal) -> Vec<OpOrdinal>,
    ) -> Vec<DagRow> {
        let mut state = LaneState::new();
        let mut rows = Vec::with_capacity(ordinals.len());

        // Process newest-first for parent expansion, but build rows oldest-first.
        // We iterate ordinals in reverse (newest first) to build lane state,
        // then assign cells per row.

        // Step 1: Build a map from ordinal -> lane assignment
        // We process newest -> oldest
        let mut lane_of_op: std::collections::HashMap<OpId, usize> = std::collections::HashMap::new();

        for &ord in ordinals.iter().rev() {
            let op_id = op_id_of(ord);
            let parents = parents_of(ord);

            // Find which lane this operation occupies
            let lane_idx = if let Some(&idx) = lane_of_op.get(&op_id) {
                // Already assigned (shouldn't happen for unique ops)
                idx
            } else {
                // Allocate a new lane
                let idx = state.active.len();
                state.active.push(Some(op_id));
                lane_of_op.insert(op_id, idx);
                idx
            };

            // Remove this op from its lane and insert parents
            state.active[lane_idx] = None;

            match parents.len() {
                0 => {
                    // Root — no parents to insert
                }
                1 => {
                    // Single parent — keep in same lane
                    let parent_id = op_id_of(parents[0]);
                    state.active[lane_idx] = Some(parent_id);
                    lane_of_op.entry(parent_id).or_insert(lane_idx);
                }
                2 => {
                    // Two parents — keep one in current lane, allocate another
                    let p0_id = op_id_of(parents[0]);
                    let p1_id = op_id_of(parents[1]);

                    state.active[lane_idx] = Some(p0_id);
                    lane_of_op.entry(p0_id).or_insert(lane_idx);

                    // Find or allocate a lane for the second parent
                    lane_of_op.entry(p1_id).or_insert_with(|| {
                        let p1_lane = find_spare_lane(&state.active, lane_idx);
                        if p1_lane < state.active.len() {
                            state.active[p1_lane] = Some(p1_id);
                        } else {
                            state.active.push(Some(p1_id));
                        }
                        p1_lane
                    });
                }
                _ => {
                    // Many parents — just keep first in lane
                    if let Some(&p0) = parents.first() {
                        let p0_id = op_id_of(p0);
                        state.active[lane_idx] = Some(p0_id);
                        lane_of_op.entry(p0_id).or_insert(lane_idx);
                    }
                }
            }
        }

        // Step 2: Build rows oldest-first with cell assignments
        // We need to track which lanes are "active" at each row going forward.
        let mut active_at_row: Vec<Vec<Option<OpId>>> = Vec::with_capacity(ordinals.len());

        // Reset lane state for forward pass
        let mut forward_active: Vec<Option<OpId>> = Vec::new();

        for &ord in ordinals.iter() {
            let op_id = op_id_of(ord);
            let parents = parents_of(ord);

            // Ensure enough lanes
            let op_lane = lane_of_op.get(&op_id).copied().unwrap_or(0);
            while forward_active.len() <= op_lane {
                forward_active.push(None);
            }

            // Record current active state
            active_at_row.push(forward_active.clone());

            // Advance: remove this op, add parents
            forward_active[op_lane] = None;

            match parents.len() {
                0 => {}
                1 => {
                    let p0_id = op_id_of(parents[0]);
                    if let Some(&p0_lane) = lane_of_op.get(&p0_id) {
                        while forward_active.len() <= p0_lane {
                            forward_active.push(None);
                        }
                        forward_active[p0_lane] = Some(p0_id);
                    }
                }
                2 => {
                    let p0_id = op_id_of(parents[0]);
                    let p1_id = op_id_of(parents[1]);

                    if let Some(&p0_lane) = lane_of_op.get(&p0_id) {
                        while forward_active.len() <= p0_lane {
                            forward_active.push(None);
                        }
                        forward_active[p0_lane] = Some(p0_id);
                    }
                    if let Some(&p1_lane) = lane_of_op.get(&p1_id) {
                        while forward_active.len() <= p1_lane {
                            forward_active.push(None);
                        }
                        forward_active[p1_lane] = Some(p1_id);
                    }
                }
                _ => {}
            }
        }

        // Step 3: Build DagRow cells from active_at_row states
        for (i, &ord) in ordinals.iter().enumerate() {
            let op_id = op_id_of(ord);
            let op_lane = lane_of_op.get(&op_id).copied().unwrap_or(0);
            let active = &active_at_row[i];

            let num_cols = active.len().max(op_lane + 1);
            let mut cells = Vec::with_capacity(num_cols);

            for col in 0..num_cols {
                let is_op_col = col == op_lane;
                let has_op_above = col < active.len() && active[col].is_some();

                if is_op_col {
                    cells.push(LaneCell::Node);
                } else if has_op_above {
                    cells.push(LaneCell::Vertical);
                } else {
                    cells.push(LaneCell::Empty);
                }
            }

            rows.push(DagRow { op: ord, cells });
        }

        rows
    }
}

/// Find a spare lane index, preferring one near `preferred`.
fn find_spare_lane(active: &[Option<OpId>], preferred: usize) -> usize {
    if preferred < active.len() && active[preferred].is_none() {
        return preferred;
    }
    if let Some(i) = active.iter().position(|slot| slot.is_none()) {
        return i;
    }
    active.len() // allocate new
}
