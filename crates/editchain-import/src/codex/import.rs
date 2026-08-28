use std::path::{Path, PathBuf};

use editchain_core::clock::Clock;
use serde_json::Value;

use crate::claude_code::reader::read_session_file;
use crate::cursor::{check_file_generation, read_new_bytes};
use crate::error::ImportError;
use crate::ids::{derive_session_id, derive_source_stream, SourcePosition};
use crate::model::{ImportOptions, ImportReport};
use crate::sink::{BlobSink, CursorStore, MemoryOpSink, OpSink};

use super::discover::discover_rollouts;
use super::helper::HelperCommand;
use super::link::{
    emit_codex_relationship_notes, ActivityMarker, CompletionEvidence, LegacyCompletionEvidence,
    ThreadTopology,
};
use super::normalize::{
    build_raw_op, completed_agent_paths_from_tool, is_blank_line, normalized_ops_for_compaction,
    normalized_ops_for_inter_agent, normalized_ops_for_item, normalized_ops_for_turn,
    owning_thread_from_raw_line, ItemAnchor, NormalizeContext,
};
use super::projection::{parse_projection, ProjectionKind};

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

/// How one rollout's raw bytes are read for this run.
///
/// [`ReadState::Fresh`] and [`ReadState::Rewritten`] re-read the whole file
/// from byte 0 (a "full read": the exact non-blank line set is known and the
/// projection record count is enforced); [`ReadState::Append`] reads only the
/// bytes past the persisted cursor.
enum ReadState {
    /// First import (or a reset re-import) of the source.
    Fresh {
        /// Boot generation for the deterministic source stream.
        boot: u32,
        /// Complete lines read from the whole file.
        lines: Vec<crate::claude_code::reader::LineWithHash>,
        /// Cursor covering the whole file.
        new_cursor: crate::sink::CursorValue,
    },
    /// The source grew since the last read; same generation, incremental read.
    Append {
        /// Boot generation of the current generation (unchanged by appends).
        boot: u32,
        /// First ordinal of this batch (one past the cursor's emitted count).
        start_seq: u64,
        /// Complete lines read past the cursor.
        lines: Vec<crate::claude_code::reader::LineWithHash>,
        /// Cursor covering the old plus new bytes.
        new_cursor: crate::sink::CursorValue,
    },
    /// The source was truncated/rewritten; bumped to a new generation and
    /// re-read whole from byte 0.
    Rewritten {
        /// New boot generation, persisted by the cursor store.
        boot: u32,
        /// Complete lines read from the whole rewritten file.
        lines: Vec<crate::claude_code::reader::LineWithHash>,
        /// Cursor covering the whole rewritten file.
        new_cursor: crate::sink::CursorValue,
    },
}

/// Import all Codex rollouts under a raw sessions root into editchain ops.
///
/// This is the Codex counterpart of
/// [`crate::import::import_claude_code`]. For every physical `rollout-*.jsonl`
/// file it:
///
/// 1. Checks the cursor — unchanged files are skipped (idempotent); grown
///    files are read incrementally via the shared reader machinery;
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
/// 7. Persists the cursor only after the whole file succeeded.
///
/// Session scope is the owning thread id: bridge metadata first, then raw
/// `session_meta.payload.id`, then the rollout filename stem. `payload
/// session_id` is never used (Codex subagents carry parent session ids).
///
/// # Rewrite detection residual
///
/// The persisted cursor records the source's size, read offset, and a
/// cumulative hash of the bytes read so far. Only a size decrease (truncation
/// or a rewrite that shrinks the file) is detectable reliably. An exact
/// same-size rewrite is indistinguishable from an unchanged file and is
/// skipped; a rewrite that grows the file looks like an append, so bytes at
/// the old read offsets are assumed unchanged. Both cases are accepted
/// residuals of the current cursor design (no per-file content hash of the
/// full file is persisted); deleting the source's cursor file re-imports it.
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
    // Per-thread topology for the structural linking post-pass (explicit
    // session metadata + subagent lifecycle markers found in this run's ops).
    let mut topology: Vec<ThreadTopology> = Vec::new();

    let rollouts = discover_rollouts(&request.raw_root).map_err(ImportError::OpSink)?;
    report.files_discovered = rollouts.len();
    let workspace_str = request.workspace_path.to_str().unwrap_or("/workspace");

    for rollout in &rollouts {
        let cursor_key = rollout.path.to_string_lossy().to_string();
        let existing_cursor = cursors.get_cursor(&cursor_key)?;

        // Decide how to read this rollout. A persisted cursor whose source was
        // truncated or rewritten is NOT fatal: the file is bumped to a new
        // deterministic boot generation (persisted per source by the cursor
        // store) and re-imported whole from byte 0, so its new ops never
        // collide with the previous generation's ids and unrelated rollouts
        // keep importing. Only size decreases are detectable with the current
        // cursor design; an exact same-size rewrite is indistinguishable from
        // an unchanged file and is skipped (documented residual).
        let read = if let Some(cursor) = &existing_cursor {
            match check_file_generation(&rollout.path, cursor) {
                Ok(true) => {
                    // Unchanged — idempotent skip.
                    continue;
                }
                Ok(false) => {
                    // Grew — incremental append on the current generation's stream.
                    let boot = cursors.get_generation(&cursor_key)?;
                    let (lines, _bytes_read, new_cursor) =
                        read_session_file(&rollout.path, Some(cursor))?;
                    ReadState::Append {
                        boot,
                        start_seq: cursor.ops_emitted,
                        lines,
                        new_cursor,
                    }
                }
                Err(ImportError::SourceGenerationChanged { .. }) => {
                    // Truncated/rewritten — bump to a new generation and read
                    // the whole file from scratch (a fresh read, so the stale
                    // cursor never re-triggers the generation error).
                    let generation = cursors.get_generation(&cursor_key)?.saturating_add(1);
                    cursors.set_generation(&cursor_key, generation)?;
                    let (lines, _bytes_read, new_cursor) = read_session_file(&rollout.path, None)?;
                    ReadState::Rewritten {
                        boot: generation,
                        lines,
                        new_cursor,
                    }
                }
                Err(e) => return Err(e),
            }
        } else {
            // First import — generation 0 (unless a reset re-import is
            // replaying a previously rewritten source).
            let boot = cursors.get_generation(&cursor_key)?;
            let (lines, _bytes_read, new_cursor) = read_session_file(&rollout.path, None)?;
            ReadState::Fresh {
                boot,
                lines,
                new_cursor,
            }
        };

        let (boot, start_seq, full_read, lines, new_cursor) = match read {
            ReadState::Fresh {
                boot,
                lines,
                new_cursor,
            }
            | ReadState::Rewritten {
                boot,
                lines,
                new_cursor,
            } => (boot, 0, true, lines, new_cursor),
            ReadState::Append {
                boot,
                start_seq,
                lines,
                new_cursor,
            } => (boot, start_seq, false, lines, new_cursor),
        };

        // Deterministic source stream per physical file (the file path is the
        // owning stream identity). The boot generation separates rewritten
        // generations from the original import and from each other, so op ids
        // never collide across generations of one file.
        let stream = derive_source_stream(workspace_str, &cursor_key, boot);
        // The bridge counts every physical line it reads, including blank lines
        // and one trailing partial line; align `expected_total` with it.
        let (has_partial, partial_blank) = trailing_partial(&rollout.path, &new_cursor)?;
        let expected_total = start_seq + lines.len() as u64 + u64::from(has_partial);

        if expected_total == 0 {
            // Empty file — nothing to project or emit; still checkpoint.
            report.files_processed += 1;
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
        let stdout = helper.run(&rollout.path)?;
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
            &lines,
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
            None => owning_thread_from_rollout(&rollout.path)?
                .unwrap_or_else(|| rollout.session_id.clone()),
        };
        let session_id = derive_session_id(&owning_thread);
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
            markers: Vec::new(),
            completions: Vec::new(),
            legacy_completions: Vec::new(),
        };

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
                    if item.kind == ProjectionKind::Note {
                        if let Some(agent_thread) = item
                            .payload
                            .get("agentThreadId")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            if let Some(marker_op) = item_ops.first() {
                                topo.markers.push(ActivityMarker {
                                    agent_thread_id: agent_thread.to_string(),
                                    agent_path: item
                                        .payload
                                        .get("agentPath")
                                        .and_then(Value::as_str)
                                        .filter(|s| !s.is_empty())
                                        .map(ToString::to_string),
                                    op_id: marker_op.id,
                                    started: item
                                        .payload
                                        .get("activityKind")
                                        .and_then(Value::as_str)
                                        .is_some_and(|k| k.eq_ignore_ascii_case("started")),
                                });
                            }
                        }
                    } else if item.kind == ProjectionKind::Tool {
                        // Explicit per-child completion evidence only. The
                        // collab tool call's own status/tool never completes a
                        // child; subAgentActivity kinds never do either.
                        if let Some(marker_op) = item_ops.last() {
                            if let Some(agents_states) =
                                item.payload.get("agentsStates").and_then(Value::as_object)
                            {
                                for (child_thread, state) in agents_states {
                                    let completed = state
                                        .get("status")
                                        .and_then(Value::as_str)
                                        .is_some_and(|s| s.eq_ignore_ascii_case("completed"));
                                    if completed {
                                        topo.completions.push(CompletionEvidence {
                                            agent_thread_id: child_thread.clone(),
                                            op_id: marker_op.id,
                                        });
                                    }
                                }
                            }
                            if item.payload.get("tool").and_then(Value::as_str)
                                == Some("list_agents")
                            {
                                for path in completed_agent_paths_from_tool(&item.payload) {
                                    topo.legacy_completions.push(LegacyCompletionEvidence {
                                        agent_path: path,
                                        op_id: marker_op.id,
                                    });
                                }
                            }
                        }
                    }
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

        // Only persist the cursor after the whole file succeeded.
        cursors.set_cursor(&cursor_key, &new_cursor)?;
        topology.push(topo);
    }

    // Post-pass: emit provider-neutral structural relationship notes from the
    // explicit Codex session metadata (parentThreadId → SubagentOf, completion
    // markers → ReconnectsTo, forkedFromId → ForkOf). Best-effort over this
    // run's ops, mirroring the Claude subagent/fork post-passes.
    let relationship_notes = emit_codex_relationship_notes_from(ops, &topology);
    for note in &relationship_notes {
        let _: bool = ops.accept_op(note)?;
        report.normalized_ops += 1;
    }

    Ok(report)
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

/// Emit Codex relationship notes over the ops already emitted into a sink.
///
/// Reads the collected ops from a [`MemoryOpSink`] (which exposes them as a
/// slice) and returns the new relationship notes to append. For sinks that do
/// not expose their op vec, returns an empty vec (linking is best-effort).
fn emit_codex_relationship_notes_from(
    ops: &mut dyn OpSink,
    topology: &[ThreadTopology],
) -> Vec<editchain_core::Op> {
    if let Some(mem) = ops
        .as_any_mut()
        .and_then(|o| o.downcast_mut::<MemoryOpSink>())
    {
        emit_codex_relationship_notes(&mem.ops, topology)
    } else {
        Vec::new()
    }
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

/// Detect a trailing partial line (bytes after the last complete line) and
/// whether it is whitespace-only, mirroring the bridge's physical line count.
fn trailing_partial(
    path: &Path,
    cursor: &crate::sink::CursorValue,
) -> Result<(bool, bool), ImportError> {
    if cursor.file_size <= cursor.byte_offset {
        return Ok((false, false));
    }
    let (tail, _hash) = read_new_bytes(path, cursor.byte_offset)?;
    let blank = tail.iter().all(u8::is_ascii_whitespace);
    Ok((true, blank))
}
