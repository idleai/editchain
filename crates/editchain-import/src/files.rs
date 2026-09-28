//! Bounded per-file capture without reopening the destination writer.

use std::{io, path::Path};

use crate::{
    batch::ImportBatch, claude_code::discover::SessionFile, BlobSink, CursorStore, ImportError,
    ImportOptions, ImportSource,
};

/// One discovered source with the provider metadata needed for exact capture.
///
/// Keep the same source root for discovery and capture. In particular, selecting
/// a nested Claude source retains its parent session and spawn sidecar metadata.
#[derive(Debug, Clone)]
pub struct ImportFile(FileKind);

#[derive(Debug, Clone)]
enum FileKind {
    Claude(SessionFile),
    Codex(std::path::PathBuf),
    Human(std::path::PathBuf),
}

impl ImportFile {
    /// Source path beneath the discovery request's stable cursor root.
    #[must_use]
    pub fn path(&self) -> &Path {
        match &self.0 {
            FileKind::Claude(session) => &session.path,
            FileKind::Codex(path) | FileKind::Human(path) => path,
        }
    }
}

/// Discover supported files in deterministic order without capturing operations.
///
/// Callers may filter this list, then capture and persist one file at a time
/// through one retained durable writer. The provider's normal discovery rules,
/// explicit Codex rollout selection, and Claude sidecar metadata are preserved.
///
/// # Errors
/// Returns cancellation, discovery, or invalid selected-path errors.
pub fn discover_import_files(
    source: ImportSource<'_>,
    options: &ImportOptions,
) -> Result<Vec<ImportFile>, ImportError> {
    match source {
        ImportSource::Claude(request) => {
            options.cancellation.check(&request.sessions_dir)?;
            Ok(
                crate::claude_code::discover::discover_sessions(&request.sessions_dir)
                    .map_err(ImportError::OpSink)?
                    .into_iter()
                    .map(|session| ImportFile(FileKind::Claude(session)))
                    .collect(),
            )
        }
        ImportSource::Codex { request, .. } => {
            options.cancellation.check(&request.raw_root)?;
            let rollouts = if request.selected_paths.is_empty() {
                crate::codex::discover::discover_rollouts(&request.raw_root)
            } else {
                crate::codex::discover::selected_rollouts(
                    &request.raw_root,
                    &request.selected_paths,
                )
            }
            .map_err(ImportError::OpSink)?;
            Ok(rollouts
                .into_iter()
                .map(|file| ImportFile(FileKind::Codex(file.path)))
                .collect())
        }
        ImportSource::Human(request) => Ok(crate::human::archives(
            &request.source,
            &options.cancellation,
        )?
        .into_iter()
        .map(|path| ImportFile(FileKind::Human(path)))
        .collect()),
    }
}

/// Capture one previously discovered file with uncommitted source checkpoints.
///
/// The ordinary source, helper, and capture-batch limits still apply to each
/// file. Flush buffered blobs before persisting the returned batch, and retain
/// the same durable writer across files to reuse its admission state. A failure
/// leaves this file's checkpoints uncommitted; prior files may be durable.
///
/// # Errors
/// Returns provider mismatch, capture, helper, cancellation, or resource errors.
pub fn capture_import_file(
    source: ImportSource<'_>,
    file: &ImportFile,
    options: &ImportOptions,
    blobs: &mut dyn BlobSink,
    cursors: &dyn CursorStore,
) -> Result<ImportBatch, ImportError> {
    options.cancellation.check(file.path())?;
    ImportBatch::capture_bounded(cursors, options.batch_limits, |ops, pending| {
        match (source, &file.0) {
            (ImportSource::Claude(request), FileKind::Claude(session)) => {
                crate::import::import_claude_sessions(
                    (request, std::slice::from_ref(session)),
                    options,
                    ops,
                    blobs,
                    pending,
                )
            }
            (ImportSource::Codex { request, helper }, FileKind::Codex(path)) => {
                let relative = path
                    .strip_prefix(&request.raw_root)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                let selected = crate::codex::CodexDiscoveryRequest {
                    workspace_path: request.workspace_path.clone(),
                    raw_root: request.raw_root.clone(),
                    selected_paths: vec![relative.to_owned()],
                    repositories: request.repositories,
                };
                crate::codex::import_codex(&selected, options, helper, ops, blobs, pending)
            }
            (ImportSource::Human(request), FileKind::Human(path)) => {
                crate::human::import_human_files(
                    (request, std::slice::from_ref(path)),
                    options,
                    ops,
                    blobs,
                    pending,
                )
            }
            (ImportSource::Claude(_), FileKind::Codex(_) | FileKind::Human(_))
            | (ImportSource::Human(_), FileKind::Claude(_) | FileKind::Codex(_))
            | (ImportSource::Codex { .. }, FileKind::Claude(_) | FileKind::Human(_)) => {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "source and discovered file providers differ",
                )
                .into())
            }
        }
    })
}
