//! Activity-view row assembly and protocol presentation metadata.

use super::details::payload_text;
use super::{ExpandedChildRow, HistoryWindowOptions, Workspace, WorkspaceBackend};
use editchain_core::{GitOid, Op, OpId, OpKind, Payload, RepositoryId, Tags};
use editchain_project::activity::{SessionSummaryMarker, WorkUnitMarker};
use editchain_project::activity_view::ActivityPresentation;
use editchain_project::taxonomy::{ActivityKind, Outcome, RecordRole, Visibility};
use editchain_protocol::{
    ContentTextDto, ExpansionSpanDto, FileChangeDto, HistoryRow, HistoryWindow, ParentRelationDto,
    ParentRelationKind, RowContentDto, SessionSummaryDto, SubOpSummary, WorkUnitDto,
};
use std::collections::HashMap;

fn compact_continuity_key(key: &str) -> String {
    format!("item:{}", blake3::hash(key.as_bytes()).to_hex())
}

fn child_continuity_key(
    projection: &editchain_project::HistoryProjection,
    child: &ExpandedChildRow,
    parent: &str,
    ordinal: usize,
) -> String {
    let mut parts = child.op_id.split(':');
    let id = (|| {
        Some(OpId {
            node: editchain_core::NodeId(parts.next()?.parse().ok()?),
            boot: parts.next()?.parse().ok()?,
            seq: parts.next()?.parse().ok()?,
        })
    })();
    let logical = id.and_then(|id| projection.continuity_key(id));
    let owner = logical.unwrap_or_else(|| {
        if child.op_id.is_empty() {
            parent
        } else {
            &child.op_id
        }
    });
    let suffix = child.file_change.as_ref().map_or_else(
        || format!("{}:{ordinal}", child.kind),
        |change| format!("file:{}", change.path),
    );
    compact_continuity_key(&format!("child:{owner}:{suffix}"))
}

impl Workspace {
    /// Get a window from the fixed opened Activity view.
    ///
    /// # Errors
    ///
    /// Returns an error if cached rows cannot be read. Failures never become
    /// an apparently successful empty history.
    pub fn history_window(
        &mut self,
        options: HistoryWindowOptions,
    ) -> Result<HistoryWindow, Box<dyn std::error::Error>> {
        if let WorkspaceBackend::Cached(snapshot) = &mut self.backend {
            return snapshot.history_window(options.offset, options.limit, options.include_layout);
        }
        Ok(self.projection_history_window(options))
    }

    /// Compute a history window from the complete in-memory projection.
    #[must_use]
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::needless_borrow,
        reason = "expanded-slot prefix sums are bounded by node count; indexing is bounds-checked by partition_point; node is a &editchain_project::HistoryNode reference"
    )]
    pub(super) fn projection_history_window(
        &mut self,
        options: HistoryWindowOptions,
    ) -> HistoryWindow {
        let HistoryWindowOptions {
            offset,
            limit,
            include_layout,
        } = options;
        let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        self.ensure_view_snapshot();
        let Some(snapshot) = self.current_view.as_ref() else {
            return HistoryWindow {
                snapshot_id: self.snapshot_id.clone(),
                rows: Vec::new(),
                total: 0,
                chain_generation: u64::try_from(self.projection.ops().len()).unwrap_or(u64::MAX),
                max_lane: 0,
                sub_op_counts: (offset == 0).then(Vec::new),
                expansion_spans: (offset == 0).then(Vec::new),
                layout_ready: include_layout,
            };
        };
        let filtered = snapshot.entries();
        let ctx = if include_layout {
            Some(snapshot.ensure_layout())
        } else {
            snapshot.layout()
        };

        // The service emits a FIXED fully-expanded depth-first list: every
        // top-level graph node always occupies one parent slot followed by all
        // presentation descendants. Fetch/cache indices therefore never move;
        // collapse/expand is purely a client decision driven by expansion spans.
        let starts = snapshot.starts();
        let expanded_total = snapshot.expanded_total();

        // Find the first top-level node whose expanded block overlaps [offset, end).
        let end_usize = offset_usize.saturating_add(limit_usize);
        let first_node = starts
            .partition_point(|&s| s <= offset_usize)
            .saturating_sub(1)
            .min(filtered.len());
        let mut rows: Vec<HistoryRow> = Vec::new();
        for abs_idx in first_node..filtered.len() {
            if starts[abs_idx] >= end_usize {
                break;
            }
            let entry = &filtered[abs_idx];
            let node = entry.node();
            let block_start = starts[abs_idx];
            // Per-row graph geometry from the layout context (absolute row index
            // into the full sorted list).
            let (lane, above, below, transitions, muted_above, muted_below, muted_transitions) =
                ctx.map_or_else(
                    || {
                        (
                            0,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )
                    },
                    |layout| {
                        (
                            layout.lanes.get(abs_idx).map_or(0, |row| row.lane),
                            layout.row_above.get(abs_idx).cloned().unwrap_or_default(),
                            layout.row_below.get(abs_idx).cloned().unwrap_or_default(),
                            layout
                                .row_transitions
                                .get(abs_idx)
                                .cloned()
                                .unwrap_or_default(),
                            layout
                                .row_muted_above
                                .get(abs_idx)
                                .cloned()
                                .unwrap_or_default(),
                            layout
                                .row_muted_below
                                .get(abs_idx)
                                .cloned()
                                .unwrap_or_default(),
                            layout
                                .row_muted_transitions
                                .get(abs_idx)
                                .cloned()
                                .unwrap_or_default(),
                        )
                    },
                );
            let parent_row = block_start;
            let group = node.group();
            let session_meta = self.session_metadata.get(&group).cloned();
            // Emit the parent row if it falls inside the window.
            if block_start >= offset_usize && block_start < end_usize {
                let parents = snapshot
                    .graph()
                    .parents(node.key())
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                let parent_relations = snapshot
                    .graph()
                    .relations(node.key())
                    .iter()
                    .map(|relation| ParentRelationDto {
                        parent: relation.parent.to_string(),
                        kind: protocol_relation_kind(relation.kind),
                    })
                    .collect();
                rows.push(HistoryRow {
                    native_expanded: None,
                    op_id: node.op_id().map(|id| id.to_string()),
                    git_oid: node.git_oid().map(|oid| oid.to_hex()),
                    repository: node.repository().map(|rid| rid.0.to_string()),
                    summary: ContentTextDto::new(node.summary(), false).text,
                    content: Some(row_content_dto(node.display_content())),
                    timestamp_ms: node.timestamp_ms(),
                    group: group.clone(),
                    group_end: filtered
                        .get(abs_idx.saturating_add(1))
                        .is_none_or(|next| next.node().group() != group),
                    node_key: node.node_key(),
                    continuity_key: node
                        .op_id()
                        .and_then(|id| self.projection.continuity_key(id))
                        .map(compact_continuity_key)
                        .unwrap_or_default(),
                    parents,
                    parent_relations,
                    is_submodule: node
                        .repository()
                        .is_some_and(|rid| self.repo_is_submodule(rid)),
                    is_system: node_is_system(&node),
                    author: ContentTextDto::new(node_author(&node), false).text,
                    commit_id: node_commit_id(&node),
                    kind: node.kind(),
                    lane,
                    above,
                    below,
                    transitions,
                    muted_above,
                    muted_below,
                    muted_transitions,
                    sub_ops: entry
                        .children(0)
                        .map(ExpandedChildRow::summary_dto)
                        .collect(),
                    is_subop: false,
                    hierarchy_depth: 0,
                    parent_row: None,
                    subop_kind: None,
                    record_role: node.record_role(),
                    activity_kind: node.activity_kind(),
                    visibility: node.visibility(),
                    outcome: node.outcome(),
                    chain_state: node.chain_state(),
                    turn_id: node.turn_id().map(|id| id.0.to_string()),
                    session_meta: session_meta.clone(),
                    session_summary: entry
                        .annotation()
                        .session_summary
                        .as_ref()
                        .map(session_summary_dto),
                    task_group: None,
                    work_unit: Some(work_unit_dto(&entry.annotation().work_unit)),
                    promoted: entry.annotation().promoted,
                    activity_bundle: node_activity_bundle(node),
                    file_change: None,
                });
            }
            // Emit the fixed depth-first descendant rows immediately after the
            // graph parent. Work groups use depth 1 for activities and depth 2
            // for an activity's pre-existing bundle/detail rows.
            // Lanes passing straight through this sub-op region (between this
            // parent and the next top-level node): any lane with a vertical line
            // leaving this parent downward AND entering the next node from above
            // spans the whole region continuously. Sub-op rows draw these as
            // full-height straight lines with no dot.
            let below_parent = ctx
                .and_then(|layout| layout.row_below.get(abs_idx))
                .map_or(&[][..], Vec::as_slice);
            let above_next = ctx
                .and_then(|layout| layout.row_above.get(abs_idx + 1))
                .map_or(&[][..], Vec::as_slice);
            let region_lanes = intersect_sorted(below_parent, above_next);
            let muted_below_parent = ctx
                .and_then(|layout| layout.row_muted_below.get(abs_idx))
                .map_or(&[][..], Vec::as_slice);
            let muted_above_next = ctx
                .and_then(|layout| layout.row_muted_above.get(abs_idx + 1))
                .map_or(&[][..], Vec::as_slice);
            let muted_region_lanes = intersect_sorted(muted_below_parent, muted_above_next);
            for (i, presentation_row) in entry.descendants().iter().enumerate() {
                let child = presentation_row.content();
                let slot = block_start + 1 + i;
                if slot < offset_usize || slot >= end_usize {
                    continue;
                }
                rows.push(HistoryRow {
                    native_expanded: None,
                    op_id: (!child.op_id.is_empty()).then(|| child.op_id.clone()),
                    git_oid: child.git_oid.clone(),
                    repository: child.repository.clone(),
                    summary: child.summary.clone(),
                    content: Some(child.content.clone()),
                    timestamp_ms: child.timestamp_ms,
                    group: group.clone(),
                    group_end: false,
                    // Nested rows are not graph nodes; stable synthetic keys
                    // keep group-start detection and click routing unambiguous.
                    node_key: format!("{}::child:{i}", node.node_key()),
                    continuity_key: child_continuity_key(
                        &self.projection,
                        child,
                        &node.node_key(),
                        i,
                    ),
                    parents: Vec::new(),
                    parent_relations: Vec::new(),
                    is_submodule: false,
                    is_system: child.is_system,
                    author: child.author.clone(),
                    commit_id: child.commit_id.clone(),
                    kind: child.kind.clone(),
                    // No dot of its own — draw every pass-through lane as a
                    // full-height straight line (both halves meet at midY).
                    lane,
                    above: region_lanes.clone(),
                    below: region_lanes.clone(),
                    transitions: Vec::new(),
                    muted_above: muted_region_lanes.clone(),
                    muted_below: muted_region_lanes.clone(),
                    muted_transitions: Vec::new(),
                    sub_ops: entry
                        .children(i.saturating_add(1))
                        .map(ExpandedChildRow::summary_dto)
                        .collect(),
                    is_subop: true,
                    hierarchy_depth: presentation_row.depth(),
                    parent_row: Some(parent_row.saturating_add(presentation_row.parent_relative())),
                    subop_kind: Some(subop_semantic_class(&child.kind)),
                    record_role: child.record_role,
                    activity_kind: child.activity_kind,
                    visibility: child.visibility,
                    outcome: child.outcome,
                    chain_state: child.chain_state,
                    turn_id: child.turn_id.clone(),
                    session_meta: session_meta.clone(),
                    session_summary: None,
                    task_group: None,
                    work_unit: None,
                    promoted: child.promoted,
                    activity_bundle: child.activity_bundle.clone(),
                    file_change: child.file_change.clone(),
                });
            }
        }
        HistoryWindow {
            snapshot_id: self.snapshot_id.clone(),
            rows,
            total: u64::try_from(expanded_total).unwrap_or(u64::MAX),
            chain_generation: u64::try_from(self.projection.ops().len()).unwrap_or(u64::MAX),
            max_lane: snapshot.max_lane(),
            // The renderer always establishes snapshot state from offset zero;
            // ship the O(V) expansion index once for that snapshot, not with
            // every O(window) page.
            sub_op_counts: (offset == 0).then(|| snapshot.sub_op_counts()),
            expansion_spans: (offset == 0).then(|| {
                snapshot
                    .expansion_spans()
                    .into_iter()
                    .map(expansion_span_dto)
                    .collect()
            }),
            layout_ready: snapshot.layout().is_some(),
        }
    }

    /// Build and cache the immutable fixed Activity-view snapshot.
    pub(super) fn ensure_view_snapshot(&mut self) {
        if self.current_view.is_some() {
            return;
        }
        let presentation = ServicePresentation {
            agent_changes: &self.agent_file_changes,
            git_changes: &self.git_file_changes,
        };
        self.current_view = Some(self.projection.build_activity_view(
            |repository| !self.repo_is_submodule(repository),
            &presentation,
        ));
    }

    pub(super) fn prepare_live_item_view(&mut self) {
        let presentation = ServicePresentation {
            agent_changes: &self.agent_file_changes,
            git_changes: &self.git_file_changes,
        };
        self.current_view = Some(self.projection.build_item_view(&presentation));
    }
}

/// Whether a history node is a system-generated artifact (tool results, raw
/// import records) rather than user-facing text.
///
/// The viewer uses this to dim or hide such rows. It is derived from the node's
/// kind — not from sniffing the summary text — so it stays correct regardless
/// of content.
#[must_use]
fn node_is_system(node: &editchain_project::HistoryNode) -> bool {
    match node {
        editchain_project::HistoryNode::EditOperation { op, .. } => {
            matches!(op.kind, OpKind::Tool(_) | OpKind::Import(_))
        }
        // Collapsed imports fold a raw import + its children into one node; the
        // dominant child kind tells us whether it is user-facing text or a
        // system artifact.
        editchain_project::HistoryNode::CollapsedImport { kind, .. } => {
            matches!(
                kind.as_str(),
                "tool" | "import" | "token_count" | "token_usage_record"
            )
        }
        // Execute-run bundles summarize tool/command rows: dim them like the
        // individual tool rows they fold.
        editchain_project::HistoryNode::ExecuteBundle { .. } => true,
        // Work groups and Plan-repeat bundles are navigational/prose-first
        // summary rows; Git is likewise user-facing source history.
        editchain_project::HistoryNode::WorkGroup { .. }
        | editchain_project::HistoryNode::PlanBundle { .. }
        | editchain_project::HistoryNode::GitCommit { .. } => false,
    }
}

/// Author display value for a history node.
///
/// Git commits show the commit author's name. `EditChain` ops have no stored
/// author name (their actor is a derived hash), so they show a tag-derived
/// label (`human` / `agent` / `system`) so the Author column reads uniformly
/// across both row types instead of being blank for ops.
#[must_use]
fn node_author(node: &editchain_project::HistoryNode) -> String {
    match node {
        editchain_project::HistoryNode::EditOperation { op, .. } => op_author_label(op.tags),
        // Collapsed imports carry their author label directly (derived from the
        // children's tags in the projection), since the raw import op's own tags
        // only carry `IMPORT`.
        editchain_project::HistoryNode::CollapsedImport { author, .. }
        | editchain_project::HistoryNode::ExecuteBundle { author, .. }
        | editchain_project::HistoryNode::PlanBundle { author, .. } => author.clone(),
        editchain_project::HistoryNode::WorkGroup { member_nodes, .. } => if member_nodes
            .iter()
            .all(|member| node_author(member) == "human")
        {
            "human"
        } else {
            "agent"
        }
        .to_string(),
        editchain_project::HistoryNode::GitCommit { commit, .. } => {
            payload_text(&commit.author.name)
        }
    }
}

/// Semantic role/activity for a bundled sub-op row.
///
/// Tool-result sub-ops (Finish stage) are results of the enclosing call;
/// bundled metadata imports are lifecycle/system records. Everything else
/// stays conservatively unknown.
#[must_use]
fn sub_op_meta(op: &Op) -> (RecordRole, ActivityKind) {
    match &op.kind {
        OpKind::Tool(t) if matches!(t.stage, editchain_core::op::ToolStage::Finish) => {
            (RecordRole::Result, ActivityKind::Execute)
        }
        OpKind::Tool(_) => (RecordRole::Action, ActivityKind::Execute),
        OpKind::Import(_) => (RecordRole::Lifecycle, ActivityKind::System),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::Command(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => (RecordRole::Unknown, ActivityKind::Unknown),
    }
}

/// Build the bundled sub-op summaries for a row from its attached metadata ops.
///
/// Each bundled sub-op is a raw `Import` op tagged `META`. Its summary is the
/// record type (derived from the raw JSONL's `type` field when parseable, else
/// the raw reference text), so the viewer can label each revealed sub-row.
#[must_use]
fn sub_op_summaries(sub_ops: &[std::sync::Arc<Op>]) -> Vec<SubOpSummary> {
    sub_ops
        .iter()
        .map(|op| {
            let (summary, kind) = sub_op_label(op);
            SubOpSummary {
                op_id: op.id.to_string(),
                summary,
                kind,
                timestamp_ms: op.observed_unix_ms().unwrap_or(0),
            }
        })
        .collect()
}

/// Convert a row's Activity annotation into the protocol's wire DTO.
#[must_use]
fn work_unit_dto(marker: &WorkUnitMarker) -> WorkUnitDto {
    WorkUnitDto {
        id: marker.id.clone(),
        is_start: marker.is_start,
        is_end: marker.is_end,
        title: marker
            .title
            .clone()
            .map(|title| ContentTextDto::new(title, false).text),
        count: marker.count,
    }
}

/// Convert a row's whole-session marker into the additive wire DTO.
#[must_use]
const fn session_summary_dto(marker: &SessionSummaryMarker) -> SessionSummaryDto {
    SessionSummaryDto {
        count: marker.count,
    }
}

/// Typed Activity-view bundle metadata for a synthetic Activity row.
///
/// `Some` only for synthetic Activity bundles, carrying the ORIGINAL top-level
/// member count (`member_nodes.len()`, never the flattened
/// metadata-subop count) so the viewer can render faithful bundle labels from
/// structured data without parsing the summary string. Inner execute/plan
/// bundles keep this metadata when nested beneath a work group. `None` for
/// ordinary rows and expandable Activity bundles.
#[must_use]
fn node_activity_bundle(
    node: &editchain_project::HistoryNode,
) -> Option<editchain_protocol::ActivityBundleDto> {
    match node {
        editchain_project::HistoryNode::WorkGroup { .. } => {
            Some(editchain_protocol::ActivityBundleDto {
                kind: editchain_protocol::ActivityBundleKind::WorkGroup,
                member_count: u64::try_from(node.represented_activity_count()).unwrap_or(u64::MAX),
            })
        }
        editchain_project::HistoryNode::ExecuteBundle { member_nodes, .. } => {
            Some(editchain_protocol::ActivityBundleDto {
                kind: editchain_protocol::ActivityBundleKind::ExecuteRun,
                member_count: u64::try_from(member_nodes.len()).unwrap_or(u64::MAX),
            })
        }
        editchain_project::HistoryNode::PlanBundle { member_nodes, .. } => {
            Some(editchain_protocol::ActivityBundleDto {
                kind: editchain_protocol::ActivityBundleKind::PlanRepeat,
                member_count: u64::try_from(member_nodes.len()).unwrap_or(u64::MAX),
            })
        }
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::CollapsedImport { .. }
        | editchain_project::HistoryNode::GitCommit { .. } => None,
    }
}

pub(super) fn row_content_dto(
    content: editchain_project::content::DisplayContent,
) -> RowContentDto {
    RowContentDto {
        tool_label: content
            .tool_label
            .map(|text| ContentTextDto::tool_label(text.text, text.complete)),
        authored_summary: content
            .authored_summary
            .map(|text| ContentTextDto::new(text.text, text.complete)),
        output_preview: content
            .output_preview
            .map(|text| ContentTextDto::new(text.text, text.complete)),
    }
}

fn child_content_dto(op: Option<&Op>, summary: &str) -> RowContentDto {
    let content = op
        .filter(|op| matches!(op.kind, OpKind::Tool(_)))
        .map_or_else(
            || editchain_project::content::DisplayContent::summary(summary.to_owned()),
            |op| editchain_project::content::operation(op, false).display,
        );
    row_content_dto(content)
}

struct ServicePresentation<'a> {
    agent_changes: &'a HashMap<OpId, Vec<FileChangeDto>>,
    git_changes: &'a HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>>,
}

impl ActivityPresentation for ServicePresentation<'_> {
    type Row = ExpandedChildRow;

    fn activity(&self, member: &editchain_project::HistoryNode) -> ExpandedChildRow {
        ExpandedChildRow {
            op_id: member.op_id().map_or_else(String::new, |id| id.to_string()),
            git_oid: member.git_oid().map(|oid| oid.to_hex()),
            repository: member.repository().map(|id| id.0.to_string()),
            summary: ContentTextDto::new(member.summary(), false).text,
            content: row_content_dto(member.display_content()),
            timestamp_ms: member.timestamp_ms(),
            kind: member.kind(),
            author: ContentTextDto::new(node_author(member), false).text,
            commit_id: node_commit_id(member),
            is_system: node_is_system(member),
            record_role: member.record_role(),
            activity_kind: member.activity_kind(),
            visibility: member.visibility(),
            outcome: member.outcome(),
            chain_state: member.chain_state(),
            turn_id: member.turn_id().map(|id| id.0.to_string()),
            promoted: matches!(
                member.outcome(),
                Outcome::Warning | Outcome::Failure | Outcome::Cancelled
            ) || matches!(
                member.activity_kind(),
                ActivityKind::Change | ActivityKind::Verify
            ),
            activity_bundle: node_activity_bundle(member),
            file_change: None,
        }
    }

    fn details(&self, node: &editchain_project::HistoryNode) -> Vec<ExpandedChildRow> {
        let mut rows = flat_op_child_rows(node);
        let changes = node_file_changes(node, self.agent_changes, self.git_changes);
        rows.extend(file_change_rows(&changes, node));
        rows
    }
}

impl ExpandedChildRow {
    fn summary_dto(&self) -> SubOpSummary {
        SubOpSummary {
            op_id: self.op_id.clone(),
            summary: self.summary.clone(),
            kind: self.kind.clone(),
            timestamp_ms: self.timestamp_ms,
        }
    }
}

fn expansion_span_dto(span: editchain_project::activity_view::ExpansionSpan) -> ExpansionSpanDto {
    ExpansionSpanDto {
        row: u64::try_from(span.row).unwrap_or(u64::MAX),
        descendant_count: u64::try_from(span.descendant_count).unwrap_or(u64::MAX),
    }
}

/// File changes represented by one row, recursively collecting synthetic
/// bundle members while keeping ordinary rows and Git commits direct.
fn node_file_changes(
    node: &editchain_project::HistoryNode,
    agent_changes: &HashMap<OpId, Vec<FileChangeDto>>,
    git_changes: &HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>>,
) -> Vec<FileChangeDto> {
    match node {
        editchain_project::HistoryNode::EditOperation { op, .. }
        | editchain_project::HistoryNode::CollapsedImport { op, .. } => {
            agent_changes.get(&op.id).cloned().unwrap_or_default()
        }
        editchain_project::HistoryNode::GitCommit { commit, .. } => git_changes
            .get(&(commit.repository, commit.oid))
            .cloned()
            .unwrap_or_default(),
        editchain_project::HistoryNode::ExecuteBundle { member_nodes, .. }
        | editchain_project::HistoryNode::PlanBundle { member_nodes, .. }
        | editchain_project::HistoryNode::WorkGroup { member_nodes, .. } => member_nodes
            .iter()
            .flat_map(|member| node_file_changes(member, agent_changes, git_changes))
            .collect(),
    }
}

fn file_change_rows(
    changes: &[FileChangeDto],
    node: &editchain_project::HistoryNode,
) -> Vec<ExpandedChildRow> {
    changes
        .iter()
        .cloned()
        .map(|change| ExpandedChildRow {
            op_id: change.op_id.clone().unwrap_or_default(),
            git_oid: change.commit_oid.clone(),
            repository: change.repository.clone(),
            summary: ContentTextDto::new(change.path.clone(), true).text,
            content: row_content_dto(editchain_project::content::DisplayContent::summary(
                change.path.clone(),
            )),
            timestamp_ms: node.timestamp_ms(),
            kind: "file".to_string(),
            author: String::new(),
            commit_id: String::new(),
            is_system: false,
            record_role: RecordRole::Artifact,
            activity_kind: ActivityKind::Change,
            visibility: Visibility::Supporting,
            outcome: Outcome::Unknown,
            chain_state: node.chain_state(),
            turn_id: node.turn_id().map(|id| id.0.to_string()),
            promoted: false,
            activity_bundle: None,
            file_change: Some(change),
        })
        .collect()
}

/// Established flat detail/member rows for one ordinary or inner bundle node.
#[must_use]
fn flat_op_child_rows(node: &editchain_project::HistoryNode) -> Vec<ExpandedChildRow> {
    let summaries = node_sub_op_summaries(node);
    let member_meta = node_sub_op_meta_index(node);
    summaries
        .into_iter()
        .enumerate()
        .map(|(index, summary)| {
            let (record_role, activity_kind) = node
                .sub_ops()
                .get(index)
                .map_or((RecordRole::Unknown, ActivityKind::Unknown), |op| {
                    node_sub_op_meta(op.as_ref(), &member_meta)
                });
            ExpandedChildRow {
                op_id: summary.op_id,
                git_oid: None,
                repository: None,
                content: child_content_dto(
                    node.sub_ops().get(index).map(AsRef::as_ref),
                    &summary.summary,
                ),
                summary: ContentTextDto::new(summary.summary, false).text,
                timestamp_ms: summary.timestamp_ms,
                kind: summary.kind,
                author: String::new(),
                commit_id: String::new(),
                is_system: true,
                record_role,
                activity_kind,
                visibility: Visibility::Supporting,
                outcome: Outcome::Unknown,
                chain_state: node.chain_state(),
                turn_id: node.turn_id().map(|id| id.0.to_string()),
                promoted: false,
                activity_bundle: None,
                file_change: None,
            }
        })
        .collect()
}

/// Build the expanded sub-op summaries for a top-level node.
///
/// Activity bundles expose their folded member rows, so each member renders
/// with its ORIGINAL row's summary/kind (faithful labels) while the member's
/// own metadata sub-ops keep the generic op-derived labels. All other nodes
/// use the generic op-derived path unchanged.
#[must_use]
fn node_sub_op_summaries(node: &editchain_project::HistoryNode) -> Vec<SubOpSummary> {
    if let editchain_project::HistoryNode::ExecuteBundle {
        member_nodes,
        members,
        ..
    }
    | editchain_project::HistoryNode::PlanBundle {
        member_nodes,
        members,
        ..
    } = node
    {
        member_sub_op_summaries(members, member_nodes)
    } else {
        sub_op_summaries(node.sub_ops())
    }
}

/// Sub-op summaries for one bundle's flattened member ops.
///
/// Each entry is the member's anchor op (labels come from the original row) or
/// one of the member's own bundled metadata ops (generic labels), preserving
/// reveal order.
#[must_use]
fn member_sub_op_summaries(
    members: &[std::sync::Arc<Op>],
    member_nodes: &[editchain_project::HistoryNode],
) -> Vec<SubOpSummary> {
    let key_to_node: HashMap<String, &editchain_project::HistoryNode> = member_nodes
        .iter()
        .map(|node| (node.node_key(), node))
        .collect();
    members
        .iter()
        .map(|op| {
            let op_key = op.id.to_string();
            if let Some(member) = key_to_node.get(&op_key) {
                SubOpSummary {
                    op_id: op_key,
                    summary: ContentTextDto::new(member.summary(), false).text,
                    kind: member.kind(),
                    timestamp_ms: op.observed_unix_ms().unwrap_or(0),
                }
            } else {
                let (summary, kind) = sub_op_label(op);
                SubOpSummary {
                    op_id: op_key,
                    summary,
                    kind,
                    timestamp_ms: op.observed_unix_ms().unwrap_or(0),
                }
            }
        })
        .collect()
}

/// Per-bundle op-key -> member metadata index for expanded sub-op rows.
///
/// Built once per bundle node; entries cover only the folded member rows (their
/// own metadata sub-ops fall through to the generic op-derived classifier).
#[must_use]
fn node_sub_op_meta_index(
    node: &editchain_project::HistoryNode,
) -> HashMap<String, (RecordRole, ActivityKind)> {
    match node {
        editchain_project::HistoryNode::ExecuteBundle { member_nodes, .. }
        | editchain_project::HistoryNode::PlanBundle { member_nodes, .. } => member_nodes
            .iter()
            .map(|member| {
                (
                    member.node_key(),
                    (member.record_role(), member.activity_kind()),
                )
            })
            .collect(),
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::CollapsedImport { .. }
        | editchain_project::HistoryNode::WorkGroup { .. }
        | editchain_project::HistoryNode::GitCommit { .. } => HashMap::new(),
    }
}

/// Semantic role/activity for one expanded sub-op row, using the bundle's
/// precomputed member metadata when the op is a folded member row.
#[must_use]
fn node_sub_op_meta(
    op: &Op,
    member_meta: &HashMap<String, (RecordRole, ActivityKind)>,
) -> (RecordRole, ActivityKind) {
    member_meta
        .get(&op.id.to_string())
        .copied()
        .unwrap_or_else(|| sub_op_meta(op))
}

/// Map the projection's provider-neutral relation kind to the protocol enum.
///
/// The projection derives kinds from exact spawn, reconnect, fork, and
/// produced-commit facts; `Unknown` remains the forward-compatible fallback.
#[must_use]
fn protocol_relation_kind(kind: editchain_project::RelationKind) -> ParentRelationKind {
    match kind {
        editchain_project::RelationKind::Subagent => ParentRelationKind::Subagent,
        editchain_project::RelationKind::Reconnect => ParentRelationKind::Reconnect,
        editchain_project::RelationKind::Fork => ParentRelationKind::Fork,
        editchain_project::RelationKind::ProducedCommit => ParentRelationKind::ProducedCommit,
    }
}

/// Derive a display label for a bundled sub-op.
///
/// Metadata sub-ops are raw Import ops — parse the JSONL record type. Tool-result
/// sub-ops are `Tool` ops with `stage: Finish` — render a content preview.
#[must_use]
pub(super) fn sub_op_label(op: &Op) -> (String, String) {
    // A tool-result sub-op (grouped under its tool call): show a content preview.
    if let OpKind::Tool(t) = &op.kind {
        if matches!(t.stage, editchain_core::op::ToolStage::Finish) {
            let preview = tool_result_preview(&payload_text(&t.content));
            return (preview, "tool_result".to_string());
        }
    }
    let raw = match &op.kind {
        OpKind::Import(i) => match &i.raw_ref {
            Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
            Payload::Empty | Payload::Blob(_) => String::new(),
        },
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::Tool(_)
        | OpKind::Command(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => String::new(),
    };
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(record_type) = value.get("type").and_then(serde_json::Value::as_str) {
            let token_kind = if record_type == "token_usage_record" {
                Some("token_usage_record")
            } else if record_type == "event_msg"
                && value
                    .get("payload")
                    .and_then(|payload| payload.get("type"))
                    .and_then(serde_json::Value::as_str)
                    == Some("token_count")
            {
                Some("token_count")
            } else {
                None
            };
            if let (Some(kind), Some(summary)) = (
                token_kind,
                editchain_project::import_token_accounting_summary(raw.as_bytes()),
            ) {
                return (summary, kind.to_string());
            }
            let label = if record_type == "assistant" {
                value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(serde_json::Value::as_array)
                    .and_then(|content| content.first())
                    .and_then(|block| {
                        let kind = block.get("type").and_then(serde_json::Value::as_str)?;
                        match kind {
                            "tool_use" => block
                                .get("name")
                                .and_then(serde_json::Value::as_str)
                                .filter(|name| !name.is_empty())
                                .map(|name| format!("tool: {name}")),
                            "text" => block
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .filter(|text| !text.trim().is_empty())
                                .map(tool_result_preview),
                            "thinking" => Some("thinking".to_string()),
                            other if !other.is_empty() => Some(other.to_string()),
                            _ => None,
                        }
                    })
                    .unwrap_or_else(|| record_type.to_string())
            } else if record_type == "event_msg" {
                value
                    .get("payload")
                    .and_then(|payload| payload.get("type"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|event_type| !event_type.is_empty())
                    .unwrap_or(record_type)
                    .to_string()
            } else {
                record_type.to_string()
            };
            let kind = if record_type == "assistant" && label.starts_with("tool:") {
                "tool".to_string()
            } else {
                label.clone()
            };
            return (label, kind);
        }
    }
    (raw, "meta".to_string())
}

/// Map a sub-op's kind tag to a coarse semantic class used to pick its icon.
///
/// The class is intentionally coarse (a handful of buckets) so the client can
/// map it to a small set of Codicons without enumerating every record type.
#[must_use]
fn subop_semantic_class(kind: &str) -> String {
    match kind {
        "tool_result" => "tool_result".to_string(),
        // File-history snapshots and file edits are "edit"-like records.
        "file-history-snapshot" | "edited_text_file" | "file" | "opened_file_in_ide" => {
            "edit".to_string()
        }
        // User-facing text records.
        "message" | "command" | "last-prompt" => "msg".to_string(),
        // Everything else is metadata (mode, permission-mode, custom-title,
        // agent-name, telemetry system subtypes, etc.).
        _ => "meta".to_string(),
    }
}

/// Intersect two sorted lane lists, returning the shared lanes in order.
///
/// Used to find which lanes pass straight through a sub-op region: a lane with a
/// vertical line leaving the parent downward AND entering the next node from
/// above spans the whole region continuously.
#[must_use]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "two-pointer intersection; indices are bounds-checked by the loop condition"
)]
fn intersect_sorted(a: &[usize], b: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Produce a pretty-printed, truncated preview of a tool result's content.
///
/// Strips leading `<digits>\t` line-number prefixes, collapses to the first
/// non-empty line, and truncates to ~1024 chars. Mirrors the projection's
/// `tool_result_summary` so sub-op previews match the main-pane summaries.
#[must_use]
fn tool_result_preview(content: &str) -> String {
    const MAX: usize = 1024;
    let stripped: String = content
        .lines()
        .map(|l| {
            let trimmed = l.trim_start();
            let after_digits = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
            after_digits.strip_prefix('\t').map_or(l, |rest| rest)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut line = stripped
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    if line.chars().count() > MAX {
        let mut cut = line.chars().take(MAX).collect::<String>();
        cut.push('…');
        line = cut;
    }
    line
}

/// Derive a short author label from an op's tags.
///
/// Prefers the actor role tags (`HUMAN` / `AGENT`); falls back to `system` for
/// anything else (imports, tools, commands, etc.).
#[must_use]
fn op_author_label(tags: Tags) -> String {
    if tags.matches_any(Tags::HUMAN) {
        "human".to_string()
    } else if tags.matches_any(Tags::AGENT) {
        "agent".to_string()
    } else {
        "system".to_string()
    }
}

/// Commit/ID display value for a history node.
///
/// Git commits show an abbreviated OID; `EditChain` ops show an abbreviated
/// op id (`node:seq`, dropping the boot counter) so both row types read as a
/// short, uniform identifier in this column.
#[must_use]
fn node_commit_id(node: &editchain_project::HistoryNode) -> String {
    match node {
        editchain_project::HistoryNode::EditOperation { op, .. }
        | editchain_project::HistoryNode::CollapsedImport { op, .. } => abbreviate_op_id(&op.id),
        editchain_project::HistoryNode::ExecuteBundle { anchor, .. }
        | editchain_project::HistoryNode::PlanBundle { anchor, .. }
        | editchain_project::HistoryNode::WorkGroup { anchor, .. } => abbreviate_op_id(&anchor.id),
        editchain_project::HistoryNode::GitCommit { commit, .. } => abbreviate_oid(&commit.oid),
    }
}

/// Abbreviate an op id (`node:boot:seq`) to a short `node:seq` form.
///
/// The boot counter is almost always 0 and adds noise; dropping it keeps the
/// column compact while preserving the distinguishing sequence number.
#[must_use]
fn abbreviate_op_id(id: &OpId) -> String {
    format!("{}:{}", id.node.0, id.seq)
}

/// Abbreviate a git OID to its first 7 hex characters.
#[must_use]
fn abbreviate_oid(oid: &GitOid) -> String {
    let hex = oid.to_hex();
    hex.chars().take(7).collect()
}
