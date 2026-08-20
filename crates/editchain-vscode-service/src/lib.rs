//! Native Rust stdio service for the `EditChain` VS Code extension.
//!
//! Reads framed requests from stdin, dispatches them against the chain reader,
//! git resolver, and unified search, and writes framed responses to stdout.

#[cfg(test)]
use tempfile as _;

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_query as _;

use std::path::PathBuf;

use editchain_codec::frame::decode_op;
use editchain_core::{
    ActorId, Clock, GitOid, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
};
use editchain_git::{discover_repositories, resolve_commit, walk_history, RepositoryHandle};
use editchain_index::LexicalIndex;
use editchain_node::segment::SegmentStore;
use editchain_project::filter::ChainFilter;
use editchain_project::HistoryProjection;
use editchain_protocol::{
    ChainFilterDto, GraphLayout as ProtocolGraphLayout, HistoryRow, HistoryWindow, LayoutEdge,
    LayoutPoint, LayoutRow, NodeDetails, RepositoryInfo, Request, RequestBody, Response,
    ResponseBody,
};

/// A loaded workspace: chain ops + git repositories.
#[derive(Debug)]
pub struct Workspace {
    /// The unified history projection.
    pub projection: HistoryProjection,
    /// Discovered git repositories.
    pub repositories: Vec<editchain_git::RepositoryDiscovery>,
    /// Cached canonical node list per `(hide_submodules, filter)` state.
    ///
    /// The topological sort over the full chain is the expensive part; it is
    /// computed once per filter state and reused for every window/layout call.
    sorted_nodes: std::collections::HashMap<
        (bool, editchain_project::filter::ChainFilterKey),
        Vec<editchain_project::HistoryNode>,
    >,
    /// Cached layout context per `(hide_submodules, filter)` state.
    ///
    /// The context bundles all O(V) derived data (keys, row map, lane map, lane
    /// assignment) so per-window edge computation is O(window). Computed once
    /// per filter state and reused across scrolls/resizes.
    contexts: std::collections::HashMap<
        (bool, editchain_project::filter::ChainFilterKey),
        editchain_project::layout::LayoutContext,
    >,
}

impl Workspace {
    /// Create a workspace from an existing projection (used in tests).
    #[must_use]
    pub fn from_projection(projection: HistoryProjection) -> Self {
        Self {
            projection,
            repositories: Vec::new(),
            sorted_nodes: std::collections::HashMap::new(),
            contexts: std::collections::HashMap::new(),
        }
    }

    /// Load a workspace from a chain directory and discover git repos.
    ///
    /// # Errors
    ///
    /// Returns an error if the chain cannot be read or repos cannot be discovered.
    pub fn open(workspace_path: &str, chain_dir: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Resolve the chain directory relative to the workspace when it is a
        // relative path (e.g. ".editchain"). The service process's CWD is not
        // necessarily the workspace root, so we must join explicitly.
        let chain_path = if PathBuf::from(chain_dir).is_absolute() {
            PathBuf::from(chain_dir)
        } else {
            PathBuf::from(workspace_path).join(chain_dir)
        };
        let ops = read_chain_ops(&chain_path)?;
        // q6 Phase-1: enable per-source-chain META bundling by default in the live
        // viewer. `ProjectionOptions` is passed explicitly so the behavior is
        // deterministic and a real cache key, never a process-global toggle.
        let options = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let mut projection = HistoryProjection::from_ops_with(ops, options);
        let repositories = discover_repositories(&PathBuf::from(workspace_path))?;
        // Walk each discovered repo's history into the projection.
        for discovery in &repositories {
            let opened = open_repository_handle(discovery);
            let Ok(handle) = opened else {
                continue;
            };
            let walked = walk_history(&handle, 0);
            if let Ok(commits) = walked {
                projection.merge_git_commits(commits);
            }
        }
        // Stitch sessions and git history into a single edit chain.
        projection.link_history();
        Ok(Self {
            projection,
            repositories,
            sorted_nodes: std::collections::HashMap::new(),
            contexts: std::collections::HashMap::new(),
        })
    }

    /// Get a window of history rows (newest-first).
    #[must_use]
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::needless_borrow,
        reason = "expanded-slot prefix sums are bounded by node count; indexing is bounds-checked by partition_point; node is a &HistoryNode reference"
    )]
    pub fn history_window(
        &mut self,
        offset: u64,
        limit: u64,
        hide_submodules: bool,
        filter: &ChainFilter,
    ) -> HistoryWindow {
        let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        // Build the node list, optionally filtering out submodules and applying
        // the chain filter.
        let filtered = self.filtered_nodes(hide_submodules, filter);

        // Build/access the cached layout context so each row can carry its graph
        // geometry (lane + active lanes + transitions) for per-row rendering.
        let key = (hide_submodules, filter.key());
        if !self.contexts.contains_key(&key) {
            let sorted = self.filtered_nodes(hide_submodules, filter);
            let ctx = self.projection.layout_context(&sorted);
            drop(self.contexts.insert(key.clone(), ctx));
        }
        let ctx = self.contexts.get(&key);

        // The service emits a FIXED fully-expanded flat list: every combined op
        // always occupies its stable 1+N absolute slots (parent + one per bundled
        // sub-op), so fetch/cache indices never move regardless of reveal state.
        // Collapse/expand is purely a client rendering decision; scroll offsets are
        // derived from how many slots are currently visible via prefix sums over
        // per-node sub-op counts.
        let sub_op_counts: Vec<usize> = filtered.iter().map(|n| n.sub_ops().len()).collect();
        // Prefix sum of expanded slot starts: slot index where each top-level node's
        // block begins (parent row). Length = filtered.len() + 1; last entry is total.
        let mut starts = Vec::with_capacity(filtered.len() + 1);
        starts.push(0usize);
        for &c in &sub_op_counts {
            starts.push(*starts.last().unwrap_or(&0) + 1 + c);
        }
        let expanded_total = *starts.last().unwrap_or(&0);

        // Find the first top-level node whose expanded block overlaps [offset, end).
        let end_usize = offset_usize.saturating_add(limit_usize);
        let first_node = starts.partition_point(|&s| s < offset_usize);
        let mut rows: Vec<HistoryRow> = Vec::new();
        for abs_idx in first_node..filtered.len() {
            if starts[abs_idx] >= end_usize {
                break;
            }
            let node = &filtered[abs_idx];
            let block_start = starts[abs_idx];
            // Per-row graph geometry from the layout context (absolute row index
            // into the full sorted list).
            let (lane, above, below, transitions) =
                ctx.map_or((0, Vec::new(), Vec::new(), Vec::new()), |c| {
                    (
                        c.lanes.get(abs_idx).map_or(0, |r| r.lane),
                        c.row_above.get(abs_idx).cloned().unwrap_or_default(),
                        c.row_below.get(abs_idx).cloned().unwrap_or_default(),
                        c.row_transitions.get(abs_idx).cloned().unwrap_or_default(),
                    )
                });
            let parent_row = block_start;
            // Emit the parent row if it falls inside the window.
            if block_start >= offset_usize && block_start < end_usize {
                rows.push(HistoryRow {
                    op_id: node.op_id().map(|id| id.to_string()),
                    git_oid: node.git_oid(),
                    repository: node.repository(),
                    summary: node.summary(),
                    timestamp_ms: node.timestamp_ms(),
                    group: node.group(),
                    node_key: node.node_key(),
                    parents: self.projection.lifted_parent_keys(&node),
                    is_submodule: node
                        .repository()
                        .is_some_and(|rid| self.repo_is_submodule(rid)),
                    is_system: node_is_system(&node),
                    author: node_author(&node),
                    commit_id: node_commit_id(&node),
                    kind: node.kind(),
                    lane,
                    above,
                    below,
                    transitions,
                    sub_ops: sub_op_summaries(node.sub_ops()),
                    is_subop: false,
                    parent_row: None,
                    subop_kind: None,
                });
            }
            // Emit each bundled sub-op as its own row immediately after its parent.
            let summaries = sub_op_summaries(node.sub_ops());
            // Lanes passing straight through this sub-op region (between this
            // parent and the next top-level node): any lane with a vertical line
            // leaving this parent downward AND entering the next node from above
            // spans the whole region continuously. Sub-op rows draw these as
            // full-height straight lines with no dot.
            let region_lanes = ctx.map_or(Vec::new(), |c| {
                let below_parent = c.row_below.get(abs_idx).map_or(&[][..], Vec::as_slice);
                let above_next = c.row_above.get(abs_idx + 1).map_or(&[][..], Vec::as_slice);
                intersect_sorted(below_parent, above_next)
            });
            for (i, sub) in summaries.iter().enumerate() {
                let slot = block_start + 1 + i;
                if slot < offset_usize || slot >= end_usize {
                    continue;
                }
                rows.push(HistoryRow {
                    op_id: Some(sub.op_id.clone()),
                    git_oid: None,
                    repository: None,
                    summary: sub.summary.clone(),
                    timestamp_ms: sub.timestamp_ms,
                    group: node.group(),
                    // Sub-op rows are not graph nodes; key them under their parent so
                    // group-start detection and click routing stay unambiguous.
                    node_key: format!("{}::sub:{i}", node.node_key()),
                    parents: Vec::new(),
                    is_submodule: false,
                    is_system: true,
                    author: String::new(),
                    commit_id: String::new(),
                    kind: sub.kind.clone(),
                    // No dot of its own — draw every pass-through lane as a
                    // full-height straight line (both halves meet at midY).
                    lane,
                    above: region_lanes.clone(),
                    below: region_lanes.clone(),
                    transitions: Vec::new(),
                    sub_ops: Vec::new(),
                    is_subop: true,
                    parent_row: Some(parent_row),
                    subop_kind: Some(subop_semantic_class(&sub.kind)),
                });
            }
        }
        let max_lane = ctx.map_or(0, |c| c.lanes.iter().map(|r| r.lane).max().unwrap_or(0));
        HistoryWindow {
            rows,
            total: u64::try_from(expanded_total).unwrap_or(u64::MAX),
            chain_generation: 0,
            max_lane,
            // Ship the global per-node sub-op counts with EVERY window so the
            // client can build prefix sums for visible/absolute index mapping
            // regardless of where it jumps (a deep jump must not depend on the
            // offset==0 window having been fetched first).
            sub_op_counts: Some(sub_op_counts.clone()),
        }
    }

    /// Returns the canonical node list, optionally filtering out submodules and
    /// applying a chain filter.
    ///
    /// This is the single source of truth for both [`Self::history_window`] and
    /// [`Self::graph_layout`], so their row indices stay in lockstep. The result
    /// is cached per `(hide_submodules, filter)` so the expensive topological
    /// sort runs only once per filter state.
    #[must_use]
    fn filtered_nodes(
        &mut self,
        hide_submodules: bool,
        filter: &ChainFilter,
    ) -> Vec<editchain_project::HistoryNode> {
        let key = (hide_submodules, filter.key());
        if let Some(nodes) = self.sorted_nodes.get(&key) {
            return nodes.clone();
        }
        let all_nodes = self.projection.filtered_nodes(filter);
        let filtered: Vec<_> = if hide_submodules {
            all_nodes
                .into_iter()
                .filter(|n| {
                    !n.repository()
                        .is_some_and(|rid| self.repo_is_submodule(rid))
                })
                .collect()
        } else {
            all_nodes
        };
        drop(self.sorted_nodes.insert(key, filtered.clone()));
        filtered
    }

    /// Compute the graph layout for a bounded window of rows.
    ///
    /// The layout context (all O(V) derived data) is cached per `hide_submodules`
    /// and computed once; only edges whose child falls inside `[offset,
    /// offset+limit)` are emitted. This keeps per-scroll cost proportional to the
    /// visible slice rather than the whole graph.
    #[must_use]
    #[expect(
        clippy::print_stderr,
        reason = "Diagnostic logging to the VS Code output pane via service stderr"
    )]
    pub fn graph_layout(
        &mut self,
        hide_submodules: bool,
        offset: u64,
        limit: u64,
        filter: &ChainFilter,
    ) -> ProtocolGraphLayout {
        // Ensure the context is built (once per filter state), then borrow it.
        let key = (hide_submodules, filter.key());
        if !self.contexts.contains_key(&key) {
            let sorted = self.filtered_nodes(hide_submodules, filter);
            let ctx = self.projection.layout_context(&sorted);
            drop(self.contexts.insert(key.clone(), ctx));
        }
        let Some(ctx) = self.contexts.get(&key) else {
            return ProtocolGraphLayout {
                rows: Vec::new(),
                edges: Vec::new(),
                max_lane: 0,
            };
        };
        let offset_usize = usize::try_from(offset).unwrap_or(0);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        let edges = ctx.edges_for_window(offset_usize, limit_usize);
        eprintln!(
            "[layout] GetLayout offset={} limit={} rows={} edges={} max_lane={}",
            offset_usize,
            limit_usize,
            ctx.lanes.len(),
            edges.len(),
            ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0)
        );

        // Only send the window slice of rows (the webview needs lanes only for
        // visible rows). Sending all V rows would serialize the whole graph on
        // every scroll.
        let row_end = offset_usize
            .saturating_add(limit_usize)
            .min(ctx.lanes.len());
        let rows = ctx
            .lanes
            .get(offset_usize..row_end)
            .unwrap_or(&[])
            .iter()
            .map(|r| LayoutRow {
                node: r.node.clone(),
                lane: r.lane,
            })
            .collect();

        // Global max lane across ALL rows (not just this window), so the client
        // can size the graph column stably regardless of which window is loaded.
        let max_lane = ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0);

        ProtocolGraphLayout {
            rows,
            edges: edges
                .into_iter()
                .map(|e| LayoutEdge {
                    child: e.child,
                    parent: e.parent,
                    points: e
                        .points
                        .into_iter()
                        .map(|p| LayoutPoint {
                            row: p.row,
                            lane: p.lane,
                        })
                        .collect(),
                })
                .collect(),
            max_lane,
        }
    }

    /// Get details for a specific node by operation ID or git OID.
    #[must_use]
    pub fn node_details(
        &self,
        op_id: Option<String>,
        git_oid: Option<GitOid>,
    ) -> Option<NodeDetails> {
        if let Some(op_id_str) = op_id {
            let op_id = OpId::from_display_str(&op_id_str)?;
            let op = self.projection.ops.iter().find(|op| op.id == op_id)?;
            return Some(node_details_from_op(op));
        }
        if let Some(oid) = git_oid {
            let commit = self
                .projection
                .git
                .commits
                .values()
                .find(|c| c.oid == oid)?;
            return Some(node_details_from_commit(commit));
        }
        None
    }

    /// List discovered repositories.
    #[must_use]
    pub fn repositories_info(&self) -> Vec<RepositoryInfo> {
        self.repositories
            .iter()
            .map(|d| RepositoryInfo {
                id: d.id,
                path: d.path.to_string_lossy().to_string(),
                is_worktree: d.is_worktree,
                is_submodule: self.is_submodule(d),
            })
            .collect()
    }

    /// Returns true if a repository is nested inside another discovered repo
    /// (i.e. a submodule or vendored nested repo, not the workspace root).
    #[must_use]
    fn is_submodule(&self, discovery: &editchain_git::RepositoryDiscovery) -> bool {
        // Discovery paths point at `.git`; the repo root is the parent dir.
        let root = discovery.path.parent().unwrap_or(&discovery.path);
        let root_str = root.to_string_lossy();
        self.repositories.iter().any(|other| {
            if other.id == discovery.id || other.path == discovery.path {
                return false;
            }
            let other_root = other.path.parent().unwrap_or(&other.path);
            let other_str = other_root.to_string_lossy();
            // This repo's root is strictly inside another repo's root.
            other_str.len() < root_str.len() && root_str.starts_with(&*other_str)
        })
    }

    /// Returns true if the repository with the given id is a submodule.
    #[must_use]
    fn repo_is_submodule(&self, repository_id: editchain_core::RepositoryId) -> bool {
        self.repositories
            .iter()
            .any(|d| d.id == repository_id && self.is_submodule(d))
    }
}

/// Convert an optional protocol filter DTO into a [`ChainFilter`].
///
/// A `None` DTO yields the default filter (hide undated, splice on), matching
/// the webview's default behavior. An empty DTO yields an empty filter that
/// hides nothing.
#[must_use]
fn chain_filter_from_dto(dto: Option<&ChainFilterDto>) -> ChainFilter {
    match dto {
        Some(d) => ChainFilter::new(
            d.summary_pattern.clone(),
            d.kind_pattern.clone(),
            d.hide_undated,
            d.splice,
        ),
        None => ChainFilter::default(),
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
            kind == "tool" || kind == "import"
        }
        editchain_project::HistoryNode::GitCommit(_) => false,
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
        editchain_project::HistoryNode::CollapsedImport { author, .. } => author.clone(),
        editchain_project::HistoryNode::GitCommit(commit) => payload_text(&commit.author.name),
    }
}

/// Build the bundled sub-op summaries for a row from its attached metadata ops.
///
/// Each bundled sub-op is a raw `Import` op tagged `META`. Its summary is the
/// record type (derived from the raw JSONL's `type` field when parseable, else
/// the raw reference text), so the viewer can label each revealed sub-row.
#[must_use]
fn sub_op_summaries(sub_ops: &[Op]) -> Vec<editchain_protocol::SubOpSummary> {
    sub_ops
        .iter()
        .map(|op| {
            let (summary, kind) = sub_op_label(op);
            editchain_protocol::SubOpSummary {
                op_id: op.id.to_string(),
                summary,
                kind,
                timestamp_ms: op.clock.as_u64(),
            }
        })
        .collect()
}

/// Derive a display label for a bundled sub-op.
///
/// Metadata sub-ops are raw Import ops — parse the JSONL record type. Tool-result
/// sub-ops are `Tool` ops with `stage: Finish` — render a content preview.
#[must_use]
fn sub_op_label(op: &Op) -> (String, String) {
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
            return (record_type.to_string(), record_type.to_string());
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
        editchain_project::HistoryNode::GitCommit(commit) => abbreviate_oid(&commit.oid),
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

/// Build node details from an `EditChain` operation.
#[must_use]
fn node_details_from_op(op: &Op) -> NodeDetails {
    NodeDetails {
        op_id: Some(op.id.to_string()),
        git_oid: None,
        repository: None,
        summary: op_summary(op),
        body: op_body(op),
        parents: op.parents.iter().copied().collect(),
        git_parents: Vec::new(),
        refs: Vec::new(),
        changed_paths: Vec::new(),
    }
}

/// Build node details from a git commit entity.
#[must_use]
fn node_details_from_commit(commit: &editchain_core::GitCommitEntity) -> NodeDetails {
    let body = match &commit.message {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    };
    let refs = commit
        .live_refs
        .iter()
        .chain(commit.imported_refs.iter())
        .filter_map(|r| match r {
            Payload::Inline(b) => Some(String::from_utf8_lossy(b).to_string()),
            Payload::Empty | Payload::Blob(_) => None,
        })
        .collect();
    let changed_paths = commit
        .changed_paths
        .iter()
        .map(|p| p.0.to_string())
        .collect();
    NodeDetails {
        op_id: commit.imported_record.map(|id| id.to_string()),
        git_oid: Some(commit.oid),
        repository: Some(commit.repository),
        summary: body.clone(),
        body,
        parents: Vec::new(),
        git_parents: commit.parents.clone(),
        refs,
        changed_paths,
    }
}

/// Read all decoded operations from a chain directory.
///
/// # Errors
///
/// Returns an error if the chain directory cannot be read.
fn read_chain_ops(chain_dir: &PathBuf) -> Result<Vec<Op>, Box<dyn std::error::Error>> {
    if chain_dir.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    let store = SegmentStore::open(chain_dir)?;
    let pages = store.read_all()?;
    let mut ops = Vec::new();
    for page in &pages {
        for record in &page.records {
            if let Ok(op) = decode_op(&record.data) {
                ops.push(op);
            }
        }
    }
    Ok(ops)
}

/// Build a lexical index over all chain ops and git commits.
///
/// # Errors
///
/// Returns an error if the index cannot be created or populated.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Generation counter increments are bounded by the number of indexed ops"
)]
fn build_lexical_index(workspace: &Workspace) -> Result<LexicalIndex, Box<dyn std::error::Error>> {
    let mut index = LexicalIndex::new()?;
    let mut generation = 0u64;
    for op in &workspace.projection.ops {
        drop(index.index_op(op, generation)?);
        generation += 1;
    }
    // Index git commits as synthetic ops.
    for commit in workspace.projection.git.commits.values() {
        let op = Op {
            id: OpId::new(NodeId(0), 0, generation),
            parents: ParentSet::None,
            actor: ActorId(0),
            clock: Clock::UnixMs(u64::try_from(commit.committed_at).unwrap_or(0)),
            scope: ScopeRef::None,
            tags: Tags::IMPORT,
            kind: OpKind::GitCommit(Box::new(commit.clone())),
        };
        drop(index.index_op(&op, generation)?);
        generation += 1;
    }
    index.commit()?;
    Ok(index)
}

/// Resolve a git commit by OID in a discovered repository.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or the object cannot
/// be resolved.
pub fn resolve_git_commit(
    workspace: &Workspace,
    repository_id: editchain_core::RepositoryId,
    oid: &GitOid,
) -> Result<Option<editchain_core::GitCommitEntity>, Box<dyn std::error::Error>> {
    let Some(discovery) = workspace
        .repositories
        .iter()
        .find(|d| d.id == repository_id)
    else {
        return Ok(None);
    };
    let Ok(handle) = open_repository_handle(discovery) else {
        return Ok(None);
    };
    match resolve_commit(&handle, oid) {
        Ok(res) => Ok(Some(res.commit)),
        Err(_) => Ok(None),
    }
}

/// Open a discovered repository as a `RepositoryHandle`.
fn open_repository_handle(
    discovery: &editchain_git::RepositoryDiscovery,
) -> Result<RepositoryHandle, Box<dyn std::error::Error>> {
    let open_path = if discovery.is_worktree {
        discovery
            .path
            .parent()
            .unwrap_or(&discovery.path)
            .to_path_buf()
    } else {
        discovery.path.clone()
    };
    let repo = gix::open(&open_path)?;
    Ok(RepositoryHandle {
        repo,
        discovery: discovery.clone(),
    })
}

/// A stateful server that owns a loaded workspace across requests.
#[derive(Debug)]
pub struct Server {
    /// The currently loaded workspace (None until `Open`).
    pub workspace: Option<Workspace>,
    /// The lexical search index (built on `Open`).
    pub lexical: Option<LexicalIndex>,
}

impl Server {
    /// Create a new empty server.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workspace: None,
            lexical: None,
        }
    }

    /// Handle a single request against the current state.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be handled.
    pub fn handle(&mut self, request: &Request) -> Result<Response, Box<dyn std::error::Error>> {
        let id = request.id;
        let body = match &request.body {
            RequestBody::Open(req) => {
                let workspace = Workspace::open(&req.workspace_path, &req.chain_dir)?;
                // The lexical index is built lazily on first Search (it is
                // expensive for large chains and unnecessary for the graph view).
                self.workspace = Some(workspace);
                self.lexical = None;
                ResponseBody::Ok(serde_json::json!({
                    "workspace": req.workspace_path,
                    "chain": req.chain_dir,
                    "repos": self.workspace.as_ref().map_or(0, |w| w.repositories.len()),
                    "nodes": self.workspace.as_ref().map_or(0, |w| w.projection.len()),
                }))
            }
            RequestBody::GetWindow(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let window = ws.history_window(req.offset, req.limit, req.hide_submodules, &filter);
                ResponseBody::Ok(serde_json::to_value(window)?)
            }
            RequestBody::GetLayout(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let layout = ws.graph_layout(req.hide_submodules, req.offset, req.limit, &filter);
                ResponseBody::Ok(serde_json::to_value(layout)?)
            }
            RequestBody::GetNodeDetails(req) => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                match ws.node_details(Some(req.op_id.clone()), None) {
                    Some(details) => ResponseBody::Ok(serde_json::to_value(details)?),
                    None => ResponseBody::Error("node not found".to_string()),
                }
            }
            RequestBody::GetRepositories => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                ResponseBody::Ok(serde_json::to_value(ws.repositories_info())?)
            }
            RequestBody::ResolveObject(req) => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                match resolve_git_commit(ws, req.repository, &req.oid)? {
                    Some(commit) => ResponseBody::Ok(serde_json::to_value(&commit)?),
                    None => ResponseBody::Error("object not found".to_string()),
                }
            }
            RequestBody::SetFilters(_) => ResponseBody::Error("filters not yet wired".to_string()),
            RequestBody::Search(req) => {
                // Build the lexical index lazily on first search.
                if self.lexical.is_none() {
                    let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                    self.lexical = Some(build_lexical_index(ws)?);
                }
                let lexical = self.lexical.as_ref().ok_or("no index built")?;
                let results = lexical.search_internal(&req.query, &req.filters, req.top_k)?;
                ResponseBody::Ok(serde_json::to_value(results)?)
            }
        };
        Ok(Response { id, body })
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

/// Produce a short summary for an `EditChain` operation.
#[must_use]
fn op_summary(op: &Op) -> String {
    match &op.kind {
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.tool_name),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::File(f) => format!("file:{}", f.path.0),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(cs) => String::from_utf8_lossy(&cs.name).to_string(),
        OpKind::Actor(a) => payload_text(&a.label),
        OpKind::Import(i) => payload_text(&i.raw_ref),
        OpKind::GitCommit(c) => payload_text(&c.message),
        OpKind::GitLink(l) => format!("git:{}", l.target_oid),
        OpKind::Unknown(u) => format!("unknown kind={}", u.kind_discriminant),
    }
}

/// Produce the full body text for an `EditChain` operation.
#[must_use]
fn op_body(op: &Op) -> String {
    match &op.kind {
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.content),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::File(_)
        | OpKind::Import(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => String::new(),
    }
}

/// Extract text from a payload, or empty string.
#[must_use]
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "Tests index into vectors whose length is asserted immediately before"
)]
mod tests {
    use super::*;
    use editchain_core::{ImportOp, MessageOp, SessionId};

    /// Build a raw import op.
    fn import_op(node: u64, seq: u64, meta: bool) -> Op {
        let mut tags = Tags::IMPORT;
        if meta {
            tags |= Tags::META;
        }
        Op {
            id: OpId::new(NodeId(node), 0, seq),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(10)),
            tags,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(
                    format!(r#"{{"type":"last-prompt","seq":{seq}}}"#).into_bytes(),
                ),
                raw_hash: None,
            }),
        }
    }

    /// Build a normalized message op whose parent is `parent`.
    fn message_op(node: u64, seq: u64, parent: OpId) -> Op {
        Op {
            id: OpId::new(NodeId(node), 0, seq),
            parents: ParentSet::One(parent),
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(10)),
            tags: Tags::HUMAN | Tags::MESSAGE,
            kind: OpKind::Message(MessageOp {
                content: Payload::Inline(b"hello world".to_vec()),
                content_type: Payload::Empty,
            }),
        }
    }

    #[test]
    fn history_window_bundles_meta_subops() {
        // A real turn (import + message), then a META import. The META import
        // bundles into the turn's row as a sub-op; the service emits it as its
        // own expanded row immediately after the parent.
        let turn = import_op(1, 1, false);
        let msg = message_op(1, 2, turn.id);
        let meta = import_op(1, 3, true);

        // q6 Phase-1: bundling is driven by explicit ProjectionOptions, not a global
        // toggle — no mutex needed; each projection is independently configured.
        let opts = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection =
            HistoryProjection::from_ops_with(vec![turn.clone(), msg, meta.clone()], opts);
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::default();
        let window = ws.history_window(0, 100, false, &filter);

        // Parent row + one expanded sub-op row.
        assert_eq!(window.rows.len(), 2);
        assert_eq!(window.total, 2);
        // Parent carries the bundled sub-op summary (for the collapsed chevron).
        assert_eq!(window.rows[0].sub_ops.len(), 1);
        assert_eq!(window.rows[0].sub_ops[0].op_id, meta.id.to_string());
        assert_eq!(window.rows[0].sub_ops[0].kind, "last-prompt");
        assert!(!window.rows[0].is_subop);
        assert_eq!(window.rows[0].parent_row, None);
        // The expanded sub-op row follows its parent and inherits its lane.
        assert!(window.rows[1].is_subop);
        assert_eq!(window.rows[1].parent_row, Some(0));
        assert_eq!(
            window.rows[1].op_id.as_deref(),
            Some(meta.id.to_string().as_str())
        );
        assert_eq!(window.rows[1].subop_kind.as_deref(), Some("msg"));
    }

    #[test]
    fn meta_bundle_default_standalone_opt_in_bundles() {
        // META imports render standalone by default (no cross-session grouping).
        // Only when META bundling is re-enabled do they collapse into the nearest
        // preceding real node as an expanded sub-op row.
        let turn = import_op(1, 1, false);
        let msg = message_op(1, 2, turn.id);
        let meta = import_op(1, 3, true);

        // Default (bundling off): META is a standalone top-level row.
        let projection_off =
            HistoryProjection::from_ops(vec![turn.clone(), msg.clone(), meta.clone()]);
        let mut ws_off = Workspace::from_projection(projection_off);
        let filter = ChainFilter::default();
        let window_off = ws_off.history_window(0, 100, false, &filter);
        let meta_default = window_off
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(meta.id.to_string().as_str()));
        assert!(
            meta_default.is_some(),
            "META import must appear as a row by default"
        );
        assert!(
            !meta_default.unwrap().is_subop,
            "META import must be a standalone top-level row by default (not a sub-op)"
        );

        // Opt-in (bundling on, fresh projection so the per-filter node cache is
        // not reused): the same META op renders as an expanded sub-op row.
        let opts_on = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection_on =
            HistoryProjection::from_ops_with(vec![turn.clone(), msg, meta.clone()], opts_on);
        let mut ws_on = Workspace::from_projection(projection_on);
        let window_on = ws_on.history_window(0, 100, false, &filter);
        let meta_on = window_on
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(meta.id.to_string().as_str()));
        assert!(
            meta_on.is_some_and(|r| r.is_subop),
            "META import must be an expanded sub-op row when bundling is enabled"
        );
    }

    #[test]
    fn sub_op_rows_draw_pass_through_lanes() {
        // A linear chain where a middle turn carries a bundled META sub-op and
        // has both a child above and a parent below on its own lane. The sub-op
        // row must carry that lane as pass-through (above == below), so the
        // client draws it as a full-height straight line with no dot.
        //
        // Build newest-first by clock:
        //   child (seq high) -> turn+meta (middle) -> parent (low).
        let parent = message_op(5, 4, OpId::new(NodeId(5), 0, 3));
        let turn = import_op(5, 5, false);
        let turn_with_parent = Op {
            parents: ParentSet::One(parent.id),
            ..turn.clone()
        };
        let msg = message_op(5, 6, turn.id); // child of turn
        let meta = import_op(5, 7, true); // bundled under turn

        let opts = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection = HistoryProjection::from_ops_with(
            vec![
                msg.clone(),
                turn_with_parent.clone(),
                meta.clone(),
                parent.clone(),
            ],
            opts,
        );
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::default();
        let window = ws.history_window(0, 100, false, &filter);

        // Find the sub-op row (is_subop).
        let sub = window
            .rows
            .iter()
            .find(|r| r.is_subop)
            .expect("sub-op row present");
        // The sub-op row must have at least one pass-through lane (its own
        // parent's lane), and above == below so the client draws a full line.
        assert!(!sub.above.is_empty());
        assert_eq!(sub.above, sub.below);
    }

    #[test]
    fn sub_op_label_parses_record_type() {
        let op = import_op(1, 1, true);
        let (summary, kind) = sub_op_label(&op);
        assert_eq!(summary, "last-prompt");
        assert_eq!(kind, "last-prompt");
    }

    #[test]
    fn sub_op_label_renders_tool_result_preview() {
        // A tool-result sub-op (Tool, Finish) should render a content preview.
        let op = Op {
            id: OpId::new(NodeId(1), 0, 1),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(1),
            scope: ScopeRef::Session(SessionId(10)),
            tags: Tags::TOOL,
            kind: OpKind::Tool(editchain_core::op::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Empty,
                stage: editchain_core::op::ToolStage::Finish,
                content: Payload::Inline(b"1\tline one\n2\tline two".to_vec()),
            }),
        };
        let (summary, kind) = sub_op_label(&op);
        assert_eq!(summary, "line one");
        assert_eq!(kind, "tool_result");
    }

    /// Diagnostic (not run in CI): load a real chain and report how many
    /// independent chains the projection produces, comparing raw source chains
    /// (`(OpId.node, OpId.boot)` streams) vs the bundled projection's own
    /// connected-root count against `metadata` on/off.
    #[test]
    #[ignore = "manual diagnostics against a real chain"]
    #[expect(
        clippy::print_stderr,
        reason = "manual diagnostics deliberately print raw chain-level counts to stderr"
    )]
    fn diag_chain_counts() {
        let ops = read_chain_ops(&PathBuf::from(
            "/mnt/hot/ambientlight/repos/editchain/.editchain",
        ))
        .unwrap();
        eprintln!("\n=== chain diag: {} ops ===", ops.len());

        // Raw source chains = distinct (node, boot) streams among Import ops.
        let meta_imports = ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Import(_)))
            .filter(|o| o.tags.matches_any(Tags::META))
            .count();
        eprintln!("meta-tagged raw imports: {meta_imports}");
        // Show the import tag bitmask distribution so we can see what the old
        // importer actually stamped (META may be encoded differently or absent).
        let mut tag_hist: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
        for o in ops.iter().filter(|o| matches!(o.kind, OpKind::Import(_))) {
            *tag_hist.entry(o.tags.0).or_default() += 1;
        }
        let mut sorted_tags: Vec<(u64, usize)> = tag_hist.into_iter().collect();
        sorted_tags.sort_by_key(|(t, _)| *t);
        for (t, c) in sorted_tags.iter().take(12) {
            eprintln!("  import tags {t:#016b} x{c}");
        }

        let sources: std::collections::HashSet<(u64, u32)> = ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Import(_)))
            .map(|o| (o.id.node.0, o.id.boot))
            .collect();
        eprintln!(
            "raw source streams (node,boot) among import ops: {}",
            sources.len()
        );

        // Distinct raw import roots (SnapshotTopology: count nodes with no parent
        // present among import ops) — how many chains the importer actually made.
        let import_ids: std::collections::HashSet<OpId> = ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Import(_)))
            .map(|o| o.id)
            .collect();
        let import_roots = ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Import(_)))
            .filter(|o| {
                o.parents.iter().all(|p| !import_ids.contains(p)) // no import parent present => root
            })
            .count();
        eprintln!("raw import roots (no import parent): {import_roots}");

        // Projection counts.
        for (label, bundle) in [("meta OFF", false), ("meta ON", true)] {
            let proj = HistoryProjection::from_ops_with(
                ops.clone(),
                editchain_project::ProjectionOptions {
                    bundle_metadata: bundle,
                },
            );
            let rows = proj.nodes().len();
            let chains = proj.independent_chains();
            eprintln!("{label}: top_rows={rows} chains={chains}");
            if bundle {
                // Client-view root count over one shared meta-ON projection: how
                // many top-level rows have NO parent that resolves to a present
                // row, using the SAME lifted `parents` the service emits in
                // HistoryRow. This is what actually renders as a distinct chain.
                let nodes = proj.nodes();
                let present: std::collections::HashSet<String> = nodes
                    .iter()
                    .map(editchain_project::HistoryNode::node_key)
                    .collect();
                let mut client_roots = 0usize;
                for node in &nodes {
                    let lifted = proj.lifted_parent_keys(node);
                    if lifted.iter().all(|p| !present.contains(p)) {
                        client_roots += 1;
                    }
                }
                eprintln!("meta ON client-view roots (lifted parents): {client_roots}");
            }
        }
    }
}
