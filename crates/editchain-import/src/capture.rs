//! Provider selection with private checkpoints and explicit durable acceptance.

use crate::batch::ImportBatch;
use crate::codex::{CodexDiscoveryRequest, HelperCommand};
use crate::human::HumanImportRequest;
use crate::{BlobSink, CursorStore, DiscoveryRequest, ImportError, ImportOptions};

/// A caller-selected source; no viewer or workspace service is required.
#[derive(Debug, Clone, Copy)]
pub enum ImportSource<'a> {
    /// Claude transcripts and their subagent sidecars.
    Claude(&'a DiscoveryRequest),
    /// Codex rollouts interpreted by the existing configurable exporter.
    Codex {
        /// Discovery root and host-supplied repository identities.
        request: &'a CodexDiscoveryRequest<'a>,
        /// Versioned projection bridge, retained in its current location until f10.
        helper: &'a HelperCommand,
    },
    /// Recorded human archive envelopes, independent of editor host APIs.
    Human(&'a HumanImportRequest),
}

/// Capture history without advancing the caller's accepted cursors.
///
/// The returned batch is inspectable and can be discarded for a preview. Call
/// [`ImportBatch::persist`] to reserve source identities, durably admit every
/// operation variant, then commit the corresponding cursors. Use a durable
/// blob sink when persisting: referenced bytes are retained during capture.
/// Retry a failed persistence with the same source and cursor store; exact
/// variants collapse and conflicting variants remain evidence.
///
/// # Errors
/// Returns source, helper, resource-limit, cancellation or sink errors without
/// modifying the base cursor store. Blob writes may already have succeeded.
pub fn capture_import(
    source: ImportSource<'_>,
    options: &ImportOptions,
    blobs: &mut dyn BlobSink,
    cursors: &dyn CursorStore,
) -> Result<ImportBatch, ImportError> {
    ImportBatch::capture_bounded(cursors, options.batch_limits, |ops, pending| match source {
        ImportSource::Claude(request) => {
            crate::import::import_claude_code(request, options, ops, blobs, pending)
        }
        ImportSource::Codex { request, helper } => {
            crate::codex::import_codex(request, options, helper, ops, blobs, pending)
        }
        ImportSource::Human(request) => {
            crate::human::import_human(request, options, ops, blobs, pending)
        }
    })
}
