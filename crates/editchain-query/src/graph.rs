use editchain_core::{Frontier, NodeId, OpId};

/// A frontier map — `(node, boot) -> max_seq` for fast visibility checks.
///
/// This enables "what was known at this checkpoint?" queries without
/// traversing the full DAG.
#[derive(Debug, Clone, Default)]
pub struct FrontierMap {
    frontiers: std::collections::HashMap<(u64, u32), u64>,
}

impl FrontierMap {
    pub fn new() -> Self {
        Self {
            frontiers: std::collections::HashMap::new(),
        }
    }

    /// Build a frontier map from a slice of frontiers.
    pub fn from_frontiers(frontiers: &[Frontier]) -> Self {
        let mut map = Self::new();
        for f in frontiers {
            map.insert(f.node, f.boot, f.max_seq);
        }
        map
    }

    /// Insert a frontier entry.
    pub fn insert(&mut self, node: NodeId, boot: u32, max_seq: u64) {
        self.frontiers.insert((node.0, boot), max_seq);
    }

    /// Check if an operation is visible at this frontier.
    pub fn is_visible(&self, op_id: &OpId) -> bool {
        self.frontiers
            .get(&(op_id.node.0, op_id.boot))
            .map_or(false, |&max_seq| op_id.seq <= max_seq)
    }

    /// Get the max sequence for a given node and boot.
    pub fn max_seq(&self, node: NodeId, boot: u32) -> Option<u64> {
        self.frontiers.get(&(node.0, boot)).copied()
    }

    /// Number of entries in the frontier map.
    pub fn len(&self) -> usize {
        self.frontiers.len()
    }

    /// Returns true if the frontier map is empty.
    pub fn is_empty(&self) -> bool {
        self.frontiers.is_empty()
    }
}

/// A causal cone — the set of ancestor and descendant operations around a seed.
///
/// This is used for context expansion around search hits.
#[derive(Debug, Clone)]
pub struct CausalCone {
    /// The seed operation at the center of the cone.
    pub seed: OpId,
    /// Ancestor operations (prompts, tool starts, command invocations).
    pub ancestors: Vec<OpId>,
    /// Descendant operations (results, file effects, conclusions).
    pub descendants: Vec<OpId>,
}

impl CausalCone {
    pub fn new(seed: OpId) -> Self {
        Self {
            seed,
            ancestors: Vec::new(),
            descendants: Vec::new(),
        }
    }

    /// Total number of operations in the cone (including seed).
    pub fn total_ops(&self) -> usize {
        1 + self.ancestors.len() + self.descendants.len()
    }
}

/// Branch diversity penalty for MMR (Maximal Marginal Relevance).
///
/// Used to avoid returning multiple nearly-identical chunks from one branch.
#[derive(Debug, Clone)]
pub struct DiversityConfig {
    /// Weight for text similarity penalty.
    pub lambda_text: f64,
    /// Weight for graph overlap penalty.
    pub lambda_graph: f64,
}

impl Default for DiversityConfig {
    fn default() -> Self {
        Self {
            lambda_text: 0.3,
            lambda_graph: 0.2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontier_visibility() {
        let mut map = FrontierMap::new();
        map.insert(NodeId(1), 0, 100);

        assert!(map.is_visible(&OpId::new(NodeId(1), 0, 50)));
        assert!(map.is_visible(&OpId::new(NodeId(1), 0, 100)));
        assert!(!map.is_visible(&OpId::new(NodeId(1), 0, 101)));
        assert!(!map.is_visible(&OpId::new(NodeId(2), 0, 50))); // different node
    }

    #[test]
    fn frontier_from_slice() {
        let frontiers = vec![
            Frontier { node: NodeId(1), boot: 0, max_seq: 100 },
            Frontier { node: NodeId(2), boot: 0, max_seq: 200 },
        ];
        let map = FrontierMap::from_frontiers(&frontiers);
        assert_eq!(map.len(), 2);
        assert!(map.is_visible(&OpId::new(NodeId(1), 0, 50)));
        assert!(map.is_visible(&OpId::new(NodeId(2), 0, 150)));
    }

    #[test]
    fn causal_cone_counts() {
        let seed = OpId::new(NodeId(1), 0, 10);
        let mut cone = CausalCone::new(seed);
        cone.ancestors.push(OpId::new(NodeId(1), 0, 8));
        cone.ancestors.push(OpId::new(NodeId(1), 0, 9));
        cone.descendants.push(OpId::new(NodeId(1), 0, 11));

        assert_eq!(cone.total_ops(), 4);
    }
}