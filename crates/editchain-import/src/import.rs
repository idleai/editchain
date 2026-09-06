//! Main import orchestrator — ties together discovery, reading, and normalization.

use editchain_core::Op;

use crate::claude_code::discover::discover_sessions;
use crate::claude_code::envelope::parse_envelope;
use crate::claude_code::normalize::{normalize_envelope, NormalizeOptions};
use crate::claude_code::reader::read_session_file;
use crate::claude_code::topology::{
    relation_facts_for_envelope, spawn_fact, CLAUDE_NORMALIZATION_VERSION,
};
use crate::cursor::{check_file_generation, resolve_source_cursor};
use crate::error::ImportError;
use crate::ids::{derive_session_id, SourcePosition, SourceStream};
use crate::model::{DiscoveryRequest, ImportOptions, ImportReport};
use crate::sink::{BlobSink, CursorStore, OpSink};

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

    let workspace_str = request.workspace_path.to_str().unwrap_or("/workspace");

    for session in &sessions {
        // Resolve the provider-relative cursor key. If this chain predates that
        // contract, the exact absolute-path cursor is migrated while retaining
        // the node that already owns its immutable operation IDs.
        let resolved = resolve_source_cursor(
            cursors,
            "claude-code",
            &request.sessions_dir,
            &session.path,
            workspace_str,
        )?;
        let cursor_key = resolved.canonical_key;
        let state_key = resolved.state_key;
        let source_node = resolved.source_node;
        let migrates_legacy_key = cursor_key != state_key;
        let mut existing_cursor = resolved.cursor;
        let needs_topology_upgrade = options.normalize
            && existing_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.normalization_version < CLAUDE_NORMALIZATION_VERSION);
        let needs_cursor_upgrade = migrates_legacy_key
            || existing_cursor.as_ref().is_some_and(|cursor| {
                cursor.source_node != Some(source_node) || cursor.content_hash_version < 1
            });

        // Decide which source bytes need raw emission and whether a complete
        // topology replay is required. A topology replay reads every record but
        // emits only new, deterministic relationship facts; raw/semantic op IDs
        // remain untouched. Rewrites use the durable generation counter rather
        // than repeatedly colliding in a hard-coded boot 1 lane.
        let (boot, start_seq, lines, mut new_cursor, topology_replay) = if let Some(cursor) =
            existing_cursor.as_mut()
        {
            match check_file_generation(&session.path, cursor) {
                Ok(true) => {
                    if needs_topology_upgrade || needs_cursor_upgrade {
                        let topology_replay = if needs_topology_upgrade {
                            let (all_lines, _bytes_read, _replayed_cursor) =
                                read_session_file(&session.path, None)?;
                            Some(all_lines)
                        } else {
                            None
                        };
                        (
                            cursors.get_generation(&state_key)?,
                            cursor.ops_emitted,
                            Vec::new(),
                            cursor.clone(),
                            topology_replay,
                        )
                    } else {
                        // File unchanged and current — idempotent skip.
                        continue;
                    }
                }
                Ok(false) => {
                    let boot = cursors.get_generation(&state_key)?;
                    let (new_lines, _bytes_read, new_cursor) =
                        read_session_file(&session.path, Some(cursor))?;
                    let topology_replay = if needs_topology_upgrade {
                        let (all_lines, _bytes_read, _replayed_cursor) =
                            read_session_file(&session.path, None)?;
                        Some(all_lines)
                    } else {
                        None
                    };
                    (
                        boot,
                        cursor.ops_emitted,
                        new_lines,
                        new_cursor,
                        topology_replay,
                    )
                }
                Err(ImportError::SourceGenerationChanged { .. }) => {
                    let generation = cursors.get_generation(&state_key)?.saturating_add(1);
                    cursors.set_generation(&cursor_key, generation)?;
                    let (lines, _bytes_read, new_cursor) = read_session_file(&session.path, None)?;
                    (generation, 0, lines, new_cursor, None)
                }
                Err(e) => return Err(e),
            }
        } else {
            let boot = cursors.get_generation(&cursor_key)?;
            let (lines, _bytes_read, new_cursor) = read_session_file(&session.path, None)?;
            (boot, 0, lines, new_cursor, None)
        };

        report.files_processed += 1;

        // The cursor carries the source node explicitly so archive/live-root
        // relocation never changes existing operation IDs.
        let stream = SourceStream::new(source_node, boot);

        let norm_opts = NormalizeOptions {
            normalize: options.normalize,
            include_thinking: options.include_thinking,
        };

        // Chain raw import ops into a single linear chain per session file: each
        // line's raw op parents to the previous physical occurrence, including
        // across cursor boundaries. Provider parentage remains a separate typed
        // relation emitted below.
        let mut prev_raw_id = if start_seq > 0 {
            Some(stream.op_from_position(SourcePosition::raw(start_seq))?)
        } else {
            None
        };
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

                // Current-version incremental/fresh import: emit exact provider
                // facts for this batch directly from the parsed envelope. An
                // upgrade run emits the complete source's facts in the replay
                // below so old and new records use one path.
                if options.normalize && topology_replay.is_none() {
                    for fact in
                        relation_facts_for_envelope(envelope, &stream, seq, &session.session_id)?
                    {
                        let _: bool = ops.accept_op(&fact)?;
                        report.normalized_ops += 1;
                    }
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

        // Version upgrade: rebuild topology from complete durable source
        // evidence, including records whose raw payloads spilled to blobs. This
        // intentionally emits no historical raw/content ops and is independent
        // of the output sink's concrete type.
        if options.normalize {
            if let Some(all_lines) = topology_replay.as_ref() {
                for (i, line) in all_lines.iter().enumerate() {
                    let Some(envelope) = parse_envelope(&line.data) else {
                        continue;
                    };
                    let seq = i as u64 + 1;
                    for fact in
                        relation_facts_for_envelope(&envelope, &stream, seq, &session.session_id)?
                    {
                        let _: bool = ops.accept_op(&fact)?;
                        report.normalized_ops += 1;
                    }
                }
            }

            // The sidecar's toolUseId is an exact spawn endpoint. Emit it once
            // for a fresh generation or topology upgrade; unresolved parent tool
            // entities stay unresolved in projection instead of falling back to
            // actor, time, or content matching.
            if (start_seq == 0 || needs_topology_upgrade)
                && new_cursor.ops_emitted > 0
                && session.is_subagent
            {
                if let (Some(tool_use_id), Some(parent_session_id)) = (
                    session.tool_use_id.as_deref(),
                    session.parent_session_id.as_deref(),
                ) {
                    let first_raw = stream.op_from_position(SourcePosition::raw(1))?;
                    let fact = spawn_fact(
                        first_raw,
                        editchain_core::ScopeRef::Session(derive_session_id(parent_session_id)),
                        tool_use_id,
                    )?;
                    let _: bool = ops.accept_op(&fact)?;
                    report.normalized_ops += 1;
                }
            }

            new_cursor.normalization_version = new_cursor
                .normalization_version
                .max(CLAUDE_NORMALIZATION_VERSION);
        }
        new_cursor.source_node = Some(source_node);
        new_cursor.content_hash_version = 1;
        if migrates_legacy_key && boot > 0 {
            cursors.set_generation(&cursor_key, boot)?;
        }

        // Persist cursor after successful processing.
        cursors.set_cursor(&cursor_key, &new_cursor)?;
    }

    Ok(report)
}
