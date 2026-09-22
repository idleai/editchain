//! Full payload hydration and bounded projection preview policy.

use super::legacy_preview::{compact_import_record, compact_text_bytes};
use super::{BlobResolution, BlobResolver};
use editchain_core::{BlobRef, Op, OpId, OpKind, Payload};
use editchain_store::BlobPreviewResolution;

/// Blob access outcome for payloads decoded at open or explicitly hydrated.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct BlobHydrationStats {
    /// Blob payloads replaced with verified inline content by an explicit full
    /// hydration pass. Workspace open leaves this at zero.
    pub hydrated: usize,
    /// Blob payloads whose bounded display prefix was read for row summaries.
    pub previewed: usize,
    /// Blob payloads retained as durable references for on-demand full reads.
    pub deferred: usize,
    /// Blob refs validated against the store but preserved as refs (no inline
    /// representation exists — e.g. `FileEdit::Blob` full-result content).
    pub verified_refs: usize,
    /// Blob payloads left as refs because the blob file is absent.
    pub missing: usize,
    /// Blob payloads left as refs because length or BLAKE3 validation failed.
    pub corrupt: usize,
    /// Blob refs not addressable by the durable store (local/truncated ids).
    pub unresolved: usize,
}

/// Hydrate every blob payload across a chain's decoded ops in place.
///
/// Only payloads whose declared length and BLAKE3 hash verify are replaced
/// with inline content; every other blob stays a [`Payload::Blob`] reference
/// and is counted in the returned stats. Blob references with no inline
/// representation (`FileEdit::Blob`) are validated and preserved unchanged.
/// This fixture exercises the per-kind hydration used by full detail reads;
/// display and search select their required fields before reading payloads.
#[must_use]
#[cfg(test)]
pub(super) fn hydrate_blob_payloads(ops: &mut [Op], resolver: &BlobResolver) -> BlobHydrationStats {
    let mut stats = BlobHydrationStats::default();
    for op in ops {
        hydrate_kind(&mut op.kind, resolver, &mut stats);
    }
    stats
}

/// Hydrate the payload-bearing fields of one operation kind.
pub(super) fn hydrate_kind(
    kind: &mut OpKind,
    resolver: &BlobResolver,
    stats: &mut BlobHydrationStats,
) {
    match kind {
        OpKind::ChainStart(_) => {}
        OpKind::Actor(actor) => {
            hydrate_payload(&mut actor.label, resolver, stats);
            hydrate_payload(&mut actor.role, resolver, stats);
        }
        OpKind::Message(message) => {
            hydrate_payload(&mut message.content, resolver, stats);
            hydrate_payload(&mut message.content_type, resolver, stats);
        }
        OpKind::Tool(tool) => {
            hydrate_payload(&mut tool.tool_call_id, resolver, stats);
            hydrate_payload(&mut tool.tool_name, resolver, stats);
            hydrate_payload(&mut tool.content, resolver, stats);
        }
        OpKind::Command(command) => {
            hydrate_payload(&mut command.command_id, resolver, stats);
            hydrate_payload(&mut command.content, resolver, stats);
        }
        OpKind::File(file) => {
            // `base`/`after` are `ContentId` refs with no inline slot, so they
            // stay refs. `FileEdit::Blob` is validated against the store but
            // preserved: it is the full *result* content, and the old-file
            // length a faithful `ReplaceBytes` range would need is not stored,
            // so no replacement range is synthesized.
            match &mut file.edit {
                editchain_core::op::FileEdit::None => {}
                editchain_core::op::FileEdit::ReplaceBytes { bytes, .. } => {
                    hydrate_payload(bytes, resolver, stats);
                }
                editchain_core::op::FileEdit::UnifiedDiff(payload) => {
                    hydrate_payload(payload, resolver, stats);
                }
                editchain_core::op::FileEdit::Blob(blob_ref) => {
                    count_blob_ref(blob_ref, resolver, stats);
                }
            }
        }
        OpKind::Reflection(reflection) => {
            hydrate_payload(&mut reflection.summary, resolver, stats);
            hydrate_payload(&mut reflection.anchors, resolver, stats);
        }
        OpKind::Import(import) => {
            hydrate_payload(&mut import.raw_ref, resolver, stats);
        }
        OpKind::Note(note) => {
            hydrate_payload(&mut note.content, resolver, stats);
        }
        OpKind::Error(error) => {
            hydrate_payload(&mut error.code, resolver, stats);
            hydrate_payload(&mut error.message, resolver, stats);
        }
        OpKind::GitCommit(commit) => {
            hydrate_signature(&mut commit.author, resolver, stats);
            hydrate_signature(&mut commit.committer, resolver, stats);
            hydrate_payload(&mut commit.message, resolver, stats);
            for reference in &mut commit.imported_refs {
                hydrate_payload(reference, resolver, stats);
            }
            for reference in &mut commit.live_refs {
                hydrate_payload(reference, resolver, stats);
            }
        }
        OpKind::GitLink(link) => {
            if let editchain_core::GitLinkKind::Custom(payload) = &mut link.kind {
                hydrate_payload(payload, resolver, stats);
            }
        }
        OpKind::Unknown(unknown) => {
            hydrate_payload(&mut unknown.raw_bytes, resolver, stats);
        }
    }
}

/// Hydrate the payload fields of a git signature (name/email).
fn hydrate_signature(
    signature: &mut editchain_core::GitSignature,
    resolver: &BlobResolver,
    stats: &mut BlobHydrationStats,
) {
    hydrate_payload(&mut signature.name, resolver, stats);
    hydrate_payload(&mut signature.email, resolver, stats);
}

/// Hydrate one payload in place, replacing a verified blob with inline bytes.
fn hydrate_payload(payload: &mut Payload, resolver: &BlobResolver, stats: &mut BlobHydrationStats) {
    let Payload::Blob(blob_ref) = payload else {
        return;
    };
    match resolver.resolve(blob_ref) {
        BlobResolution::Found(bytes) => {
            *payload = Payload::Inline(bytes);
            stats.hydrated = stats.hydrated.saturating_add(1);
        }
        BlobResolution::Missing => stats.missing = stats.missing.saturating_add(1),
        BlobResolution::Corrupt => stats.corrupt = stats.corrupt.saturating_add(1),
        BlobResolution::Unresolvable => stats.unresolved = stats.unresolved.saturating_add(1),
    }
}

/// Validate a blob reference that has no inline representation.
///
/// The reference is left untouched; the outcome is counted so diagnostics
/// still distinguish verified, missing, corrupt, and unresolvable blobs.
fn count_blob_ref(blob_ref: &BlobRef, resolver: &BlobResolver, stats: &mut BlobHydrationStats) {
    match resolver.resolve(blob_ref) {
        BlobResolution::Found(_) => stats.verified_refs = stats.verified_refs.saturating_add(1),
        BlobResolution::Missing => stats.missing = stats.missing.saturating_add(1),
        BlobResolution::Corrupt => stats.corrupt = stats.corrupt.saturating_add(1),
        BlobResolution::Unresolvable => stats.unresolved = stats.unresolved.saturating_add(1),
    }
}

/// Maximum bytes read from a durable payload while preparing graph rows.
pub(super) const DISPLAY_PREVIEW_READ_LIMIT: usize = 4096;

/// Build a payload-bounded operation corpus for projection and row summaries.
///
/// The source operations retain their original inline bytes/blob references.
/// Projection clones contain at most a short text preview per payload, so the
/// projection's necessary topology/collapse clones cannot multiply hundreds of
/// megabytes of tool output.
#[must_use]
pub(super) fn projection_ops_with_previews(
    source_ops: &[Op],
    resolver: &BlobResolver,
) -> (Vec<Op>, BlobHydrationStats, std::collections::HashSet<OpId>) {
    let preview = prepare_previews(source_ops, resolver);
    (preview.ops, preview.content.stats, preview.incomplete)
}

/// Inspect references without cloning payloads, opening blobs or presenting rows.
pub(super) fn uses_blob_preview(kind: &OpKind) -> bool {
    let blob = |payload: &Payload| matches!(payload, Payload::Blob(_));
    match kind {
        OpKind::ChainStart(_) => false,
        OpKind::Actor(value) => blob(&value.label) || blob(&value.role),
        OpKind::Message(value) => blob(&value.content) || blob(&value.content_type),
        OpKind::Tool(value) => {
            blob(&value.tool_call_id) || blob(&value.tool_name) || blob(&value.content)
        }
        OpKind::Command(value) => blob(&value.command_id) || blob(&value.content),
        OpKind::File(value) => match &value.edit {
            editchain_core::FileEdit::None => false,
            editchain_core::FileEdit::ReplaceBytes { bytes, .. }
            | editchain_core::FileEdit::UnifiedDiff(bytes) => blob(bytes),
            editchain_core::FileEdit::Blob(_) => true,
        },
        OpKind::Reflection(value) => blob(&value.summary) || blob(&value.anchors),
        OpKind::Import(value) => blob(&value.raw_ref),
        OpKind::Note(value) => blob(&value.content),
        OpKind::Error(value) => blob(&value.code) || blob(&value.message),
        OpKind::GitCommit(value) => {
            blob(&value.author.name)
                || blob(&value.author.email)
                || blob(&value.committer.name)
                || blob(&value.committer.email)
                || blob(&value.message)
                || value.imported_refs.iter().chain(&value.live_refs).any(blob)
        }
        OpKind::GitLink(value) => {
            matches!(&value.kind, editchain_core::GitLinkKind::Custom(payload) if blob(payload))
        }
        OpKind::Unknown(value) => blob(&value.raw_bytes),
    }
}

pub(super) struct PreviewOps {
    pub(super) ops: Vec<Op>,
    pub(super) incomplete: std::collections::HashSet<OpId>,
    pub(super) content: PreviewContent,
}

#[derive(Default)]
pub(super) struct PreviewContent {
    pub(super) stats: BlobHydrationStats,
    pub(super) pending: Vec<BlobRef>,
}

/// Keep the exact unresolved display dependencies so a live row can recover
/// when content arrives without another operation being appended.
pub(super) fn prepare_previews(source_ops: &[Op], resolver: &BlobResolver) -> PreviewOps {
    let mut content = PreviewContent::default();
    let mut incomplete = std::collections::HashSet::new();
    let ops = source_ops
        .iter()
        .map(|source| {
            let mut op = source.clone();
            if editchain_project::human::work_record(source).is_none() {
                compact_kind_for_projection(&mut op.kind, resolver, &mut content);
            }
            if op.kind != source.kind {
                let _: bool = incomplete.insert(op.id);
            }
            op
        })
        .collect();
    PreviewOps {
        ops,
        incomplete,
        content,
    }
}

impl PreviewContent {
    fn read(&mut self, blob: &BlobRef, resolver: &BlobResolver, limit: usize) -> Option<Vec<u8>> {
        self.stats.deferred = self.stats.deferred.saturating_add(1);
        match resolver.preview(blob, limit) {
            BlobPreviewResolution::Found(bytes) => return Some(bytes),
            BlobPreviewResolution::Missing => {
                self.stats.missing = self.stats.missing.saturating_add(1);
            }
            BlobPreviewResolution::Corrupt => {
                self.stats.corrupt = self.stats.corrupt.saturating_add(1);
            }
            BlobPreviewResolution::Unresolvable => {
                self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                return None;
            }
        }
        self.pending.push(*blob);
        None
    }
}

/// Replace payloads in one projected operation with bounded display previews.
fn compact_kind_for_projection(
    kind: &mut OpKind,
    resolver: &BlobResolver,
    stats: &mut PreviewContent,
) {
    match kind {
        OpKind::ChainStart(start) => compact_inline_bytes(&mut start.name),
        OpKind::Actor(actor) => {
            compact_payload(&mut actor.label, resolver, stats);
            compact_payload(&mut actor.role, resolver, stats);
        }
        OpKind::Message(message) => {
            compact_payload(&mut message.content, resolver, stats);
            compact_payload(&mut message.content_type, resolver, stats);
        }
        OpKind::Tool(tool) => {
            compact_payload(&mut tool.tool_call_id, resolver, stats);
            compact_payload(&mut tool.tool_name, resolver, stats);
            compact_payload(&mut tool.content, resolver, stats);
        }
        OpKind::Command(command) => {
            compact_payload(&mut command.command_id, resolver, stats);
            compact_payload(&mut command.content, resolver, stats);
        }
        OpKind::File(file) => match &mut file.edit {
            editchain_core::op::FileEdit::None => {}
            editchain_core::op::FileEdit::ReplaceBytes { bytes, .. }
            | editchain_core::op::FileEdit::UnifiedDiff(bytes) => {
                compact_payload(bytes, resolver, stats);
            }
            editchain_core::op::FileEdit::Blob(blob_ref) => {
                defer_blob_ref(blob_ref, resolver, stats);
            }
        },
        OpKind::Reflection(reflection) => {
            compact_payload(&mut reflection.summary, resolver, stats);
            compact_payload(&mut reflection.anchors, resolver, stats);
        }
        OpKind::Import(import) => {
            compact_import_payload(&mut import.raw_ref, resolver, stats);
        }
        OpKind::Note(note) => compact_payload(&mut note.content, resolver, stats),
        OpKind::Error(error) => {
            compact_payload(&mut error.code, resolver, stats);
            compact_payload(&mut error.message, resolver, stats);
        }
        OpKind::GitCommit(commit) => {
            compact_signature(&mut commit.author, resolver, stats);
            compact_signature(&mut commit.committer, resolver, stats);
            compact_payload(&mut commit.message, resolver, stats);
            for reference in &mut commit.imported_refs {
                compact_payload(reference, resolver, stats);
            }
            for reference in &mut commit.live_refs {
                compact_payload(reference, resolver, stats);
            }
        }
        OpKind::GitLink(link) => {
            if let editchain_core::GitLinkKind::Custom(payload) = &mut link.kind {
                compact_payload(payload, resolver, stats);
            }
        }
        OpKind::Unknown(unknown) => compact_payload(&mut unknown.raw_bytes, resolver, stats),
    }
}

/// Compact both text fields of a projected Git signature.
fn compact_signature(
    signature: &mut editchain_core::GitSignature,
    resolver: &BlobResolver,
    stats: &mut PreviewContent,
) {
    compact_payload(&mut signature.name, resolver, stats);
    compact_payload(&mut signature.email, resolver, stats);
}

/// Materialize at most a prefix of one payload for projection.
fn compact_payload(payload: &mut Payload, resolver: &BlobResolver, stats: &mut PreviewContent) {
    match payload {
        Payload::Inline(bytes) => compact_inline_bytes(bytes),
        Payload::Blob(blob_ref) => {
            if let Some(mut bytes) = stats.read(blob_ref, resolver, DISPLAY_PREVIEW_READ_LIMIT) {
                compact_inline_bytes(&mut bytes);
                *payload = Payload::Inline(bytes);
                stats.stats.previewed = stats.stats.previewed.saturating_add(1);
            }
        }
        Payload::Empty => {}
    }
}

/// Preserve a compact, parseable import discriminator instead of raw JSONL.
fn compact_import_payload(
    payload: &mut Payload,
    resolver: &BlobResolver,
    stats: &mut PreviewContent,
) {
    let preview = match payload {
        Payload::Inline(bytes) => Some(bytes.clone()),
        Payload::Blob(blob_ref) => {
            let bytes = stats.read(blob_ref, resolver, DISPLAY_PREVIEW_READ_LIMIT);
            if bytes.is_some() {
                stats.stats.previewed = stats.stats.previewed.saturating_add(1);
            }
            bytes
        }
        Payload::Empty => None,
    };
    if let Some(bytes) = preview {
        *payload = Payload::Inline(compact_import_record(&bytes));
    }
}

/// Bound one inline byte vector as UTF-8 display text.
fn compact_inline_bytes(bytes: &mut Vec<u8>) {
    *bytes = compact_text_bytes(bytes);
}

/// Record a blob-backed field whose operation shape has no inline payload slot.
fn defer_blob_ref(blob_ref: &BlobRef, resolver: &BlobResolver, stats: &mut PreviewContent) {
    drop(stats.read(blob_ref, resolver, 0));
}
