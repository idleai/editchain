//! Main import orchestrator — ties together discovery, reading, and normalization.

use editchain_core::Op;

use crate::claude_code::discover::discover_sessions;
use crate::claude_code::envelope::parse_envelope;
use crate::claude_code::normalize::{normalize_envelope, NormalizeOptions};
use crate::claude_code::reader::read_session_file;
use crate::cursor::check_file_generation;
use crate::error::ImportError;
use crate::ids::{derive_node_id, SourceStream};
use crate::model::{DiscoveryRequest, ImportOptions, ImportReport};
use crate::sink::{BlobSink, CursorStore, OpSink};

/// Import all Claude Code sessions from a directory into editchain operations.
///
/// This is the main entry point for the import pipeline.
pub fn import_claude_code(
    request: &DiscoveryRequest,
    options: &ImportOptions,
    ops: &mut dyn OpSink,
    _blobs: &mut dyn BlobSink,
    cursors: &mut dyn CursorStore,
) -> Result<ImportReport, ImportError> {
    let mut report = ImportReport::new();

    // Discover session files.
    let sessions = discover_sessions(&request.sessions_dir)
        .map_err(|e| ImportError::OpSink(e))?;
    report.files_discovered = sessions.len();

    let node_id = derive_node_id(request.workspace_path.to_str().unwrap_or("/workspace"));
    let mut global_seq: u64 = 0;

    for session in &sessions {
        // Check cursor for idempotency.
        let cursor_key = session.path.to_string_lossy().to_string();
        let existing_cursor = cursors.get_cursor(&cursor_key)?;

        if let Some(ref cursor) = existing_cursor {
            match check_file_generation(&session.path, cursor) {
                Ok(true) => {
                    // File unchanged — skip.
                    continue;
                }
                Ok(false) => {
                    // File grew — read only new bytes.
                }
                Err(ImportError::SourceGenerationChanged { .. }) => {
                    // File was rewritten — re-read from start.
                }
                Err(e) => return Err(e),
            }
        }

        report.files_processed += 1;

        // Read session file.
        let (lines, _bytes_read, new_cursor) =
            read_session_file(&session.path, existing_cursor.as_ref())?;

        let stream = SourceStream::new(node_id, 0);
        let norm_opts = NormalizeOptions {
            normalize: options.normalize,
            include_thinking: options.include_thinking,
        };

        for line in &lines {
            global_seq += 1;

            // Parse envelope for normalization.
            let env = parse_envelope(&line.data);

            if let Some(ref envelope) = env {
                let (raw_op, normalized_ops) = normalize_envelope(
                    envelope,
                    line.hash,
                    &line.data,
                    &stream,
                    global_seq,
                    &norm_opts,
                );

                // Emit raw import op.
                ops.accept_op(&raw_op)?;
                report.raw_ops += 1;

                // Emit normalized ops.
                for norm_op in &normalized_ops {
                    ops.accept_op(norm_op)?;
                    report.normalized_ops += 1;
                }
            } else {
                // Unparseable line — still emit as raw ImportOp.
                let op_id = stream.op_id(global_seq);
                let raw_op = Op {
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
                ops.accept_op(&raw_op)?;
                report.raw_ops += 1;
                report.malformed += 1;
            }
        }

        // Persist cursor after successful processing.
        cursors.set_cursor(&cursor_key, &new_cursor)?;
    }

    Ok(report)
}
