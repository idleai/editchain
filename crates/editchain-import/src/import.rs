//! Main import orchestrator — ties together discovery, reading, and normalization.

use editchain_core::Op;

use crate::claude_code::discover::{discover_sessions, SessionFile};
use crate::claude_code::envelope::parse_envelope;
use crate::claude_code::normalize::{normalize_envelope, NormalizeOptions};
use crate::claude_code::reader::read_session_file;
use crate::cursor::check_file_generation;
use crate::error::ImportError;
use crate::fork::emit_fork_notes;
use crate::ids::{derive_source_stream, SourcePosition};
use crate::model::{DiscoveryRequest, ImportOptions, ImportReport};
use crate::sink::{BlobSink, CursorStore, MemoryOpSink, OpSink};
use crate::subagent::{emit_subagent_notes_from, SubagentMeta};

/// Import all Claude Code sessions from a directory into editchain operations.
///
/// This is the main entry point for the import pipeline.
///
/// # Errors
///
/// Returns `ImportError` if session discovery, reading, or normalization fails.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "counter increments are bounded by file/op counts"
)]
#[expect(
    clippy::as_conversions,
    reason = "usize to u64 is safe on all supported platforms"
)]
#[expect(
    clippy::let_underscore_untyped,
    reason = "Result return values are intentionally discarded for side effects"
)]
pub fn import_claude_code(
    request: &DiscoveryRequest,
    options: &ImportOptions,
    ops: &mut dyn OpSink,
    blobs: &mut dyn BlobSink,
    cursors: &mut dyn CursorStore,
) -> Result<ImportReport, ImportError> {
    let mut report = ImportReport::new();

    // Discover session files.
    let sessions = discover_sessions(&request.sessions_dir).map_err(ImportError::OpSink)?;
    report.files_discovered = sessions.len();

    // Collect subagent metadata for post-import branch/reconnect linking.
    let subagent_meta: Vec<SubagentMeta> = sessions
        .iter()
        .filter(|s| s.is_subagent)
        .filter_map(subagent_meta_from)
        .collect();

    let workspace_str = request.workspace_path.to_str().unwrap_or("/workspace");

    for session in &sessions {
        // Check cursor for idempotency.
        let cursor_key = session.path.to_string_lossy().to_string();
        let existing_cursor = cursors.get_cursor(&cursor_key)?;

        // Determine boot generation — increment if file was rewritten.
        let boot = if let Some(ref cursor) = existing_cursor {
            match check_file_generation(&session.path, cursor) {
                Ok(true) => {
                    // File unchanged — skip entirely.
                    continue;
                }
                Ok(false) => {
                    // File grew — same boot, read only new bytes.
                    0
                }
                Err(ImportError::SourceGenerationChanged { .. }) => {
                    // File was rewritten — new boot generation.
                    1
                }
                Err(e) => return Err(e),
            }
        } else {
            0
        };

        report.files_processed += 1;

        // Derive a deterministic source stream per session file.
        let stream = derive_source_stream(workspace_str, &cursor_key, boot);

        // Read session file (from cursor offset if appending).
        let (lines, _bytes_read, new_cursor) =
            read_session_file(&session.path, existing_cursor.as_ref())?;

        let norm_opts = NormalizeOptions {
            normalize: options.normalize,
            include_thinking: options.include_thinking,
        };

        // Use per-file sequence numbering starting from cursor's last ordinal.
        let start_seq = existing_cursor.as_ref().map_or(0, |c| c.ops_emitted);
        // Chain raw import ops into a single linear chain per session file: each
        // line's raw op parents to the previous line's raw op. This makes a
        // session read as one continuous chain rather than N disconnected roots.
        let mut prev_raw_id: Option<editchain_core::OpId> = None;
        for (i, line) in lines.iter().enumerate() {
            let seq = start_seq + i as u64 + 1;

            // Parse envelope for normalization.
            let env = parse_envelope(&line.data);

            if let Some(ref envelope) = env {
                let (mut raw_op, normalized_ops) = normalize_envelope(
                    envelope,
                    line.hash,
                    &line.data,
                    &stream,
                    seq,
                    &norm_opts,
                    blobs,
                    &session.session_id,
                );

                // Chain this raw op to the previous line's raw op.
                if let Some(prev) = prev_raw_id {
                    raw_op.parents = editchain_core::parents::ParentSet::One(prev);
                }

                // Emit raw import op.
                let _: bool = ops.accept_op(&raw_op)?;
                report.raw_ops += 1;
                prev_raw_id = Some(raw_op.id);

                // Emit normalized ops.
                for norm_op in &normalized_ops {
                    let _ = ops.accept_op(norm_op)?;
                    report.normalized_ops += 1;
                }
            } else {
                // Unparseable line — still emit as raw ImportOp, chained to the
                // previous line's raw op using the same ID scheme (seq << 16).
                let op_id = stream.op_from_position(SourcePosition::raw(seq))?;
                let mut raw_op = Op {
                    id: op_id,
                    parents: editchain_core::parents::ParentSet::None,
                    actor: editchain_core::ActorId(0),
                    clock: editchain_core::clock::Clock::None,
                    scope: editchain_core::scope::ScopeRef::None,
                    tags: editchain_core::tags::Tags::IMPORT | editchain_core::tags::Tags::ERROR,
                    kind: editchain_core::op::OpKind::Import(editchain_core::op::ImportOp {
                        raw_ref: editchain_core::payload::Payload::Inline(line.data.clone()),
                        raw_hash: Some(line.hash),
                    }),
                };
                if let Some(prev) = prev_raw_id {
                    raw_op.parents = editchain_core::parents::ParentSet::One(prev);
                }
                let _: bool = ops.accept_op(&raw_op)?;
                report.raw_ops += 1;
                report.malformed += 1;
                prev_raw_id = Some(op_id);
            }
        }

        // Persist cursor after successful processing.
        cursors.set_cursor(&cursor_key, &new_cursor)?;
    }

    // Post-pass: emit subagent branch/reconnect relationship notes. These are
    // new ops appended to the sink (not mutations of existing ops), so they work
    // for any sink that can accept ops — not just in-memory ones. They must run
    // after all sessions are imported because they need cross-session view.
    let subagent_notes = emit_subagent_notes(ops, &subagent_meta);
    for note in &subagent_notes {
        let _: bool = ops.accept_op(note)?;
        report.normalized_ops += 1;
    }

    // Post-pass: emit ForkOf relationship notes linking sessions that are forks
    // of one original session (shared parentUuid chain) so they render as a fork
    // rather than duplicated chains.
    let fork_notes = emit_fork_notes_from(ops);
    for note in &fork_notes {
        let _: bool = ops.accept_op(note)?;
        report.normalized_ops += 1;
    }

    Ok(report)
}

/// Convert a discovered subagent `SessionFile` into linking metadata.
fn subagent_meta_from(session: &SessionFile) -> Option<SubagentMeta> {
    let tool_use_id = session.tool_use_id.clone()?;
    let parent_session_id = session.parent_session_id.clone()?;
    Some(SubagentMeta {
        subagent_session_id: session.session_id.clone(),
        parent_session_id,
        tool_use_id,
    })
}

/// Emit subagent branch/reconnect relationship notes over the ops already
/// emitted into a sink.
///
/// Reads the collected ops from a [`MemoryOpSink`] (which exposes them as a
/// slice) and returns the new relationship notes to append. For sinks that do
/// not expose their op vec, returns an empty vec (linking is best-effort).
fn emit_subagent_notes(ops: &mut dyn OpSink, meta: &[SubagentMeta]) -> Vec<Op> {
    if let Some(mem) = ops
        .as_any_mut()
        .and_then(|o| o.downcast_mut::<MemoryOpSink>())
    {
        emit_subagent_notes_from(&mem.ops, meta)
    } else {
        Vec::new()
    }
}

/// Emit `ForkOf` relationship notes over the ops already emitted into a sink.
///
/// Reads the collected ops from a [`MemoryOpSink`] and returns the new notes to
/// append. For sinks that do not expose their op vec, returns an empty vec.
fn emit_fork_notes_from(ops: &mut dyn OpSink) -> Vec<Op> {
    if let Some(mem) = ops
        .as_any_mut()
        .and_then(|o| o.downcast_mut::<MemoryOpSink>())
    {
        emit_fork_notes(&mem.ops)
    } else {
        Vec::new()
    }
}
