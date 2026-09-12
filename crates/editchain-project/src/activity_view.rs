//! Complete Activity assembly, presentation coordinates, and source ownership.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use editchain_core::RepositoryId;

use crate::activity::{self, ActivityRowAnnotation};
use crate::layout::LayoutContext;
use crate::{
    canonical_op_id, CollapsedProjection, HistoryNode, HistoryProjection, NodeKey, ResolvedGraph,
};

/// Supplies display content without choosing hierarchy or row coordinates.
///
/// Details are flat leaves belonging to the supplied node. Activity grouping
/// decides where those leaves appear in the bounded presentation tree.
pub trait ActivityPresentation {
    /// Adapter-owned content, such as display labels and file-change details.
    type Row;

    /// Content for an original activity nested in a work group.
    fn activity(&self, node: &HistoryNode) -> Self::Row;

    /// Stable detail/member order for an ordinary or inner bundle node.
    fn details(&self, node: &HistoryNode) -> Vec<Self::Row>;
}

/// Why a known source has no row in this Activity view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmissionReason {
    /// Its representative carries no usable source time.
    UnknownTime,
    /// Its representative is low-level trace evidence.
    Trace,
    /// Its repository is excluded by the view's repository selection.
    HiddenRepository,
    /// A relationship fact has no unambiguous visible anchor.
    UnresolvedRelationship,
}

/// The exact visible owner or omission of a source identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDisposition {
    /// Index of the containing top-level Activity entry.
    Row(usize),
    /// The source remains available through its identity but has no row.
    Omitted(OmissionReason),
}

/// One immutable depth-first descendant within a top-level Activity block.
#[derive(Debug)]
pub struct PresentationRow<R> {
    content: R,
    parent_relative: usize,
    depth: u8,
    descendant_count: usize,
}

impl<R> PresentationRow<R> {
    /// Content supplied by the presentation adapter.
    #[must_use]
    pub const fn content(&self) -> &R {
        &self.content
    }

    /// Direct parent's slot relative to the top-level row at zero.
    #[must_use]
    pub const fn parent_relative(&self) -> usize {
        self.parent_relative
    }

    /// Hierarchy depth, with the top-level row at zero.
    #[must_use]
    pub const fn depth(&self) -> u8 {
        self.depth
    }

    /// Number of consecutive depth-first descendants after this row.
    #[must_use]
    pub const fn descendant_count(&self) -> usize {
        self.descendant_count
    }
}

/// One top-level graph node with its presentation descendants and annotations.
#[derive(Debug)]
pub struct ActivityEntry<R> {
    node: HistoryNode,
    annotation: ActivityRowAnnotation,
    descendants: Vec<PresentationRow<R>>,
}

impl<R> ActivityEntry<R> {
    /// Immutable semantic node represented by this entry.
    #[must_use]
    pub const fn node(&self) -> &HistoryNode {
        &self.node
    }

    /// Markers and promotion computed after all Activity contractions.
    #[must_use]
    pub const fn annotation(&self) -> &ActivityRowAnnotation {
        &self.annotation
    }

    /// All descendants, in their fixed expanded slot order.
    #[must_use]
    pub fn descendants(&self) -> &[PresentationRow<R>] {
        &self.descendants
    }

    /// Direct children of a relative slot, read from the same tree as paging.
    pub fn children(&self, parent_relative: usize) -> impl Iterator<Item = &R> {
        let count = if parent_relative == 0 {
            self.descendants.len()
        } else {
            self.descendants
                .get(parent_relative.saturating_sub(1))
                .map_or(0, |parent| parent.descendant_count)
        };
        let end = parent_relative.saturating_add(count);
        let mut remaining = self.descendants.get(parent_relative..end).unwrap_or(&[]);
        std::iter::from_fn(move || {
            let child = remaining.first()?;
            remaining = remaining
                .get(child.descendant_count.saturating_add(1)..)
                .unwrap_or(&[]);
            Some(&child.content)
        })
    }
}

/// Absolute expansion interval derived from the fixed presentation tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpansionSpan {
    /// Fully expanded slot occupied by the parent.
    pub row: usize,
    /// Consecutive descendant slots controlled by that parent.
    pub descendant_count: usize,
}

/// Immutable Activity view shared by paging, geometry, expansion, and search.
#[derive(Debug)]
pub struct ActivityView<R> {
    entries: Vec<ActivityEntry<R>>,
    graph: ResolvedGraph,
    starts: Vec<usize>,
    omissions: HashMap<NodeKey, OmissionReason>,
    collapsed: Arc<CollapsedProjection>,
    layout: OnceLock<ActivityLayout>,
    source_rows: OnceLock<HashMap<NodeKey, usize>>,
}

#[derive(Debug)]
struct ActivityLayout {
    context: LayoutContext,
    max_lane: usize,
}

impl HistoryNode {
    /// Original activities represented by this row, excluding detail leaves.
    #[must_use]
    pub fn represented_activity_count(&self) -> usize {
        match self {
            Self::ExecuteBundle { member_nodes, .. }
            | Self::PlanBundle { member_nodes, .. }
            | Self::WorkGroup { member_nodes, .. } => member_nodes
                .iter()
                .map(Self::represented_activity_count)
                .fold(0usize, usize::saturating_add),
            Self::EditOperation { .. } | Self::CollapsedImport { .. } | Self::GitCommit { .. } => 1,
        }
    }
}

impl HistoryProjection {
    /// Present current live items with their details, without contracting
    /// separate logical identities into historical work groups.
    #[must_use]
    pub fn build_item_view<P: ActivityPresentation>(
        &self,
        presentation: &P,
    ) -> ActivityView<P::Row> {
        let (nodes, omissions) = self.activity_nodes_with_omissions();
        self.assemble_activity_view(nodes, omissions, presentation)
    }

    /// Build the complete fixed Activity view, in the canonical pass order.
    ///
    /// Repository selection precedes Activity grouping. The adapter supplies
    /// leaf content; this layer owns grouping, hierarchy, and all coordinates.
    #[must_use]
    pub fn build_activity_view<P: ActivityPresentation>(
        &self,
        repository_visible: impl Fn(RepositoryId) -> bool,
        presentation: &P,
    ) -> ActivityView<P::Row> {
        let (nodes, mut omissions) = self.activity_nodes_with_omissions();
        let nodes = nodes
            .into_iter()
            .filter(|node| {
                if node.repository().is_some_and(|id| !repository_visible(id)) {
                    let _: Option<OmissionReason> =
                        omissions.insert(node.key(), OmissionReason::HiddenRepository);
                    false
                } else {
                    true
                }
            })
            .collect::<Vec<_>>();
        let structural = self.structural_row_keys(&nodes);
        let nodes = activity::inline_context_compaction_checkpoints(nodes, &structural);
        let nodes = activity::bundle_activity_plan_repeats(nodes, &structural);
        let annotations = activity::annotate_activity_rows(&nodes);
        let nodes = activity::bundle_activity_execute_runs(nodes, &annotations, &structural);
        let nodes = activity::bundle_claude_response_tool_fragments(nodes, &structural);
        let nodes = activity::bundle_activity_work_groups(nodes, &structural);
        self.assemble_activity_view(nodes, omissions, presentation)
    }

    fn assemble_activity_view<P: ActivityPresentation>(
        &self,
        nodes: Vec<HistoryNode>,
        omissions: HashMap<NodeKey, OmissionReason>,
        presentation: &P,
    ) -> ActivityView<P::Row> {
        let graph = self.resolved_graph(&nodes);
        let annotations = activity::annotate_activity_rows(&nodes);
        let entries: Vec<_> = nodes
            .into_iter()
            .zip(annotations)
            .map(|(node, annotation)| {
                let descendants = presentation_rows(&node, presentation);
                ActivityEntry {
                    node,
                    annotation,
                    descendants,
                }
            })
            .collect();
        let mut starts = Vec::with_capacity(entries.len().saturating_add(1));
        starts.push(0usize);
        for entry in &entries {
            let next = starts
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(1)
                .saturating_add(entry.descendants.len());
            starts.push(next);
        }
        ActivityView {
            entries,
            graph,
            starts,
            omissions,
            collapsed: Arc::clone(&self.collapsed_projection),
            layout: OnceLock::new(),
            source_rows: OnceLock::new(),
        }
    }

    pub(crate) fn activity_nodes_with_omissions(
        &self,
    ) -> (Vec<HistoryNode>, HashMap<NodeKey, OmissionReason>) {
        let nodes = self.ordered_nodes();
        let graph = self.resolved_graph(&nodes);
        let mut structural = self.structural_row_keys(&nodes);
        // A dated producing row survives even if its linked commit is unavailable.
        structural.extend(
            self.git
                .links()
                .values()
                .flatten()
                .filter(|link| link.kind == editchain_core::GitLinkKind::ProducedBy)
                .filter_map(|link| self.visible_op_id(link.source).map(NodeKey::Op)),
        );
        let omissions = nodes
            .iter()
            .filter_map(|node| {
                crate::view::omission_reason(node, &structural).map(|reason| (node.key(), reason))
            })
            .collect();
        (crate::view::apply(nodes, &graph, &structural), omissions)
    }
}

impl<R> ActivityView<R> {
    /// Top-level entries in display order.
    #[must_use]
    pub fn entries(&self) -> &[ActivityEntry<R>] {
        &self.entries
    }

    /// Final graph used by both row metadata and geometry.
    #[must_use]
    pub const fn graph(&self) -> &ResolvedGraph {
        &self.graph
    }

    /// Expanded starts for each top-level entry, followed by a total sentinel.
    #[must_use]
    pub fn starts(&self) -> &[usize] {
        &self.starts
    }

    /// Number of slots when every presentation row is expanded.
    #[must_use]
    pub fn expanded_total(&self) -> usize {
        self.starts.last().copied().unwrap_or(0)
    }

    /// Top-level descendant counts, derived directly from the presentation tree.
    #[must_use]
    pub fn sub_op_counts(&self) -> Vec<usize> {
        self.entries
            .iter()
            .map(|entry| entry.descendants.len())
            .collect()
    }

    /// Every disclosure interval in expanded row order.
    #[must_use]
    pub fn expansion_spans(&self) -> Vec<ExpansionSpan> {
        let mut spans = Vec::new();
        for (entry, &start) in self.entries.iter().zip(&self.starts) {
            if !entry.descendants.is_empty() {
                spans.push(ExpansionSpan {
                    row: start,
                    descendant_count: entry.descendants.len(),
                });
            }
            for (index, child) in entry.descendants.iter().enumerate() {
                if child.descendant_count > 0 {
                    spans.push(ExpansionSpan {
                        row: start.saturating_add(1).saturating_add(index),
                        descendant_count: child.descendant_count,
                    });
                }
            }
        }
        spans
    }

    /// Existing geometry, absent until requested after first paint.
    #[must_use]
    pub fn layout(&self) -> Option<&LayoutContext> {
        self.layout.get().map(|layout| &layout.context)
    }

    /// Build graph geometry once, without changing rows or coordinates.
    #[must_use]
    pub fn ensure_layout(&self) -> &LayoutContext {
        &self
            .layout
            .get_or_init(|| {
                let context = self.graph.layout_context();
                let max_lane = context.lanes.iter().map(|row| row.lane).max().unwrap_or(0);
                ActivityLayout { context, max_lane }
            })
            .context
    }

    /// Highest assigned lane, or zero before geometry is requested.
    #[must_use]
    pub fn max_lane(&self) -> usize {
        self.layout.get().map_or(0, |layout| layout.max_lane)
    }

    /// Resolve an accepted operation or observed Git commit in this exact view.
    /// Unknown identities return `None`; omitted evidence retains its reason.
    #[must_use]
    pub fn source_disposition(&self, source: NodeKey) -> Option<SourceDisposition> {
        let rows = self.source_rows.get_or_init(|| self.build_source_rows());
        if let Some(&row) = rows.get(&source) {
            return Some(SourceDisposition::Row(row));
        }
        let canonical = match source {
            NodeKey::Op(id) => {
                if self.collapsed.unresolved_relations.contains(&id) {
                    return Some(SourceDisposition::Omitted(
                        OmissionReason::UnresolvedRelationship,
                    ));
                }
                NodeKey::Op(canonical_op_id(
                    id,
                    &self.collapsed.representative,
                    &self.collapsed.present,
                )?)
            }
            NodeKey::Git(_) => source,
        };
        rows.get(&canonical)
            .copied()
            .map(SourceDisposition::Row)
            .or_else(|| {
                self.omissions
                    .get(&canonical)
                    .copied()
                    .map(SourceDisposition::Omitted)
            })
    }

    /// Top-level owner of a searchable source, if visible.
    #[must_use]
    pub fn source_row(&self, source: NodeKey) -> Option<usize> {
        match self.source_disposition(source)? {
            SourceDisposition::Row(row) => Some(row),
            SourceDisposition::Omitted(_) => None,
        }
    }

    fn build_source_rows(&self) -> HashMap<NodeKey, usize> {
        let mut rows = HashMap::with_capacity(self.entries.len());
        for (row, entry) in self.entries.iter().enumerate() {
            let _: Option<usize> = rows.insert(entry.node.key(), row);
            for op in entry.node.sub_ops() {
                let _: Option<usize> = rows.insert(NodeKey::Op(op.id), row);
            }
        }
        rows
    }
}

fn presentation_rows<P: ActivityPresentation>(
    node: &HistoryNode,
    adapter: &P,
) -> Vec<PresentationRow<P::Row>> {
    let HistoryNode::WorkGroup { member_nodes, .. } = node else {
        return detail_rows(adapter.details(node), 0, 1).collect();
    };
    let mut rows = Vec::new();
    for member in member_nodes {
        let relative = rows.len().saturating_add(1);
        let details = adapter.details(member);
        rows.push(PresentationRow {
            content: adapter.activity(member),
            parent_relative: 0,
            depth: 1,
            descendant_count: details.len(),
        });
        rows.extend(detail_rows(details, relative, 2));
    }
    rows
}

fn detail_rows<R>(
    details: Vec<R>,
    parent_relative: usize,
    depth: u8,
) -> impl Iterator<Item = PresentationRow<R>> {
    details.into_iter().map(move |content| PresentationRow {
        content,
        parent_relative,
        depth,
        descendant_count: 0,
    })
}
