//! Main import orchestrator — ties together discovery, reading, and normalization.

use crate::claude_code::discover::discover_sessions;
use crate::claude_code::envelope::parse_envelope;
use crate::claude_code::materialize::{raw_record, record_outputs, CONTRACT};
use crate::claude_code::topology::{
    occurrence_fingerprint_fact, relation_facts_for_envelope, spawn_fact,
    CLAUDE_NORMALIZATION_VERSION,
};
use crate::cursor::resolve_source_cursor;
use crate::error::ImportError;
use crate::ids::{derive_session_id, SourcePosition, SourceStream};
use crate::model::{DiscoveryRequest, ImportOptions, ImportReport};
use crate::sink::{
    emit_op, BlobSink, CursorStore, EmissionKind, MaterializationCheckpoint, OpSink,
};
use crate::source_read::{SourceReadPlan, SourceReadState};

/// Version that first emitted the complete provider topology. Version-2
/// sources need only the collision-free payload-fingerprint supplement when
/// upgrading; older sources require a complete topology replay.
const CLAUDE_PROVIDER_TOPOLOGY_VERSION: u32 = 2;

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
pub fn import_claude_code(
    request: &DiscoveryRequest,
    options: &ImportOptions,
    ops: &mut dyn OpSink,
    blobs: &mut dyn BlobSink,
    cursors: &mut dyn CursorStore,
) -> Result<ImportReport, ImportError> {
    options.cancellation.check(&request.sessions_dir)?;
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
        let existing_cursor = resolved.cursor;
        let existing_normalization_version = existing_cursor
            .as_ref()
            .map(|cursor| cursor.normalization_version);
        let needs_topology_upgrade = options.normalize
            && existing_normalization_version
                .is_some_and(|version| version < CLAUDE_NORMALIZATION_VERSION);
        let needs_full_topology_replay = needs_topology_upgrade
            && existing_normalization_version
                .is_some_and(|version| version < CLAUDE_PROVIDER_TOPOLOGY_VERSION);
        let needs_fingerprint_replay = needs_topology_upgrade
            && existing_normalization_version == Some(CLAUDE_PROVIDER_TOPOLOGY_VERSION);
        let needs_cursor_upgrade = migrates_legacy_key
            || existing_cursor.as_ref().is_some_and(|cursor| {
                cursor.source_node != Some(source_node)
                    || cursor.content_hash_version < 1
                    || cursor.accepted_generation.is_none()
            });

        let plan = SourceReadPlan::capture_controlled(
            &session.path,
            existing_cursor.as_ref(),
            cursors.get_generation(&state_key)?,
            cursors.get_reservation(&cursor_key)?.as_ref(),
            &options.source_control(),
        )?;
        let needs_materialization_replay = options.normalize
            && MaterializationCheckpoint::needs_replay(
                plan.checkpoint().materialization.as_ref(),
                CONTRACT,
                options.include_thinking,
                plan.start_seq(),
            )?;
        if plan.state() == SourceReadState::Unchanged
            && !needs_topology_upgrade
            && !needs_cursor_upgrade
            && !needs_materialization_replay
        {
            continue;
        }
        let boot = plan.generation();
        let start_seq = plan.start_seq();
        let lines = plan.lines();
        let mut new_cursor = plan.checkpoint().clone();
        // Historical upgrades use the same captured source and emit only
        // relationship evidence. A rewrite starts a fresh generation instead.
        let topology_replay =
            if needs_topology_upgrade && plan.state() != SourceReadState::Rewritten {
                Some(plan.all_lines()?)
            } else {
                None
            };

        report.files_processed += 1;

        // The cursor carries the source node explicitly so archive/live-root
        // relocation never changes existing operation IDs.
        let stream = SourceStream::new(source_node, boot);

        let replay_lines = (needs_materialization_replay && start_seq > 0)
            .then(|| plan.all_lines())
            .transpose()?;
        let content_lines = replay_lines.as_deref().unwrap_or(lines);
        let content_start = if replay_lines.is_some() { 0 } else { start_seq };
        for (index, line) in content_lines.iter().enumerate() {
            options.cancellation.check(&session.path)?;
            let seq = content_start
                .checked_add(u64::try_from(index).map_err(std::io::Error::other)?)
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or_else(|| {
                    ImportError::CursorStore("Claude record ordinal exhausted".into())
                })?;
            let envelope = parse_envelope(&line.data);
            let raw = raw_record(
                envelope.as_ref(),
                line,
                stream.op_from_position(SourcePosition::raw(seq))?,
                &session.session_id,
                blobs,
            )?;
            let derived = if options.normalize {
                record_outputs(
                    envelope.as_ref(),
                    &raw,
                    line.hash,
                    options.include_thinking,
                    blobs,
                )?
            } else {
                Vec::new()
            };
            if seq > start_seq {
                emit_op(&raw, ops, &mut report, EmissionKind::Raw)?;
                report.malformed = report
                    .malformed
                    .saturating_add(usize::from(envelope.is_none()));
            }
            for op in &derived {
                emit_op(op, ops, &mut report, EmissionKind::Derived)?;
            }
            if seq > start_seq
                && options.normalize
                && (!needs_full_topology_replay || topology_replay.is_none())
            {
                if let Some(envelope) = &envelope {
                    for fact in relation_facts_for_envelope(
                        envelope,
                        &line.data,
                        &stream,
                        seq,
                        &session.session_id,
                    )? {
                        emit_op(&fact, ops, &mut report, EmissionKind::Derived)?;
                    }
                }
            }
        }

        // Version upgrade: rebuild topology from complete durable source
        // evidence, including records whose raw payloads spilled to blobs. This
        // intentionally emits no historical raw/content ops and is independent
        // of the output sink's concrete type.
        if options.normalize {
            if let Some(all_lines) = topology_replay.as_ref() {
                let replay_limit = if needs_fingerprint_replay {
                    let historical_count =
                        usize::try_from(start_seq).map_or(all_lines.len(), |count| count);
                    all_lines.len().min(historical_count)
                } else {
                    all_lines.len()
                };
                for (i, line) in all_lines.iter().take(replay_limit).enumerate() {
                    options.cancellation.check(&session.path)?;
                    let Some(envelope) = parse_envelope(&line.data) else {
                        continue;
                    };
                    let seq = i as u64 + 1;
                    if needs_fingerprint_replay {
                        if let Some(fact) = occurrence_fingerprint_fact(
                            &envelope,
                            &line.data,
                            &stream,
                            seq,
                            &session.session_id,
                        )? {
                            emit_op(&fact, ops, &mut report, EmissionKind::Derived)?;
                        }
                    } else {
                        for fact in relation_facts_for_envelope(
                            &envelope,
                            &line.data,
                            &stream,
                            seq,
                            &session.session_id,
                        )? {
                            emit_op(&fact, ops, &mut report, EmissionKind::Derived)?;
                        }
                    }
                }
            }

            // The sidecar's toolUseId is an exact spawn endpoint. Emit it once
            // for a fresh generation or full topology upgrade; unresolved parent
            // tool entities stay unresolved in projection instead of falling
            // back to actor, time, or content matching.
            if (start_seq == 0 || needs_full_topology_replay)
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
                    emit_op(&fact, ops, &mut report, EmissionKind::Derived)?;
                }
            }

            new_cursor.materialization = Some(MaterializationCheckpoint {
                contract: CONTRACT.to_owned(),
                through: new_cursor.ops_emitted,
                includes_thinking: options.include_thinking,
            });
            new_cursor.normalization_version = new_cursor
                .normalization_version
                .max(CLAUDE_NORMALIZATION_VERSION);
        }
        if !options.normalize && new_cursor.ops_emitted > start_seq {
            // The metadata version no longer covers the full accepted raw
            // prefix. Replaying exact facts fills this gap on normalization.
            new_cursor.normalization_version = 0;
        }
        new_cursor.source_node = Some(source_node);
        new_cursor.content_hash_version = 1;
        options.cancellation.check(&session.path)?;
        if boot > 0 {
            cursors.set_generation(&cursor_key, boot)?;
        }

        // Persist cursor after successful processing.
        cursors.set_cursor(&cursor_key, &new_cursor)?;
    }

    Ok(report)
}
