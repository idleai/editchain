//! Compatibility adapters for the web-ui-owned retained graph.
//! Viewer wire types remain here until the f31/f43 consumers switch.

use crate::{HistoryRow, LiveBlockMeta};
use history_geometry::live::{GraphNode, GraphRow, RowGeometry};

/// Retained geometry shared with the Dioxus graph.
pub type LiveGraph = history_geometry::live::LiveGraph<LiveBlockMeta>;

impl GraphNode for LiveBlockMeta {
    fn key(&self) -> &String {
        &self.key
    }
    fn node_key(&self) -> &String {
        &self.node_key
    }
    fn parents(&self) -> &Vec<String> {
        &self.parents
    }
    fn sort_time(&self) -> u64 {
        self.sort_time
    }
    fn set_sort_time(&mut self, time: u64) {
        self.sort_time = time;
    }
    fn muted(&self) -> bool {
        !self.chain_state.is_active()
    }
    fn task_protected(&self) -> bool {
        self.task_protected
    }
    fn same_source(&self, other: &Self) -> bool {
        if self.human_stream.is_some() || other.human_stream.is_some() {
            self.human_stream == other.human_stream
        } else {
            self.source_stream
                .as_ref()
                .zip(other.source_stream.as_ref())
                .is_none_or(|(left, right)| left == right)
        }
    }
}

impl GraphRow for HistoryRow {
    fn set_graph(&mut self, graph: RowGeometry, root: bool) {
        self.lane = graph.lane;
        self.above = graph.above;
        self.below = graph.below;
        self.transitions = graph.transitions;
        self.muted_above = graph.muted_above;
        self.muted_below = graph.muted_below;
        self.muted_transitions = graph.muted_transitions;
        if root {
            self.parents = graph.parents;
        }
    }
}
