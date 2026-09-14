//! Deterministic import, metadata, and tool-result folding over source operations.

use crate::ancestry::{
    canonical_op_id, canonical_present_op, canonicalize_parents, has_exact_provider_parent,
    is_hidden_relation_fact, is_visible_edge_relationship, resolve_relationship_note_targets,
    row_node_keys,
};
use crate::labels::{collapsed_import_author, collapsed_import_kind};
use crate::node::{source_time_of, HistoryNode};
use crate::taxonomy::{ChainState, Outcome};
use crate::{content, meta, CollapsedProjection, HistoryProjection};
use editchain_core::op::NoteRelationship;
use editchain_core::{Op, OpId, Payload};
use std::collections::HashMap;
use std::sync::Arc;

impl HistoryProjection {
    /// Collapse raw import ops with their normalized children into single nodes.
    ///
    /// Each raw `Import` op is the linear backbone of a session; its normalized
    /// children (`Message`, `Tool`, `Command`, `File`) branch off it. This folds
    /// each raw op + its children into one [`HistoryNode::CollapsedImport`] whose
    /// summary is derived from the children's content, so the graph shows one
    /// meaningful node per source line instead of a dense star. Non-import ops
    /// (e.g. `ChainStart`, git-link records) are kept as-is.
    ///
    /// Metadata-only raw imports (tagged `META`) bundle only when their unique
    /// graph parent resolves to another collapsed import row. Provider
    /// occurrences use their explicit
    /// provider relationship; records without provider identity use their stored
    /// source parent. Metadata chains are followed transitively. Missing,
    /// ambiguous, cyclic, and non-import parents leave the metadata standalone.
    /// Bundling never consults timestamps, input proximity, or a per-source
    /// "last row" cursor, and never rewrites stored `Op.parents` or clocks.
    #[must_use]
    pub(super) fn collapsed_ops(
        &self,
        incomplete: &std::collections::HashSet<OpId>,
    ) -> CollapsedProjection {
        // One-to-one cross-record duplicate-pair state for the response_item /
        // event_msg echo family, computed once in a single O(n) pass over the
        // ops. Response_item rows consume one pair slot per matching event_msg
        // row as the main loop reaches them in input order.
        let mut echo_pairs = meta::EchoPairState::from_ops(&self.ops);
        // Set of raw import op ids (the linear backbone).
        let import_ids: std::collections::HashSet<OpId> = self
            .ops
            .iter()
            .filter(|op| matches!(op.kind, editchain_core::OpKind::Import(_)))
            .map(|op| op.id)
            .collect();
        // Index provider entities before any display folding. `OccurrenceOf`
        // and `Contains` are exact identity facts, but identity alone does not
        // prove that two payload occurrences are interchangeable: providers can
        // reuse an event UUID while incrementally extending its content. Current
        // occurrence facts carry an importer-owned canonical payload
        // fingerprint; legacy facts conservatively fall back to the raw hash.
        let mut entity_occurrences: HashMap<OpId, Vec<OpId>> = HashMap::new();
        let mut event_occurrences: HashMap<OpId, Vec<OpId>> = HashMap::new();
        let mut occurrence_payload_fingerprints: HashMap<OpId, String> = HashMap::new();
        let mut conflicting_payload_fingerprints: std::collections::HashSet<OpId> =
            std::collections::HashSet::new();
        for op in &self.ops {
            let editchain_core::OpKind::Note(note) = &op.kind else {
                continue;
            };
            if !matches!(
                note.relationship,
                NoteRelationship::OccurrenceOf | NoteRelationship::Contains
            ) {
                continue;
            }
            let Some(anchor) = op.parents.iter().next().copied() else {
                continue;
            };
            for target in &note.target_ids {
                entity_occurrences.entry(*target).or_default().push(anchor);
                if note.relationship == NoteRelationship::OccurrenceOf {
                    event_occurrences.entry(*target).or_default().push(anchor);
                    if let Some(fingerprint) = exact_occurrence_payload_fingerprint(note) {
                        if occurrence_payload_fingerprints
                            .get(&anchor)
                            .is_some_and(|known| known != &fingerprint)
                        {
                            drop(occurrence_payload_fingerprints.remove(&anchor));
                            let _: bool = conflicting_payload_fingerprints.insert(anchor);
                        } else if !conflicting_payload_fingerprints.contains(&anchor) {
                            let _: &mut String = occurrence_payload_fingerprints
                                .entry(anchor)
                                .or_insert(fingerprint);
                        }
                    }
                }
            }
        }
        let mut representative = self.materialization.representatives.clone();
        representative.extend(&self.materialization.message_echoes);
        let mut duplicate_event_occurrences: std::collections::HashSet<OpId> = self
            .materialization
            .message_echoes
            .keys()
            .copied()
            .collect();
        let imports_by_id: HashMap<OpId, &editchain_core::op::ImportOp> = self
            .ops
            .iter()
            .filter_map(|op| match &op.kind {
                editchain_core::OpKind::Import(import) => Some((op.id, import)),
                editchain_core::OpKind::ChainStart(_)
                | editchain_core::OpKind::Actor(_)
                | editchain_core::OpKind::Message(_)
                | editchain_core::OpKind::Tool(_)
                | editchain_core::OpKind::Command(_)
                | editchain_core::OpKind::File(_)
                | editchain_core::OpKind::Reflection(_)
                | editchain_core::OpKind::Note(_)
                | editchain_core::OpKind::Error(_)
                | editchain_core::OpKind::GitCommit(_)
                | editchain_core::OpKind::GitLink(_)
                | editchain_core::OpKind::Unknown(_) => None,
            })
            .collect();
        for occurrences in event_occurrences.values() {
            // A shared provider payload fingerprint proves that copied session
            // envelopes carry the same event content even when session/topology
            // fields differ. Legacy occurrences retain the stricter raw-hash
            // behavior. Hash-less records, conflicting evidence, and distinct
            // revisions remain separate rows. The minimum ID merely names one
            // member of an exact-equivalence class; it supplies no ancestry.
            let mut by_payload: std::collections::BTreeMap<OccurrencePayloadKey, Vec<OpId>> =
                std::collections::BTreeMap::new();
            for occurrence in occurrences {
                let payload_key = occurrence_payload_fingerprints
                    .get(occurrence)
                    .cloned()
                    .map(OccurrencePayloadKey::ProviderFingerprint)
                    .or_else(|| {
                        imports_by_id
                            .get(occurrence)
                            .and_then(|import| import.raw_hash)
                            .map(OccurrencePayloadKey::RawHash)
                    });
                if let Some(payload_key) = payload_key {
                    by_payload.entry(payload_key).or_default().push(*occurrence);
                }
            }
            for equivalent in by_payload.values() {
                let Some(canonical) = equivalent.iter().copied().min() else {
                    continue;
                };
                for occurrence in equivalent {
                    if *occurrence != canonical && import_ids.contains(occurrence) {
                        let _: bool = duplicate_event_occurrences.insert(*occurrence);
                        let _: Option<OpId> = representative.insert(*occurrence, canonical);
                    }
                }
            }
        }
        // Resolve entity handles in cloned projection facts using source
        // identity. A same-source unique occurrence wins; otherwise all exact
        // duplicate classes must converge to one occurrence. Ambiguity remains
        // unresolved rather than selecting a global minimum occurrence.
        let resolved_relationship_notes = resolve_relationship_note_targets(
            &self.relationship_notes,
            &entity_occurrences,
            &representative,
        );
        // Map raw import op id -> its normalized children (in input order).
        let mut children_of: HashMap<OpId, Vec<&Op>> = HashMap::new();
        // Map folded child op id -> the raw import op id it folds into. Every
        // folded child must resolve to that import's visible row through the
        // canonical representative map (the semantic-collapse invariant).
        let mut parent_import_of: HashMap<OpId, OpId> = HashMap::new();
        // Track which non-import ops are folded into an import parent (so they
        // are dropped), versus standalone ops that must be kept.
        let mut folded: std::collections::HashSet<OpId> = std::collections::HashSet::new();
        for op in &self.ops {
            if matches!(op.kind, editchain_core::OpKind::Import(_))
                || is_hidden_relation_fact(op)
                || self.materialization.hidden.contains(&op.id)
            {
                continue;
            }
            for &parent in &op.parents {
                if import_ids.contains(&parent) {
                    let _: bool = folded.insert(op.id);
                    children_of.entry(parent).or_default().push(op);
                    let _: &mut OpId = parent_import_of.entry(op.id).or_insert(parent);
                }
            }
        }

        for children in children_of.values_mut() {
            if !children
                .iter()
                .any(|op| self.materialization.output_order.contains_key(&op.id))
            {
                continue;
            }
            children.sort_by_key(|op| {
                (
                    self.materialization
                        .output_order
                        .get(&op.id)
                        .copied()
                        .unwrap_or(usize::MAX),
                    op.id,
                )
            });
        }

        // Build every raw import as a top-level node first. Metadata folding is a
        // separate exact-parent contraction pass below, after every possible
        // endpoint exists. This avoids making topology depend on input order.
        let mut result: Vec<HistoryNode> = Vec::with_capacity(self.ops.len());
        for op in &self.ops {
            // Structural relationship notes (ForkOf/SubagentOf/ReconnectsTo) are
            // pure edge bookkeeping — they never render as rows themselves, only
            // their virtual edges do. They remain addressable in the OpSet and
            // indexed in `relationship_notes`.
            if is_hidden_relation_fact(op) || self.materialization.hidden.contains(&op.id) {
                continue;
            }
            if matches!(op.kind, editchain_core::OpKind::Import(_)) {
                // A copied provider event is represented exactly once. The raw
                // occurrence and all of its normalized children remain in the
                // OpSet and resolve through `representative`; only the duplicate
                // top-level row is suppressed.
                if duplicate_event_occurrences.contains(&op.id) {
                    continue;
                }
                let children = children_of.get(&op.id);
                let content = content::collapsed_import(op, children, incomplete);
                let kind = collapsed_import_kind(op, children);
                let author = collapsed_import_author(op, children);
                // Semantic readability metadata is derived deterministically
                // here, where the raw envelope and its normalized children are
                // both available.
                let duplicate_of_event_msg = echo_pairs.is_paired_response_item(op);
                let meta = meta::for_collapsed_import(
                    op,
                    children.map(Vec::as_slice),
                    duplicate_of_event_msg,
                );
                let source_time = source_time_of(op);
                result.push(HistoryNode::CollapsedImport {
                    op: Arc::new(op.clone()),
                    source_time,
                    parent_override: None,
                    content,
                    kind,
                    author,
                    sub_ops: Vec::new(),
                    meta,
                });
            } else if folded.contains(&op.id) {
                // Drop normalized ops folded into their parent import op, and
                // record the fold so any parent/target that references this op
                // (relationship notes, causal parents of surviving rows) resolves
                // to the import's visible row instead of dangling.
                if let Some(import_id) = parent_import_of.get(&op.id).copied() {
                    let _: Option<OpId> = representative.insert(op.id, import_id);
                }
            } else {
                // Standalone op (e.g. ChainStart, or a message not tied to an
                // import) — keep as-is. Raw metadata is never attached to it:
                // only collapsed import rows can own imported sub-ops.
                let source_time = source_time_of(op);
                result.push(HistoryNode::EditOperation {
                    content: content::operation(op, !incomplete.contains(&op.id)),
                    op: Arc::new(op.clone()),
                    source_time,
                    parent_override: None,
                });
            }
        }

        Self::bundle_metadata_by_exact_parent(
            &mut result,
            &mut representative,
            &resolved_relationship_notes,
        );

        // Tool-grouping pass: fold each tool RESULT into its tool CALL's sub-ops
        // so a call + its result render as one row (the result revealed on click).
        //
        // A tool result is a CollapsedImport whose dominant Tool child is
        // `stage: Finish` with an empty name; its parent is the tool call's raw
        // import op. We attach the result's Tool op to the call's `sub_ops` and
        // drop the result from the top-level list, splicing its children to the
        // call (same technique as META bundling) so chain continuity holds.
        self.group_tool_results(
            &mut result,
            &children_of,
            &mut representative,
            &resolved_relationship_notes,
        );

        // The present row set is final once every fold pass has run. Any op id
        // that is neither a row nor resolvable to a row here is genuinely
        // external and must be dropped from edges — never fed to layout.
        let present = row_node_keys(&result);

        // Structural relationship notes are folded out of rendering entirely; give
        // each one a canonical representative (its anchor's visible row, falling
        // back to its first target's visible row) so the invariant holds even if
        // some op ever references a note id directly.
        for op in &self.ops {
            if !is_hidden_relation_fact(op) || representative.contains_key(&op.id) {
                continue;
            }
            let endpoint = op.parents.iter().next().copied().or_else(|| {
                if let editchain_core::OpKind::Note(n) = &op.kind {
                    n.target_ids.first().copied()
                } else {
                    None
                }
            });
            if let Some(rep) =
                endpoint.and_then(|id| canonical_op_id(id, &representative, &present))
            {
                let _: Option<OpId> = representative.insert(op.id, rep);
            }
        }

        // Re-key relationship notes by their canonical visible anchor so a folded
        // anchor (e.g. a ReconnectsTo on a collab Tool op) is still reachable
        // from the visible row that represents it. Provider entity targets have
        // already resolved within exact source context; every edge-construction
        // path lifts the resulting physical ids through the representative map,
        // dropping anything absent before lane allocation or edge geometry.
        let canonical_notes = Self::canonicalize_relationship_notes(
            &resolved_relationship_notes,
            &representative,
            &present,
            &duplicate_event_occurrences,
            &self.ops,
        );

        // Semantic-collapse invariant, checked once per collapse: every ordinary
        // op either renders as a row or resolves through the representative map
        // to a row. A structural note with no resolvable endpoint is inert and is
        // deliberately dropped, so it is the sole exception.
        debug_assert!(
            self.ops.iter().all(|op| {
                let key = op.id.to_string();
                present.contains(&key)
                    || representative.contains_key(&op.id)
                    || is_hidden_relation_fact(op)
            }),
            "every ordinary op must render as a row or resolve through the canonical representative map"
        );

        let unresolved_relations = self
            .ops
            .iter()
            .filter(|op| {
                is_hidden_relation_fact(op)
                    && canonical_op_id(op.id, &representative, &present).is_none()
            })
            .map(|op| op.id)
            .collect();
        CollapsedProjection {
            present,
            nodes: result,
            representative,
            canonical_notes,
            unresolved_relations,
        }
    }

    /// Fold metadata rows along their unique graph-parent path.
    ///
    /// Provider occurrences with a resolved exact parent take that relationship
    /// in preference to source order. Occurrences without a provider parent use
    /// their stored operation parent as a conservative fallback. A metadata
    /// chain contracts when that path reaches one non-META collapsed import
    /// row. If it instead ends at an exact same-session metadata root, its
    /// descendants coalesce into that root while the root itself remains visible
    /// as the session boundary. Cycles and cross-session paths remain visible.
    /// Legacy Codex token-usage imports are classified from their exact raw
    /// schema because their immutable stored tags predate `META` classification.
    fn bundle_metadata_by_exact_parent(
        result: &mut Vec<HistoryNode>,
        representative: &mut HashMap<OpId, OpId>,
        relationship_notes: &HashMap<OpId, Vec<Op>>,
    ) {
        #[derive(Clone, Copy)]
        enum MetadataResolution {
            Anchor(OpId),
            Root(OpId),
            Unresolved,
        }

        let present: std::collections::HashSet<OpId> =
            result.iter().filter_map(HistoryNode::op_id).collect();
        let is_metadata = |op: &Op| {
            op.tags.matches_any(editchain_core::Tags::META)
                || meta::is_codex_token_usage_record_import(op)
                || meta::is_legacy_claude_bundle_metadata_import(op)
        };
        let metadata_scopes: HashMap<OpId, _> = result
            .iter()
            .filter_map(|node| match node {
                HistoryNode::CollapsedImport { op, .. } if is_metadata(op) => {
                    Some((op.id, op.scope))
                }
                HistoryNode::EditOperation { .. }
                | HistoryNode::CollapsedImport { .. }
                | HistoryNode::ExecuteBundle { .. }
                | HistoryNode::PlanBundle { .. }
                | HistoryNode::WorkGroup { .. }
                | HistoryNode::GitCommit { .. } => None,
            })
            .collect();
        let metadata: std::collections::HashSet<OpId> = metadata_scopes.keys().copied().collect();
        if metadata.is_empty() {
            return;
        }
        let anchors: std::collections::HashSet<OpId> = result
            .iter()
            .filter_map(|node| match node {
                HistoryNode::CollapsedImport { op, .. } if !metadata.contains(&op.id) => {
                    Some(op.id)
                }
                HistoryNode::EditOperation { .. }
                | HistoryNode::CollapsedImport { .. }
                | HistoryNode::ExecuteBundle { .. }
                | HistoryNode::PlanBundle { .. }
                | HistoryNode::WorkGroup { .. }
                | HistoryNode::GitCommit { .. } => None,
            })
            .collect();

        // One exact visible parent per metadata row. More than one distinct
        // endpoint is not an ownership relation, so it cannot select a bundle.
        let mut direct_parent: HashMap<OpId, OpId> = HashMap::new();
        for node in result.iter() {
            let HistoryNode::CollapsedImport { op, .. } = node else {
                continue;
            };
            if !metadata.contains(&op.id) {
                continue;
            }
            let notes = relationship_notes.get(&op.id);
            let has_provider_parent = has_exact_provider_parent(op.id, notes.map(Vec::as_slice));
            let mut candidates = std::collections::BTreeSet::new();
            if !has_provider_parent {
                for parent in &op.parents {
                    if let Some(parent) = canonical_present_op(*parent, representative, &present) {
                        let _: bool = candidates.insert(parent);
                    }
                }
            }
            if let Some(notes) = notes {
                for note in notes {
                    let editchain_core::OpKind::Note(note) = &note.kind else {
                        continue;
                    };
                    if !is_visible_edge_relationship(note.relationship) {
                        continue;
                    }
                    for target in &note.target_ids {
                        if let Some(parent) =
                            canonical_present_op(*target, representative, &present)
                        {
                            let _: bool = candidates.insert(parent);
                        }
                    }
                }
            }
            if candidates.len() == 1 {
                let parent = candidates.into_iter().next();
                if let Some(parent) = parent.filter(|parent| *parent != op.id) {
                    let _: Option<OpId> = direct_parent.insert(op.id, parent);
                }
            }
        }

        // Resolve metadata-to-metadata paths with memoized path compression.
        // A same-session path with no semantic destination retains its oldest
        // metadata member as a visible root and folds only exact descendants
        // into it. This is the session-start shape: both the first activity and
        // the separately persisted session title can name `session_meta` as
        // their parent without creating a false title branch. Cross-session
        // paths and cycles remain uncontracted.
        let mut resolution: HashMap<OpId, MetadataResolution> = HashMap::new();
        for start in &metadata {
            if resolution.contains_key(start) {
                continue;
            }
            let mut path = Vec::new();
            let mut path_set = std::collections::HashSet::new();
            let mut current = *start;
            let resolved = loop {
                if let Some(known) = resolution.get(&current).copied() {
                    break known;
                }
                if !path_set.insert(current) {
                    break MetadataResolution::Unresolved;
                }
                path.push(current);
                let Some(parent) = direct_parent.get(&current).copied() else {
                    break MetadataResolution::Root(current);
                };
                if anchors.contains(&parent) {
                    break MetadataResolution::Anchor(parent);
                }
                if !metadata.contains(&parent) {
                    break MetadataResolution::Root(current);
                }
                if metadata_scopes.get(&current) != metadata_scopes.get(&parent) {
                    break MetadataResolution::Root(current);
                }
                current = parent;
            };
            for member in path {
                let _: Option<MetadataResolution> = resolution.insert(member, resolved);
            }
        }

        let destinations: HashMap<OpId, OpId> = resolution
            .into_iter()
            .filter_map(|(metadata, resolved)| match resolved {
                MetadataResolution::Anchor(anchor) => Some((metadata, anchor)),
                MetadataResolution::Root(root) if metadata != root => Some((metadata, root)),
                MetadataResolution::Root(_) | MetadataResolution::Unresolved => None,
            })
            .collect();
        if destinations.is_empty() {
            return;
        }

        // Collect before mutating so attachment order remains the operation
        // input order, independent of HashMap iteration.
        let mut attachments: HashMap<OpId, Vec<Arc<Op>>> = HashMap::new();
        let mut muted_anchors: std::collections::HashSet<OpId> = std::collections::HashSet::new();
        for node in result.iter() {
            let HistoryNode::CollapsedImport {
                op, sub_ops, meta, ..
            } = node
            else {
                continue;
            };
            let Some(anchor) = destinations.get(&op.id).copied() else {
                continue;
            };
            attachments.entry(anchor).or_default().push(Arc::clone(op));
            attachments
                .entry(anchor)
                .or_default()
                .extend(sub_ops.iter().cloned());
            if meta.chain_state == ChainState::Muted {
                let _: bool = muted_anchors.insert(anchor);
            }
        }
        for (&metadata, &anchor) in &destinations {
            let _: Option<OpId> = representative.insert(metadata, anchor);
        }
        result.retain(|node| {
            node.op_id()
                .is_none_or(|op_id| !destinations.contains_key(&op_id))
        });
        for node in result.iter_mut() {
            let HistoryNode::CollapsedImport {
                op, sub_ops, meta, ..
            } = node
            else {
                continue;
            };
            if let Some(mut folded) = attachments.remove(&op.id) {
                sub_ops.append(&mut folded);
            }
            if muted_anchors.contains(&op.id) {
                meta.chain_state = ChainState::Muted;
            }
        }
    }

    /// Fold tool-result nodes into their exactly correlated tool calls.
    ///
    /// Correlation requires both a non-empty provider `tool_call_id` matching a
    /// start op on the result's sole visible causal parent and that direct edge.
    /// The edge may traverse already-contracted metadata, but it may not cross
    /// an intervening semantic row. Call IDs are deliberately scoped by the
    /// causal edge rather than assumed globally unique across merged/copied
    /// histories. Ambiguous/orphan/non-direct results remain standalone.
    fn group_tool_results(
        &self,
        result: &mut Vec<HistoryNode>,
        children_of: &HashMap<OpId, Vec<&Op>>,
        representative: &mut HashMap<OpId, OpId>,
        relationship_notes: &HashMap<OpId, Vec<Op>>,
    ) {
        // Identify which nodes are tool results and which are tool calls.
        let mut is_result: Vec<bool> = Vec::with_capacity(result.len());
        for n in result.iter() {
            is_result.push(Self::node_is_tool_result(n, children_of));
        }

        let mut index_of: HashMap<String, usize> = HashMap::with_capacity(result.len());
        for (index, node) in result.iter().enumerate() {
            let _: Option<usize> = index_of.insert(node.node_key(), index);
        }
        let present = row_node_keys(result);

        // For each tool-result node, find its parent; if the parent is a tool
        // call, fold the result into it. Collect decisions first (no mutation of
        // `result` during iteration), then apply.
        let mut replacement: HashMap<OpId, OpId> = HashMap::new();
        let mut drop_idx: std::collections::HashSet<usize> = std::collections::HashSet::new();
        // parent index -> tool-result ops to attach as sub-ops.
        let mut attach: HashMap<usize, Vec<Arc<Op>>> = HashMap::new();
        // parent index -> structured outcome carried by the absorbed result row.
        let mut outcome_fold: HashMap<usize, Outcome> = HashMap::new();
        for (i, n) in result.iter().enumerate() {
            if !is_result.get(i).copied().unwrap_or(false) {
                continue;
            }
            let result_ids: Vec<Vec<u8>> = Self::tool_children(n, children_of)
                .into_iter()
                .filter(|tool| matches!(tool.stage, editchain_core::op::ToolStage::Finish))
                .filter_map(|tool| inline_payload_identity(&tool.tool_call_id))
                .collect();
            if result_ids.len() != 1 {
                continue;
            }
            let Some(result_id) = result_ids.first() else {
                continue;
            };
            let visible_parents = canonicalize_parents(
                n.parent_keys(self.git.links(), relationship_notes),
                representative,
                &present,
                &n.node_key(),
            );
            let [parent_key] = visible_parents.as_slice() else {
                continue;
            };
            let Some(&parent_idx) = index_of.get(parent_key) else {
                continue;
            };
            let parent_matches = !is_result.get(parent_idx).copied().unwrap_or(false)
                && result.get(parent_idx).is_some_and(|parent| {
                    Self::tool_children(parent, children_of)
                        .into_iter()
                        .any(|tool| {
                            matches!(tool.stage, editchain_core::op::ToolStage::Start)
                                && inline_payload_identity(&tool.tool_call_id).as_ref()
                                    == Some(result_id)
                        })
                });
            if parent_idx != i && parent_matches {
                let attached = attach.entry(parent_idx).or_default();
                attached.extend(Self::tool_result_ops(n, children_of));
                // The absorbed result row's structured outcome (derived from
                // its raw `status`/`errorMessage`/`exitCode`) carries over to
                // the visible call row, so the combined call+result keeps its
                // evidence-based outcome instead of degrading to unknown.
                let result_outcome = n.record_meta().outcome;
                if result_outcome != Outcome::Unknown {
                    let _: &mut Outcome = outcome_fold
                        .entry(parent_idx)
                        .and_modify(|acc| *acc = merge_outcome(*acc, result_outcome))
                        .or_insert(result_outcome);
                }
                // META records are bundled before tool-result grouping. If the
                // result row is then absorbed into its call, move those records
                // with it; otherwise they remain in storage but disappear from
                // the expanded renderer/debug view.
                attached.extend(n.sub_ops().iter().cloned());
                if let HistoryNode::CollapsedImport { op, .. } = n {
                    if let Some(parent_id) = result.get(parent_idx).and_then(HistoryNode::op_id) {
                        let _: Option<OpId> = replacement.insert(op.id, parent_id);
                    }
                    let _: bool = drop_idx.insert(i);
                }
            }
        }

        // If any META op bundled into a tool-result row that is now being folded into
        // its call, re-point the representative to the call so the lift in
        // `ordered_nodes`/`independent_chains` still reaches a present row.
        for value in representative.values_mut() {
            if let Some(repl) = replacement.get(value) {
                *value = *repl;
            }
        }

        // The dropped tool-result rows are folded bundles too: map each one to the
        // call that absorbed it (semantic-collapse invariant), so its folded
        // children, relationship notes, or later causal parents that reference the
        // result id resolve to the call's visible row instead of dangling.
        for (&dropped, &call) in &replacement {
            let _: &mut OpId = representative.entry(dropped).or_insert(call);
        }

        // Attach collected tool-result ops and their bundled metadata to the
        // call's sub-ops, folding the absorbed result's structured outcome
        // into the call row's metadata.
        for (parent_idx, ops) in &attach {
            if let Some(HistoryNode::CollapsedImport { sub_ops, meta, .. }) =
                result.get_mut(*parent_idx)
            {
                sub_ops.extend(ops.iter().cloned());
                if let Some(outcome) = outcome_fold.get(parent_idx).copied() {
                    meta.outcome = outcome;
                }
            }
        }

        // Remove dropped results and splice their children to the call.
        if !drop_idx.is_empty() {
            let mut kept: Vec<HistoryNode> = Vec::with_capacity(result.len());
            for (i, n) in result.drain(..).enumerate() {
                if drop_idx.contains(&i) {
                    continue;
                }
                kept.push(n);
            }
            *result = kept;
        }
    }

    /// Whether a collapsed-import node carries at least one tool result.
    fn node_is_tool_result(node: &HistoryNode, children_of: &HashMap<OpId, Vec<&Op>>) -> bool {
        Self::tool_children(node, children_of)
            .into_iter()
            .any(|tool| matches!(tool.stage, editchain_core::op::ToolStage::Finish))
    }

    /// Every normalized Tool child of one collapsed raw import.
    fn tool_children<'a>(
        node: &HistoryNode,
        children_of: &'a HashMap<OpId, Vec<&'a Op>>,
    ) -> Vec<&'a editchain_core::op::ToolOp> {
        let import_id = node.op_id();
        import_id
            .and_then(|id| children_of.get(&id))
            .into_iter()
            .flatten()
            .filter_map(|child| match &child.kind {
                editchain_core::OpKind::Tool(tool) => Some(tool),
                editchain_core::OpKind::ChainStart(_)
                | editchain_core::OpKind::Actor(_)
                | editchain_core::OpKind::Message(_)
                | editchain_core::OpKind::Command(_)
                | editchain_core::OpKind::File(_)
                | editchain_core::OpKind::Reflection(_)
                | editchain_core::OpKind::Import(_)
                | editchain_core::OpKind::Note(_)
                | editchain_core::OpKind::Error(_)
                | editchain_core::OpKind::GitCommit(_)
                | editchain_core::OpKind::GitLink(_)
                | editchain_core::OpKind::Unknown(_) => None,
            })
            .collect()
    }

    /// The normalized Tool result ops of a row (for attaching as sub-ops).
    fn tool_result_ops(node: &HistoryNode, children_of: &HashMap<OpId, Vec<&Op>>) -> Vec<Arc<Op>> {
        let HistoryNode::CollapsedImport { op, .. } = node else {
            return Vec::new();
        };
        children_of.get(&op.id).map_or_else(Vec::new, |children| {
            children
                .iter()
                .filter(|child| {
                    matches!(
                        &child.kind,
                        editchain_core::OpKind::Tool(tool)
                            if matches!(tool.stage, editchain_core::op::ToolStage::Finish)
                    )
                })
                .map(|child| Arc::new((*child).clone()))
                .collect()
        })
    }
}

/// Exact-equivalence key for occurrences of one provider event entity.
///
/// Current importers supply a canonical provider payload fingerprint that
/// excludes copy-local session/topology fields. Legacy occurrences retain the
/// byte-exact raw hash contract and never compare across key variants.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum OccurrencePayloadKey {
    /// Canonical content evidence supplied by the provider adapter.
    ProviderFingerprint(String),
    /// Byte-exact raw-line evidence retained for legacy facts.
    RawHash([u8; 32]),
}

/// Read exact importer-owned payload-equivalence evidence from an
/// `OccurrenceOf` note.
///
/// The fingerprint is intentionally opaque to the provider-neutral projection.
/// Requiring exact confidence and a complete 256-bit lowercase/uppercase hex
/// value prevents truncated display previews or arbitrary prose notes from
/// participating in occurrence contraction.
fn exact_occurrence_payload_fingerprint(note: &editchain_core::op::NoteOp) -> Option<String> {
    if note.relationship != NoteRelationship::OccurrenceOf {
        return None;
    }
    let Payload::Inline(evidence) = &note.content else {
        return None;
    };
    let value = serde_json::from_slice::<serde_json::Value>(evidence).ok()?;
    if value.get("confidence").and_then(serde_json::Value::as_str) != Some("exact") {
        return None;
    }
    let fingerprint = value
        .get("payloadFingerprint")
        .and_then(serde_json::Value::as_str)?;
    (fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| fingerprint.to_string())
}

/// Exact non-empty identity bytes carried inline by a provider lifecycle op.
///
/// Blob/empty identities stay unresolved; projection never guesses from names,
/// adjacency, or content when the correlation key is unavailable.
fn inline_payload_identity(payload: &Payload) -> Option<Vec<u8>> {
    match payload {
        Payload::Inline(bytes) if !bytes.is_empty() => Some(bytes.clone()),
        Payload::Empty | Payload::Blob(_) | Payload::Inline(_) => None,
    }
}

/// Conservative merge precedence for folded outcomes.
///
/// When multiple tool-result rows fold into one call, the combined outcome is
/// the most severe concluded outcome among them, so a later milder result can
/// never mask earlier evidence of a problem: `Failure` > `Cancelled` >
/// `Warning` > `Success`. `Unknown` ranks lowest; callers exclude it before
/// merging so it can never erase known evidence.
#[must_use]
fn outcome_severity(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Failure => 4,
        Outcome::Cancelled => 3,
        Outcome::Warning => 2,
        Outcome::Success => 1,
        Outcome::Unknown => 0,
    }
}

/// Deterministic fold of two concluded outcomes: the more severe wins; ties
/// keep the existing (earlier) outcome.
#[must_use]
fn merge_outcome(acc: Outcome, next: Outcome) -> Outcome {
    if outcome_severity(next) > outcome_severity(acc) {
        next
    } else {
        acc
    }
}
