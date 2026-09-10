//! Typed graph identity and resolved edges for one projection stage.

use std::collections::HashMap;

use editchain_core::{GitCommitKey, OpId};

use crate::layout::{compute_lane_assignment, GraphLayout, GraphRow, LayoutContext};
use crate::taxonomy::ChainState;
use crate::RelationKind;

/// Stable graph identity, with repository identity retained for Git commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKey {
    /// An accepted source operation or a group anchored on that operation.
    Op(OpId),
    /// A commit in one repository.
    Git(GitCommitKey),
}

impl NodeKey {
    /// Parse a display key at an external compatibility boundary.
    #[must_use]
    pub fn from_display_str(value: &str) -> Option<Self> {
        OpId::from_display_str(value)
            .map(Self::Op)
            .or_else(|| GitCommitKey::from_display_str(value).map(Self::Git))
    }
}

impl std::fmt::Display for NodeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Op(id) => id.fmt(formatter),
            Self::Git(key) => key.fmt(formatter),
        }
    }
}

/// A structural label supported by source evidence on a resolved parent edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRelation {
    /// Parent in this exact graph stage.
    pub parent: NodeKey,
    /// Provider-neutral meaning of the edge.
    pub kind: RelationKind,
    /// Relationship-note IDs, or producing source-operation IDs for Git links.
    pub evidence: Vec<OpId>,
}

/// One immutable graph stage in display order. Every parent belongs to it.
///
/// Scheduling, Activity filtering, and geometry resolve ancestry through the
/// same typed node contract. A completed view retains this table, so paging
/// and relation labels do not re-interpret source envelopes.
#[derive(Debug)]
pub struct ResolvedGraph {
    keys: Vec<NodeKey>,
    rows: HashMap<NodeKey, ResolvedGraphRow>,
}

#[derive(Debug)]
pub(crate) struct ResolvedGraphRow {
    pub(crate) parents: Vec<NodeKey>,
    pub(crate) relations: Vec<ResolvedRelation>,
    pub(crate) chain_state: ChainState,
}

impl ResolvedGraph {
    pub(crate) fn new(keys: Vec<NodeKey>, rows: HashMap<NodeKey, ResolvedGraphRow>) -> Self {
        Self { keys, rows }
    }

    /// Nodes in the exact order used by layout.
    #[must_use]
    pub fn keys(&self) -> &[NodeKey] {
        &self.keys
    }

    /// Final deduplicated parents, in source precedence order.
    #[must_use]
    pub fn parents(&self, key: NodeKey) -> &[NodeKey] {
        self.rows.get(&key).map_or(&[], |row| &row.parents)
    }

    /// Structural labels whose endpoints both exist in this graph.
    #[must_use]
    pub fn relations(&self, key: NodeKey) -> &[ResolvedRelation] {
        self.rows.get(&key).map_or(&[], |row| &row.relations)
    }

    /// Build geometry from the finalized edge table.
    #[must_use]
    pub fn layout_context(&self) -> LayoutContext {
        let input = LayoutInput::new(self);
        LayoutContext::new_with_chain_state(
            &input.keys,
            &|key| input.parents(key),
            &|key| input.is_git(key),
            &|key| {
                input
                    .rows
                    .get(key)
                    .map_or(ChainState::Active, |row| row.state)
            },
        )
    }

    /// Assign lanes without constructing full edge geometry.
    #[must_use]
    pub fn lane_assignment(&self) -> Vec<GraphRow> {
        let input = LayoutInput::new(self);
        compute_lane_assignment(&input.keys, &|key| input.parents(key), &|key| {
            input.is_git(key)
        })
    }

    /// Build the complete layout for this graph.
    #[must_use]
    pub fn layout(&self) -> GraphLayout {
        let context = self.layout_context();
        let edges = context.edges_for_window(0, self.keys.len());
        GraphLayout {
            rows: context.lanes,
            edges,
        }
    }
}

/// The geometry engine treats formatted labels as opaque keys. It receives
/// one converted table and never parses them back into domain identities.
struct LayoutInput {
    keys: Vec<String>,
    rows: HashMap<String, LayoutRow>,
}

struct LayoutRow {
    parents: Vec<String>,
    state: ChainState,
    is_git: bool,
}

impl LayoutInput {
    fn new(graph: &ResolvedGraph) -> Self {
        let keys = graph.keys.iter().map(ToString::to_string).collect();
        let rows = graph
            .rows
            .iter()
            .map(|(key, row)| {
                (
                    key.to_string(),
                    LayoutRow {
                        parents: row.parents.iter().map(ToString::to_string).collect(),
                        state: row.chain_state,
                        is_git: matches!(key, NodeKey::Git(_)),
                    },
                )
            })
            .collect();
        Self { keys, rows }
    }

    fn parents(&self, key: &str) -> Vec<String> {
        self.rows
            .get(key)
            .map_or_else(Vec::new, |row| row.parents.clone())
    }

    fn is_git(&self, key: &str) -> bool {
        self.rows.get(key).is_some_and(|row| row.is_git)
    }
}
