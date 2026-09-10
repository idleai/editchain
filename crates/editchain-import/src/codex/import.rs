use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use editchain_core::clock::Clock;
use serde_json::Value;

use crate::cursor::resolve_source_cursor;
use crate::error::ImportError;
use crate::ids::{derive_session_id, SourcePosition, SourceStream};
use crate::model::{ImportOptions, ImportReport};
use crate::sink::{BlobSink, CursorStore, OpSink};
use crate::source_read::{SourceReadPlan, SourceReadState};

use super::discover::discover_rollouts;
use super::helper::HelperCommand;
use super::link::{
    emit_codex_relationship_notes, ActivityMarker, CompletionEvidence, LegacyCompletionEvidence,
    ThreadTopology, CODEX_NORMALIZATION_VERSION, SPAWN_SIGNAL_COLLAB_TOOL,
    SPAWN_SIGNAL_SUBAGENT_ACTIVITY,
};
use super::normalize::{
    build_raw_op, completed_agent_paths_from_tool, is_blank_line, normalized_ops_for_compaction,
    normalized_ops_for_inter_agent, normalized_ops_for_item, normalized_ops_for_turn,
    owning_thread_from_raw_line, ItemAnchor, NormalizeContext,
};
use super::projection::{parse_projection, FinalItem, ProjectionKind};
use super::session_git::session_git_link_op;
use super::title::{load_session_titles, raw_session_identity, session_title_op};

/// Normalization version that introduced exact session-start Git links.
const CODEX_GIT_NORMALIZATION_VERSION: u32 = 1;

/// Configuration for a Codex rollout discovery/import request.
#[derive(Debug, Clone)]
pub struct CodexDiscoveryRequest {
    /// Path to the workspace root. Used for deterministic source stream IDs
    /// and as the conservative project filter: a rollout is included when its
    /// projected `sessionMeta.cwd` is equal to or nested within this path, and
    /// excluded only when cwd is explicitly present and outside it.
    pub workspace_path: PathBuf,
    /// Root directory containing raw Codex rollout JSONL files, recursively
    /// (e.g. `~/.codex/sessions`; date trees are discovered automatically).
    pub raw_root: PathBuf,
}

/// Import all Codex rollouts under a raw sessions root into editchain ops.
///
/// This is the Codex counterpart of
/// [`crate::import::import_claude_code`]. For every physical `rollout-*.jsonl`
/// file it:
///
/// 1. Checks the cursor — unchanged files at the current normalization version
///    are skipped; older projections run a metadata-only upgrade; grown files
///    are read incrementally via the shared reader machinery;
/// 2. Detects truncation/rewrite from a persisted cursor and re-imports the
///    whole changed file under a new deterministic boot generation (bumped
///    and persisted per source by the [`CursorStore`]), so rewritten sources
///    never collide with their previous generation's op ids and never abort
///    unrelated rollouts;
/// 3. Invokes the configured helper over the whole file and validates the
///    `editchain-v1` projection (schema, ordinal sequence, record count);
/// 4. Applies the conservative workspace project filter from the projected
///    `sessionMeta.cwd` — foreign rollouts are skipped before any op or
///    cursor is written, so they never advance cursors and reruns stay
///    deterministic (see [`rollout_in_workspace`]);
/// 5. Emits one byte-exact raw `ImportOp` per new physical line, chained into
///    the per-file raw chain;
/// 6. Folds the projection's upsert/remove records to final logical items and
///    emits one normalized op per final item first seen in this batch;
/// 7. Persists the cursor and normalization version only after the whole file
///    succeeded.
///
/// Session scope is the owning thread id: bridge metadata first, then raw
/// `session_meta.payload.id`, then the rollout filename stem. `payload
/// session_id` is never used (Codex subagents carry parent session ids).
///
/// # Rewrite detection
///
/// The persisted cursor records an exact direct BLAKE3 hash of every accepted
/// source byte. Before an append or relocation, the importer re-hashes that
/// prefix byte-for-byte. Same-size and grown rewrites therefore start a new
/// source generation; file size alone never proves continuity. A trailing
/// partial line remains outside the accepted prefix and is read again once it
/// becomes complete.
///
/// # Errors
///
/// Returns [`ImportError`] when discovery fails, the helper fails or emits an
/// invalid projection, or a sink rejects an op. Truncated/rewritten files no
/// longer abort the import: they are re-imported under a new generation. On
/// error the affected file's cursor is not advanced.
///
/// # Panics
///
/// Panics if a folded item's first-seen ordinal falls outside the current
/// batch's line range — guarded by projection validation, so unreachable in
/// practice.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "counter increments and usize/u64 casts are bounded by file sizes and line counts"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "import orchestrator takes the request, options, helper bridge, and three sinks"
)]
#[expect(
    clippy::expect_used,
    reason = "first-seen ordinals are validated against the batch's line range by parse_projection"
)]
pub fn import_codex(
    request: &CodexDiscoveryRequest,
    options: &ImportOptions,
    helper: &HelperCommand,
    ops: &mut dyn OpSink,
    blobs: &mut dyn BlobSink,
    cursors: &mut dyn CursorStore,
) -> Result<ImportReport, ImportError> {
    let mut report = ImportReport::new();
    // Per-thread exact topology for the sink-independent relationship pass.
    let mut topology: Vec<ThreadTopology> = Vec::new();
    let session_titles = if options.normalize {
        load_session_titles(&request.raw_root)?
    } else {
        std::collections::HashMap::new()
    };

    let rollouts = discover_rollouts(&request.raw_root).map_err(ImportError::OpSink)?;
    report.files_discovered = rollouts.len();
    let workspace_str = request.workspace_path.to_str().unwrap_or("/workspace");

    for rollout in &rollouts {
        let resolved = resolve_source_cursor(
            cursors,
            "codex",
            &request.raw_root,
            &rollout.path,
            workspace_str,
        )?;
        let cursor_key = resolved.canonical_key;
        let state_key = resolved.state_key;
        let source_node = resolved.source_node;
        let migrates_legacy_key = cursor_key != state_key;
        let existing_cursor = resolved.cursor;
        let plan = SourceReadPlan::capture(
            &rollout.path,
            existing_cursor.as_ref(),
            cursors.get_generation(&state_key)?,
            options.source_limits,
        )?;
        let raw_identity = if session_titles.is_empty() {
            None
        } else {
            raw_session_identity(plan.captured_path())?
        };
        let indexed_title = raw_identity.as_ref().and_then(|identity| {
            session_titles.get(&identity.thread_id).or_else(|| {
                identity
                    .parent_thread_id
                    .as_ref()
                    .and_then(|parent| session_titles.get(parent))
            })
        });
        let previous_session_title_hash = existing_cursor
            .as_ref()
            .and_then(|cursor| cursor.session_title_hash);
        let needs_session_title_refresh = options.normalize
            && indexed_title
                .is_some_and(|title| previous_session_title_hash != Some(title.source_hash));
        let needs_git_upgrade = options.normalize
            && existing_cursor.as_ref().is_some_and(|cursor| {
                cursor.normalization_version < CODEX_GIT_NORMALIZATION_VERSION
            });
        let needs_normalization_upgrade = options.normalize
            && existing_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.normalization_version < CODEX_NORMALIZATION_VERSION);
        let needs_cursor_upgrade = migrates_legacy_key
            || existing_cursor.as_ref().is_some_and(|cursor| {
                cursor.source_node != Some(source_node) || cursor.content_hash_version < 1
            });

        if plan.state() == SourceReadState::Unchanged
            && !needs_normalization_upgrade
            && !needs_session_title_refresh
            && !needs_cursor_upgrade
        {
            continue;
        }
        let boot = plan.generation();
        let start_seq = plan.start_seq();
        let full_read = matches!(
            plan.state(),
            SourceReadState::Fresh | SourceReadState::Rewritten
        );
        let lines = plan.lines();
        let mut new_cursor = plan.checkpoint().clone();

        // Deterministic source stream per physical file (the file path is the
        // owning stream identity). The boot generation separates rewritten
        // generations from the original import and from each other, so op ids
        // never collide across generations of one file.
        let stream = SourceStream::new(source_node, boot);
        // The bridge counts every physical line it reads, including blank lines
        // and one trailing partial line; align `expected_total` with it.
        let has_partial = plan.partial().is_some();
        let partial_blank = plan.partial() == Some(true);
        let expected_total = start_seq + lines.len() as u64 + u64::from(has_partial);

        if expected_total == 0 {
            // Empty file — nothing to project or emit; still checkpoint.
            report.files_processed += 1;
            if options.normalize {
                new_cursor.normalization_version = new_cursor
                    .normalization_version
                    .max(CODEX_NORMALIZATION_VERSION);
            }
            new_cursor.source_node = Some(source_node);
            new_cursor.content_hash_version = 1;
            if boot > 0 && (migrates_legacy_key || plan.state() == SourceReadState::Rewritten) {
                cursors.set_generation(&cursor_key, boot)?;
            }
            cursors.set_cursor(&cursor_key, &new_cursor)?;
            continue;
        }

        // On a full-file import we know the exact non-blank line set and can
        // enforce an exact line-record count; incremental imports cannot (old
        // blank-line layout is not persisted), so they validate ordinals only.
        // Rewrites count as full imports: the whole rewritten file is re-read
        // and must be projected exactly once.
        let expected_records = if full_read {
            let non_blank = lines.iter().filter(|l| !is_blank_line(&l.data)).count();
            Some(non_blank as u64 + u64::from(has_partial && !partial_blank))
        } else {
            None
        };

        // Run the helper over the whole file and validate/fold its projection
        // BEFORE emitting anything, so a bridge failure leaves no partial state.
        let stdout = helper.run_captured(plan.captured_path(), &rollout.path)?;
        let projection =
            parse_projection(&stdout, expected_total, expected_records).map_err(|e| {
                ImportError::ProjectionProtocol {
                    path: rollout.path.clone(),
                    detail: match e {
                        crate::codex::projection::ProjectionError::Protocol(detail) => detail,
                    },
                }
            })?;
        validate_new_line_records(
            &projection.line_ordinals,
            lines,
            start_seq,
            (has_partial && !partial_blank).then_some(expected_total),
            &rollout.path,
        )?;

        // Conservative workspace project filter. A rollout is included when
        // its projected session_meta cwd is equal to or nested within the
        // requested workspace and excluded only when cwd is explicitly present
        // and outside it; absent/unclassifiable cwd values are included for
        // compatibility. This runs before any op emission or cursor write, so
        // excluded rollouts never advance cursors and reruns are idempotent.
        let excluded = projection
            .session_meta
            .as_ref()
            .and_then(|meta| meta.cwd.as_deref())
            .is_some_and(|cwd| !rollout_in_workspace(&request.workspace_path, cwd));
        if excluded {
            continue;
        }
        report.files_processed += 1;
        report.malformed += projection.malformed;

        // Owning thread id: bridge metadata (sessionMeta.threadId / final
        // threadId), then raw session_meta.payload.id, then the rollout
        // filename stem. Never payload.session_id.
        let owning_thread = match projection.owning_thread.clone() {
            Some(thread) => thread,
            None => owning_thread_from_rollout(plan.captured_path())?
                .unwrap_or_else(|| rollout.session_id.clone()),
        };
        let session_id = derive_session_id(&owning_thread);
        let session_title = session_titles.get(&owning_thread).or_else(|| {
            projection
                .session_meta
                .as_ref()
                .and_then(|meta| meta.parent_thread_id.as_ref())
                .and_then(|parent| session_titles.get(parent))
        });
        let mut topo = ThreadTopology {
            thread_id: owning_thread.clone(),
            parent_thread_id: projection
                .session_meta
                .as_ref()
                .and_then(|m| m.parent_thread_id.clone()),
            forked_from_id: projection
                .session_meta
                .as_ref()
                .and_then(|m| m.forked_from_id.clone()),
            agent_path: projection
                .session_meta
                .as_ref()
                .and_then(|m| m.agent_path.clone()),
            first_raw: (new_cursor.ops_emitted > 0)
                .then(|| stream.op_from_position(SourcePosition::raw(1)))
                .transpose()?,
            last_raw: (new_cursor.ops_emitted > 0)
                .then(|| stream.op_from_position(SourcePosition::raw(new_cursor.ops_emitted)))
                .transpose()?,
            markers: Vec::new(),
            completions: Vec::new(),
            legacy_completions: Vec::new(),
        };

        // Capture exact lifecycle endpoints from the complete helper
        // projection, including metadata-only version upgrades. Endpoints are
        // physical source occurrences, so their IDs do not depend on derived
        // lane allocation or on whether this batch replayed normalized rows.
        if options.normalize {
            collect_topology_evidence(&projection.final_items, &stream, &mut topo)?;
        }

        // Emit raw ops for the new lines, chaining across the cursor boundary.
        let mut prev_raw_id = if start_seq > 0 {
            Some(stream.op_from_position(SourcePosition::raw(start_seq))?)
        } else {
            None
        };
        let mut clocks: Vec<Clock> = Vec::with_capacity(lines.len());
        for (i, line) in lines.iter().enumerate() {
            let seq = start_seq + i as u64 + 1;
            let op = build_raw_op(
                &line.data,
                line.hash,
                &stream,
                seq,
                &owning_thread,
                session_id,
                prev_raw_id,
                blobs,
            )?;
            clocks.push(op.clock);
            let _: bool = ops.accept_op(&op)?;
            report.raw_ops += 1;
            prev_raw_id = Some(op.id);
        }

        // Codex keeps user-visible thread renames in `session_index.jsonl`,
        // outside the rollout. Persist the selected bounded title as a
        // deterministic metadata op attached to this source's first raw row.
        // A parent thread's title is inherited by a named subagent session so
        // the renderer can display `title · nickname` without consulting live
        // Codex state.
        let should_emit_session_title = options.normalize
            && session_title.is_some_and(|title| {
                full_read || previous_session_title_hash != Some(title.source_hash)
            });
        if should_emit_session_title {
            if let (Some(title), Some(first_raw)) = (
                session_title,
                (new_cursor.ops_emitted > 0)
                    .then(|| stream.op_from_position(SourcePosition::raw(1)))
                    .transpose()?,
            ) {
                let title_op =
                    session_title_op(title, &owning_thread, session_id, first_raw, blobs)?;
                let accepted = ops.accept_op(&title_op)?;
                report.normalized_ops = report.normalized_ops.saturating_add(usize::from(accepted));
            }
        }

        // Codex records one exact Git snapshot on `session_meta`. Materialize
        // that fact once, at the source record that starts the session. There
        // is deliberately no command-text or timestamp inference here: an
        // absent/invalid hash or an unresolvable local repository yields no
        // link. Appends do not replay the deterministic session-start link.
        if options.normalize && (start_seq == 0 || needs_git_upgrade) {
            let raw_batch_end = u64::try_from(lines.len())
                .unwrap_or(u64::MAX)
                .saturating_add(start_seq);
            if let (Some(meta), Some(source_ordinal)) = (
                projection.session_meta.as_ref(),
                projection.session_meta_source_ordinal,
            ) {
                if source_ordinal <= raw_batch_end {
                    if let Some(op) = session_git_link_op(
                        &request.workspace_path,
                        meta,
                        source_ordinal,
                        &stream,
                        session_id,
                    )? {
                        let _: bool = ops.accept_op(&op)?;
                        report.normalized_ops += 1;
                    }
                }
            }
        }

        // Emit normalized ops: fresh items (first seen after the cursor)
        // anchored at their first-seen ordinal, deterministic update ops for
        // items first seen before the cursor but changed after it (anchored at
        // their last-seen ordinal), and per-line inter-agent/compaction lanes.
        // All normalized ops at one ordinal share the derived lane counters so
        // ids never collide and stay deterministic across repeated runs.
        if options.normalize {
            let batch_end = start_seq + lines.len() as u64;
            let clock_at = |ordinal: u64| -> Clock {
                let clock_idx = usize::try_from(ordinal - start_seq - 1)
                    .expect("anchor ordinal fits usize and is within the batch");
                *clocks
                    .get(clock_idx)
                    .expect("anchor ordinal validated against batch line range")
            };
            let mut ctx = NormalizeContext {
                stream: &stream,
                thread: &owning_thread,
                session_id,
                lanes: std::collections::HashMap::new(),
                batch_end,
                include_thinking: options.include_thinking,
                blobs,
            };
            for item in &projection.final_items {
                if item.first_seen > start_seq {
                    // Fresh item: anchor at first-seen. Items anchored to a
                    // trailing partial line emit on a later run once the line
                    // completes (fold state is recomputed per run).
                    if item.first_seen > batch_end {
                        continue;
                    }
                    let first_seen_clock = clock_at(item.first_seen);
                    let last_seen_clock = if item.last_seen <= batch_end {
                        clock_at(item.last_seen)
                    } else {
                        first_seen_clock
                    };
                    let item_ops = normalized_ops_for_item(
                        item,
                        ItemAnchor::FirstSeen,
                        first_seen_clock,
                        last_seen_clock,
                        &mut ctx,
                    )?;
                    for op in &item_ops {
                        let _: bool = ops.accept_op(op)?;
                        report.normalized_ops += 1;
                    }
                } else if item.last_seen > start_seq {
                    // Deterministic update: first seen before the cursor,
                    // changed after it. Emit the final state anchored at the
                    // change ordinal so no stale content is left behind.
                    if item.last_seen > batch_end {
                        continue;
                    }
                    let item_ops = normalized_ops_for_item(
                        item,
                        ItemAnchor::LastSeen,
                        clock_at(item.last_seen),
                        clock_at(item.last_seen),
                        &mut ctx,
                    )?;
                    for op in &item_ops {
                        let _: bool = ops.accept_op(op)?;
                        report.normalized_ops += 1;
                    }
                }
            }
            for line in &projection.inter_agent_lines {
                if line.source_ordinal <= start_seq || line.source_ordinal > batch_end {
                    continue;
                }
                let line_ops =
                    normalized_ops_for_inter_agent(line, clock_at(line.source_ordinal), &mut ctx)?;
                for op in &line_ops {
                    let _: bool = ops.accept_op(op)?;
                    report.normalized_ops += 1;
                }
            }
            for line in &projection.compacted_lines {
                if line.source_ordinal <= start_seq || line.source_ordinal > batch_end {
                    continue;
                }
                let line_ops =
                    normalized_ops_for_compaction(line, clock_at(line.source_ordinal), &mut ctx)?;
                for op in &line_ops {
                    let _: bool = ops.accept_op(op)?;
                    report.normalized_ops += 1;
                }
            }
            // Persist turn identity and metadata: one provider-neutral note per
            // fresh turn, anchored at the turn's first-seen ordinal. Lanes are
            // allocated after the item/inter-agent/compaction lanes at that
            // ordinal, so op ids stay deterministic and existing anchors are
            // untouched.
            let mut items_by_turn: std::collections::HashMap<&str, (u64, usize)> =
                std::collections::HashMap::new();
            for item in &projection.final_items {
                if item.first_seen <= start_seq {
                    continue;
                }
                let entry = items_by_turn
                    .entry(item.turn_id.as_str())
                    .or_insert((item.first_seen, 0));
                entry.0 = entry.0.min(item.first_seen);
                entry.1 += 1;
            }
            for turn in &projection.turns {
                let Some((first_ordinal, item_count)) = items_by_turn.get(turn.turn_id.as_str())
                else {
                    continue;
                };
                let first_ordinal = *first_ordinal;
                let item_count = *item_count;
                if first_ordinal > batch_end {
                    continue;
                }
                let turn_ops = normalized_ops_for_turn(
                    turn,
                    first_ordinal,
                    item_count,
                    clock_at(first_ordinal),
                    &mut ctx,
                )?;
                for op in &turn_ops {
                    let _: bool = ops.accept_op(op)?;
                    report.normalized_ops += 1;
                }
            }
        }

        // Only persist the cursor after the whole file succeeded. The version
        // checkpoint makes metadata-only upgrades one-shot while preserving a
        // future version written by a newer importer.
        if options.normalize {
            new_cursor.normalization_version = new_cursor
                .normalization_version
                .max(CODEX_NORMALIZATION_VERSION);
            if let Some(title) = session_title {
                new_cursor.session_title_hash = Some(title.source_hash);
            }
        }
        new_cursor.source_node = Some(source_node);
        new_cursor.content_hash_version = 1;
        if boot > 0 && (migrates_legacy_key || plan.state() == SourceReadState::Rewritten) {
            cursors.set_generation(&cursor_key, boot)?;
        }
        cursors.set_cursor(&cursor_key, &new_cursor)?;
        if options.normalize {
            topology.push(topo);
        }
    }

    // Sink-independent exact topology pass. Missing or ambiguous visible
    // endpoints remain unlinked; no timestamp/file-order fallback is allowed.
    let relationship_notes = emit_codex_relationship_notes(&topology)?;
    for note in &relationship_notes {
        let _: bool = ops.accept_op(note)?;
        report.normalized_ops += 1;
    }

    Ok(report)
}

/// Collect exact lifecycle endpoints from a complete helper projection.
///
/// The bridge projection is always computed over the whole rollout, including
/// on an incremental append or metadata-only upgrade. Anchoring evidence to raw
/// source occurrences keeps relation identity independent from normalized lane
/// allocation. Evidence on a trailing partial line is ignored until that line
/// becomes a durable raw occurrence on a later import.
fn collect_topology_evidence(
    items: &[FinalItem],
    stream: &SourceStream,
    topology: &mut ThreadTopology,
) -> Result<(), ImportError> {
    let last_complete_ordinal = topology.last_raw.map_or(0, |op| op.seq >> 16);
    for item in items {
        if collect_subagent_activity_marker(item, stream, topology, last_complete_ordinal)? {
            continue;
        }
        collect_collab_spawn_markers(item, stream, topology, last_complete_ordinal)?;

        if item.kind != ProjectionKind::Tool
            || item.last_seen == 0
            || item.last_seen > last_complete_ordinal
        {
            continue;
        }
        let evidence_op = stream.op_from_position(SourcePosition::raw(item.last_seen))?;
        if let Some(agents_states) = item.payload.get("agentsStates").and_then(Value::as_object) {
            for (child_thread, state) in agents_states {
                let completed = state
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| status.eq_ignore_ascii_case("completed"));
                if completed {
                    topology.completions.push(CompletionEvidence {
                        agent_thread_id: child_thread.clone(),
                        op_id: evidence_op,
                    });
                }
            }
        }
        if item.payload.get("tool").and_then(Value::as_str) == Some("list_agents") {
            for agent_path in completed_agent_paths_from_tool(&item.payload) {
                topology.legacy_completions.push(LegacyCompletionEvidence {
                    agent_path,
                    op_id: evidence_op,
                });
            }
        }
    }
    Ok(())
}

/// Preserve the older dedicated subagent-activity activation signal.
fn collect_subagent_activity_marker(
    item: &FinalItem,
    stream: &SourceStream,
    topology: &mut ThreadTopology,
    last_complete_ordinal: u64,
) -> Result<bool, ImportError> {
    if item.kind != ProjectionKind::Note
        || item.first_seen == 0
        || item.first_seen > last_complete_ordinal
    {
        return Ok(false);
    }
    let Some(agent_thread) = item
        .payload
        .get("agentThreadId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return Ok(true);
    };
    topology.markers.push(ActivityMarker {
        agent_thread_id: agent_thread.to_string(),
        agent_path: item
            .payload
            .get("agentPath")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        op_id: stream.op_from_position(SourcePosition::raw(item.first_seen))?,
        started: item
            .payload
            .get("activityKind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("started")),
        signal: SPAWN_SIGNAL_SUBAGENT_ACTIVITY,
    });
    Ok(true)
}

/// Capture the exact activation shape emitted by current Codex rollouts.
fn collect_collab_spawn_markers(
    item: &FinalItem,
    stream: &SourceStream,
    topology: &mut ThreadTopology,
    last_complete_ordinal: u64,
) -> Result<(), ImportError> {
    if item.kind != ProjectionKind::Tool
        || item.first_seen == 0
        || item.first_seen > last_complete_ordinal
        || item
            .payload
            .get("tool")
            .and_then(Value::as_str)
            .is_none_or(|tool| tool != "spawnAgent")
        || item.payload.get("senderThreadId").and_then(Value::as_str)
            != Some(topology.thread_id.as_str())
    {
        return Ok(());
    }
    let Some(receivers) = item
        .payload
        .get("receiverThreadIds")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let occurrence = stream.op_from_position(SourcePosition::raw(item.first_seen))?;
    let receiver_threads: BTreeSet<String> = receivers
        .iter()
        .filter_map(Value::as_str)
        .filter(|thread| !thread.is_empty())
        .map(ToString::to_string)
        .collect();
    for agent_thread_id in receiver_threads {
        topology.markers.push(ActivityMarker {
            agent_thread_id,
            agent_path: None,
            op_id: occurrence,
            started: true,
            signal: SPAWN_SIGNAL_COLLAB_TOOL,
        });
    }
    Ok(())
}

/// Decide whether a rollout belongs to the requested workspace.
///
/// Conservative project filter over the versioned projection: include the
/// rollout when its projected `sessionMeta.cwd` is equal to or nested within
/// `workspace` (component-wise, so a `workspace-other` sibling never matches),
/// and exclude it only when cwd is explicitly present and outside. Empty or
/// relative cwd values are unclassifiable and are included, preserving
/// compatibility with older bridge projections that carry no cwd.
///
/// Paths are canonicalized when they exist on this machine (resolving
/// symlinks and `.`/`..` components); paths recorded on another machine and
/// unresolvable here fall back to their literal form, so they are never
/// misclassified as foreign. A relative workspace is resolved against the
/// current directory first.
#[must_use]
fn rollout_in_workspace(workspace: &Path, cwd: &str) -> bool {
    let cwd_path = Path::new(cwd);
    if cwd_path.as_os_str().is_empty() || cwd_path.is_relative() {
        // Unclassifiable — conservative include.
        return true;
    }
    let workspace = resolve_workspace_path(workspace);
    let workspace = canonical_or_literal(&workspace);
    let cwd = canonical_or_literal(cwd_path);
    cwd.starts_with(&workspace)
}

/// Resolve a possibly-relative workspace path against the current directory.
fn resolve_workspace_path(workspace: &Path) -> PathBuf {
    if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| workspace.to_path_buf(), |cwd| cwd.join(workspace))
    }
}

/// Canonicalize a path when it exists locally; otherwise keep it as given.
fn canonical_or_literal(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Scan the physical rollout for its raw `session_meta.payload.id` fallback.
///
/// The helper projection is normally authoritative. Reading from the whole file
/// here keeps the fallback stable on incremental imports, where `lines` contains
/// only the appended batch and no longer includes the original session metadata.
fn owning_thread_from_rollout(path: &Path) -> Result<Option<String>, ImportError> {
    let file = std::fs::File::open(path).map_err(ImportError::Io)?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let count =
            std::io::BufRead::read_until(&mut reader, b'\n', &mut line).map_err(ImportError::Io)?;
        if count == 0 {
            return Ok(None);
        }
        if let Some(thread) = owning_thread_from_raw_line(&line) {
            return Ok(Some(thread));
        }
    }
}

/// Require a helper line record for every newly read non-blank physical line.
///
/// Full imports also enforce an exact total record count in `parse_projection`.
/// On incremental imports the old blank-line layout is not in the cursor, so
/// this targeted check prevents an empty or truncated helper stream from
/// silently advancing past newly appended semantic content.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "new-line ordinals are bounded by the cursor count and current batch length"
)]
#[expect(
    clippy::as_conversions,
    reason = "usize to u64 is safe for in-memory line counts"
)]
fn validate_new_line_records(
    projected_ordinals: &[u64],
    lines: &[crate::claude_code::reader::LineWithHash],
    start_seq: u64,
    partial_non_blank_ordinal: Option<u64>,
    path: &Path,
) -> Result<(), ImportError> {
    let missing_complete = lines.iter().enumerate().find_map(|(index, line)| {
        if is_blank_line(&line.data) {
            return None;
        }
        let ordinal = start_seq + index as u64 + 1;
        projected_ordinals
            .binary_search(&ordinal)
            .is_err()
            .then_some(ordinal)
    });
    let missing = missing_complete.or_else(|| {
        partial_non_blank_ordinal
            .filter(|ordinal| projected_ordinals.binary_search(ordinal).is_err())
    });
    match missing {
        Some(ordinal) => Err(ImportError::ProjectionProtocol {
            path: path.to_path_buf(),
            detail: format!(
                "helper omitted line record for new non-blank source ordinal {ordinal}"
            ),
        }),
        None => Ok(()),
    }
}
