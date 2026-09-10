//! Shared history backend for CLI snapshot preparation and native viewer requests.
//!
//! Workspace lifetimes, source reads, projection, search, and derived snapshots
//! live here. Framed request dispatch is owned by the sibling transport adapter.

#[cfg(test)]
use editchain_import as _;
#[cfg(test)]
use tempfile as _;

mod search;
mod snapshot;

pub use search::{build_lexical_index, SearchIndexState};

pub use snapshot::RenderSnapshotReport;

use serde as _;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use editchain_core::{
    BlobRef, GitOid, Op, OpId, OpKind, Payload, RepositoryId, ScopeRef, SessionId, Tags,
};
use editchain_git::{
    commit_file_changes, open_repository, resolve_blob as resolve_git_blob, resolve_commit,
    resolve_path_at_commit, walk_history, GitFileChange, GitFileStatus, RepositoryCatalog,
    RepositoryHandle,
};
#[cfg(test)]
use editchain_index::{DocumentId, LexicalHit};
use editchain_project::activity::{SessionSummaryMarker, WorkUnitMarker};
use editchain_project::activity_view::{ActivityPresentation, ActivityView};
use editchain_project::taxonomy::{ActivityKind, ChainState, Outcome, RecordRole, Visibility};
use editchain_project::HistoryProjection;
use editchain_protocol::{
    ContentTextDto, ErrorCode, ExpansionSpanDto, FileChangeDto, FileChangeSource, FileChangeStatus,
    FileDiffDto, FileDiffHunkDto, HistoryRow, HistoryWindow, NodeDetails, ParentRelationDto,
    ParentRelationKind, ResolvedObject, RowContentDto, ServiceError, SessionMetaDto,
    SessionSummaryDto, SnapshotId, SubOpSummary, WorkUnitDto,
};

use snapshot::{RenderSnapshot, SnapshotBuilder, SnapshotIdentity, SnapshotManifestData};

/// A loaded workspace: chain ops + git repositories.
#[derive(Debug)]
pub struct Workspace {
    /// The unified history projection.
    projection: HistoryProjection,
    /// Canonical decoded operations with durable blob references preserved.
    /// Full payload bytes are materialized from this corpus only for details or
    /// the lazy search index, never for graph projection/layout.
    source_ops: Vec<Op>,
    /// Constant-time lookup into `source_ops` for detail requests.
    source_op_index: HashMap<OpId, usize>,
    /// Small session provenance labels keyed by the same `session:<id>` group
    /// strings used by projected history rows.
    session_metadata: HashMap<String, SessionMetaDto>,
    /// Imported Claude/Codex file changes keyed by the raw history row that
    /// owns their normalized operation.
    agent_file_changes: HashMap<OpId, Vec<FileChangeDto>>,
    /// Immutable first-parent Git changes keyed by repository and commit.
    git_file_changes: HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>>,
    /// Accepted operation ids and exact segment-record locations. This is
    /// persisted into render snapshots so details remain lazy on the fast path.
    source_op_locations: Vec<SnapshotOpLocator>,
    /// Read-only durable blob store used by on-demand details/search hydration.
    blob_resolver: Option<BlobResolver>,
    /// Discovered git repositories.
    repositories: RepositoryCatalog,
    /// Diagnostics for this open: chain canonicalization and bounded blob
    /// preview/deferred-hydration outcomes.
    pub diagnostics: OpenDiagnostics,
    /// Absolute workspace root used if a non-default request must lazily load
    /// the complete projection after a snapshot-backed Open.
    root_path: PathBuf,
    /// Absolute authoritative chain directory.
    chain_path: PathBuf,
    /// Inputs pinned at open; absent only for an in-memory projection.
    source_identity: Option<SnapshotIdentity>,
    /// Cached wire identity, computed once for this opened source version.
    snapshot_id: SnapshotId,
    /// Exactly one backend owns the fixed row order at a time.
    backend: WorkspaceBackend,
    /// The fixed Activity-view snapshot shared by window and find requests.
    current_view: Option<ActivityView<ExpandedChildRow>>,
}

#[derive(Debug)]
enum WorkspaceBackend {
    Cached(Box<RenderSnapshot>),
    Projected,
}

/// Parameters for one history-window read.
#[derive(Debug, Clone, Copy)]
pub struct HistoryWindowOptions {
    /// Expanded-row offset (zero is newest).
    pub offset: u64,
    /// Maximum expanded rows to return.
    pub limit: u64,
    /// Compute and attach global lane geometry before returning.
    pub include_layout: bool,
}

/// Display content supplied to the project-owned presentation tree.
#[derive(Debug)]
struct ExpandedChildRow {
    op_id: String,
    git_oid: Option<String>,
    repository: Option<String>,
    summary: String,
    content: RowContentDto,
    timestamp_ms: u64,
    kind: String,
    author: String,
    commit_id: String,
    is_system: bool,
    record_role: RecordRole,
    activity_kind: ActivityKind,
    visibility: Visibility,
    outcome: Outcome,
    chain_state: ChainState,
    turn_id: Option<String>,
    promoted: bool,
    activity_bundle: Option<editchain_protocol::ActivityBundleDto>,
    file_change: Option<FileChangeDto>,
}

use editchain_store::BlobPreviewResolution;
pub use editchain_store::{BlobReader as BlobResolver, BlobResolution, ChainReadStats};
use editchain_store::{CanonicalChain, OpRecordLocation};

/// Accepted operation identity paired with its authoritative record location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotOpLocator {
    /// Canonical operation identity.
    pub(crate) id: OpId,
    /// First accepted record carrying this identity.
    pub(crate) location: OpRecordLocation,
}

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

/// Diagnostics reported when a workspace opens.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct OpenDiagnostics {
    /// Chain record canonicalization (dedup/quarantine) counts.
    pub chain: ChainReadStats,
    /// Durable blob hydration counts.
    pub blobs: BlobHydrationStats,
    /// Git discovery and history availability gaps.
    #[serde(default)]
    pub git: GitReadStats,
}

/// Observable gaps in live repository discovery and history reads.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct GitReadStats {
    /// Repository markers or directories that could not be inspected or opened.
    pub unavailable_repositories: usize,
    /// Incomplete history reads or unavailable exact linked commit targets.
    pub history_errors: usize,
}

impl OpenDiagnostics {
    /// Human-readable warnings for integrity gaps discovered during open.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.git.unavailable_repositories > 0 {
            warnings.push(format!(
                "{} Git repository location(s) could not be inspected",
                self.git.unavailable_repositories
            ));
        }
        if self.git.history_errors > 0 {
            warnings.push(format!(
                "{} Git history read(s) or linked target(s) were incomplete",
                self.git.history_errors
            ));
        }
        if self.chain.duplicates > 0 {
            warnings.push(format!(
                "{} exact replay record(s) ignored during open",
                self.chain.duplicates
            ));
        }
        if self.chain.quarantined > 0 {
            warnings.push(format!(
                "{} conflicting same-id record(s) quarantined during open",
                self.chain.quarantined
            ));
        }
        if self.chain.undecodable > 0 {
            warnings.push(format!(
                "{} record(s) could not be decoded; source bytes remain in the segments",
                self.chain.undecodable
            ));
        }
        if self.chain.incomplete_tails > 0 {
            warnings.push(format!(
                "{} incomplete segment tail(s); only complete records were loaded",
                self.chain.incomplete_tails
            ));
        }
        if self.blobs.missing > 0 {
            warnings.push(format!(
                "{} blob payload(s) missing from the durable store",
                self.blobs.missing
            ));
        }
        if self.blobs.corrupt > 0 {
            warnings.push(format!(
                "{} blob payload(s) failed length/hash validation",
                self.blobs.corrupt
            ));
        }
        if self.blobs.unresolved > 0 {
            warnings.push(format!(
                "{} blob reference(s) not addressable by this store",
                self.blobs.unresolved
            ));
        }
        warnings
    }
}

#[must_use]
fn hash_raw(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}

fn hex_string(bytes: &[u8]) -> Result<String, std::fmt::Error> {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(&mut output, "{byte:02x}")?;
    }
    Ok(output)
}

/// Hydrate every blob payload across a chain's decoded ops in place.
///
/// Only payloads whose declared length and BLAKE3 hash verify are replaced
/// with inline content; every other blob stays a [`Payload::Blob`] reference
/// and is counted in the returned stats. Blob references with no inline
/// representation (`FileEdit::Blob`) are validated and preserved unchanged.
/// Full detail reads use this adapter; display and search select their own
/// required fields before reading payloads.
#[must_use]
pub fn hydrate_blob_payloads(ops: &mut [Op], resolver: &BlobResolver) -> BlobHydrationStats {
    let mut stats = BlobHydrationStats::default();
    for op in ops {
        hydrate_kind(&mut op.kind, resolver, &mut stats);
    }
    stats
}

/// Hydrate the payload-bearing fields of one operation kind.
fn hydrate_kind(kind: &mut OpKind, resolver: &BlobResolver, stats: &mut BlobHydrationStats) {
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
const DISPLAY_PREVIEW_READ_LIMIT: usize = 4096;
/// Maximum characters retained in any projection payload.
const DISPLAY_PREVIEW_CHAR_LIMIT: usize = 1024;

/// Build a payload-bounded operation corpus for projection and row summaries.
///
/// The source operations retain their original inline bytes/blob references.
/// Projection clones contain at most a short text preview per payload, so the
/// projection's necessary topology/collapse clones cannot multiply hundreds of
/// megabytes of tool output.
#[must_use]
fn projection_ops_with_previews(
    source_ops: &[Op],
    resolver: &BlobResolver,
) -> (Vec<Op>, BlobHydrationStats, std::collections::HashSet<OpId>) {
    let mut stats = BlobHydrationStats::default();
    let mut incomplete = std::collections::HashSet::new();
    let ops = source_ops
        .iter()
        .map(|source| {
            let mut op = source.clone();
            compact_kind_for_projection(&mut op.kind, resolver, &mut stats);
            if op.kind != source.kind {
                let _: bool = incomplete.insert(op.id);
            }
            op
        })
        .collect();
    (ops, stats, incomplete)
}

/// Replace payloads in one projected operation with bounded display previews.
fn compact_kind_for_projection(
    kind: &mut OpKind,
    resolver: &BlobResolver,
    stats: &mut BlobHydrationStats,
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
    stats: &mut BlobHydrationStats,
) {
    compact_payload(&mut signature.name, resolver, stats);
    compact_payload(&mut signature.email, resolver, stats);
}

/// Materialize at most a prefix of one payload for projection.
fn compact_payload(payload: &mut Payload, resolver: &BlobResolver, stats: &mut BlobHydrationStats) {
    match payload {
        Payload::Inline(bytes) => compact_inline_bytes(bytes),
        Payload::Blob(blob_ref) => {
            stats.deferred = stats.deferred.saturating_add(1);
            match resolver.preview(blob_ref, DISPLAY_PREVIEW_READ_LIMIT) {
                BlobPreviewResolution::Found(mut bytes) => {
                    compact_inline_bytes(&mut bytes);
                    *payload = Payload::Inline(bytes);
                    stats.previewed = stats.previewed.saturating_add(1);
                }
                BlobPreviewResolution::Missing => {
                    stats.missing = stats.missing.saturating_add(1);
                }
                BlobPreviewResolution::Corrupt => {
                    stats.corrupt = stats.corrupt.saturating_add(1);
                }
                BlobPreviewResolution::Unresolvable => {
                    stats.unresolved = stats.unresolved.saturating_add(1);
                }
            }
        }
        Payload::Empty => {}
    }
}

/// Preserve a compact, parseable import discriminator instead of raw JSONL.
fn compact_import_payload(
    payload: &mut Payload,
    resolver: &BlobResolver,
    stats: &mut BlobHydrationStats,
) {
    let preview = match payload {
        Payload::Inline(bytes) => Some(bytes.clone()),
        Payload::Blob(blob_ref) => {
            stats.deferred = stats.deferred.saturating_add(1);
            match resolver.preview(blob_ref, DISPLAY_PREVIEW_READ_LIMIT) {
                BlobPreviewResolution::Found(bytes) => {
                    stats.previewed = stats.previewed.saturating_add(1);
                    Some(bytes)
                }
                BlobPreviewResolution::Missing => {
                    stats.missing = stats.missing.saturating_add(1);
                    None
                }
                BlobPreviewResolution::Corrupt => {
                    stats.corrupt = stats.corrupt.saturating_add(1);
                    None
                }
                BlobPreviewResolution::Unresolvable => {
                    stats.unresolved = stats.unresolved.saturating_add(1);
                    None
                }
            }
        }
        Payload::Empty => None,
    };
    if let Some(bytes) = preview {
        *payload = Payload::Inline(compact_import_record(&bytes));
    }
}

/// Convert raw JSONL to the bounded semantic subset used by row labeling,
/// classification, and outcome logic.
///
/// The projection classifier and outcome logic read the envelope
/// discriminators plus a small semantic subset: `payload.message` /
/// `payload.content` text (bounded), the first `payload.summary` reasoning
/// summary text (bounded), `payload.role`, `arguments`/`output` previews,
/// bounded command-output carriers (`stdout`/`formatted_output`/aggregate
/// spellings), a bounded structural/content signal for tool-payload carriers
/// (`arguments`/`input`/`parameters`), and structured outcome evidence
/// (`status`, `exitCode`, `errorMessage` at `payload` or `payload.item`
/// level, plus the canonical three-line Codex execution-result header), and
/// Claude's interrupted-request identity/marker. Codex token-accounting
/// records retain only their bounded identity strings, request totals, latest
/// context total, and model context limit. That is enough to validate legacy
/// metadata shapes and render a useful numeric subtitle without retaining the
/// full accounting or rate-limit payload.
/// Large outputs stay bounded to the display preview limits, and blob-backed
/// imports pass through the same bounded preview path, so the full record is
/// never copied into the projection.
#[must_use]
fn compact_import_record(bytes: &[u8]) -> Vec<u8> {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        return serde_json::to_vec(&compact_import_value(&value)).unwrap_or_default();
    }

    // Large blob JSON is intentionally read only as a prefix, so a complete
    // serde parse can end at EOF. Discriminators and the semantic subset sit
    // near the envelope start; recover those simple fields without reading
    // the full record.
    let raw = String::from_utf8_lossy(bytes);
    let Some(record_type) = json_string_field(&raw, "type", 0) else {
        return compact_text_bytes(bytes);
    };
    let mut compact = serde_json::Map::new();
    drop(compact.insert(
        "type".to_string(),
        serde_json::Value::String(record_type.to_string()),
    ));
    if record_type == "session_meta" {
        let payload_start = raw.find("\"payload\"").unwrap_or(0);
        let mut payload = serde_json::Map::new();
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "model_provider");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "agent_nickname");
        if !payload.is_empty() {
            drop(compact.insert("payload".to_string(), serde_json::Value::Object(payload)));
        }
    } else if record_type == "token_usage_record" {
        let payload_start = raw.find("\"payload\"").unwrap_or(0);
        let mut payload = serde_json::Map::new();
        for field in [
            "thread_id",
            "turn_id",
            "session_id",
            "root_turn_id",
            "response_id",
        ] {
            let _: bool = copy_preview_string(&raw, payload_start, &mut payload, field);
        }
        for field in ["usage", "turn_token_usage", "thread_token_usage"] {
            if let Some(usage) = json_value_field(&raw, field, payload_start)
                .and_then(|value| compact_token_usage(&value))
            {
                drop(payload.insert(field.to_string(), usage));
            }
        }
        if !payload.is_empty() {
            drop(compact.insert("payload".to_string(), serde_json::Value::Object(payload)));
        }
    } else if let Some(field) = match record_type {
        "custom-title" => Some("customTitle"),
        "ai-title" => Some("aiTitle"),
        "agent-name" => Some("agentName"),
        "session_title" => Some("title"),
        _ => None,
    } {
        let _: bool = copy_preview_string(&raw, 0, &mut compact, field);
    } else if record_type == "user" {
        let _: bool = copy_preview_string(&raw, 0, &mut compact, "interruptedMessageId");
        let _: bool = copy_preview_string(&raw, 0, &mut compact, "text");
    } else if record_type == "assistant" {
        let message_start = raw.find("\"message\"").unwrap_or(0);
        let mut message = serde_json::Map::new();
        let truncated = copy_preview_string(&raw, message_start, &mut message, "id");
        if truncated {
            drop(message.remove("id"));
        }
        if !message.is_empty() {
            drop(compact.insert("message".to_string(), serde_json::Value::Object(message)));
        }
    } else if record_type == "event_msg" || record_type == "response_item" {
        let payload_start = raw.find("\"payload\"").unwrap_or(0);
        let mut payload = serde_json::Map::new();
        // Whether the echo message text (`payload.message` on an
        // `event_msg`/`agent_message`, or the first `payload.content` text on
        // a `response_item`/`message`) was truncated by the preview read limit
        // or the display budget. Truncated text must never participate in
        // exact duplicate pairing, so the classifier is told explicitly
        // instead of guessing from an ellipsis.
        let mut echo_text_truncated = false;
        let event_type = json_string_field(&raw, "type", payload_start);
        if let Some(event_type) = event_type {
            drop(payload.insert(
                "type".to_string(),
                serde_json::Value::String(event_type.to_string()),
            ));
        }
        if event_type == Some("token_count") {
            if let Some(info) = json_value_field(&raw, "info", payload_start) {
                copy_token_count_info(&info, &mut payload);
            }
        }
        echo_text_truncated |= copy_preview_string(&raw, payload_start, &mut payload, "message");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "role");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "status");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "errorMessage");
        // Recover payload-level outcome evidence the same way the full-parse
        // path does (a truncated prefix may cut the record before its
        // `payload.item` block entirely).
        if let Some(code) = json_number_field(&raw, "exitCode", payload_start) {
            drop(payload.insert(
                "exitCode".to_string(),
                serde_json::Value::Number(code.into()),
            ));
        }
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "arguments");
        copy_preview_structured(&raw, payload_start, &mut payload, "arguments");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "input");
        copy_preview_structured(&raw, payload_start, &mut payload, "input");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "parameters");
        copy_preview_structured(&raw, payload_start, &mut payload, "parameters");
        let _: bool = copy_preview_string(&raw, payload_start, &mut payload, "output");
        for field in COMMAND_OUTPUT_FIELDS {
            let _: bool = copy_preview_string(&raw, payload_start, &mut payload, field);
        }
        copy_codex_exec_output_header_from_prefix(&raw, payload_start, &mut payload);
        if let Some(content_start) = raw
            .get(payload_start..)
            .and_then(|tail| tail.find("\"content\""))
        {
            let content_abs = payload_start.saturating_add(content_start);
            if let Some((text, cut_by_read_limit)) =
                json_string_field_preview(&raw, "text", content_abs)
            {
                let decoded = decode_json_string_preview(text);
                let (compact, cut_by_char_limit) = compact_text_with_signal(&decoded);
                echo_text_truncated |= cut_by_read_limit || cut_by_char_limit;
                drop(payload.insert(
                    "content".to_string(),
                    serde_json::json!([{ "type": "input_text", "text": compact }]),
                ));
            }
        }
        if let Some(summary_start) = raw
            .get(payload_start..)
            .and_then(|tail| tail.find("\"summary\""))
        {
            let summary_abs = payload_start.saturating_add(summary_start);
            if let Some(text) = json_string_field(&raw, "text", summary_abs) {
                let decoded = decode_json_string_preview(text);
                if !decoded.trim().is_empty() {
                    drop(payload.insert(
                        "summary".to_string(),
                        serde_json::json!([{ "type": "summary_text", "text": compact_text(&decoded) }]),
                    ));
                }
            }
        }
        if let Some(item_start) = raw
            .get(payload_start..)
            .and_then(|tail| tail.find("\"item\""))
        {
            let item_abs = payload_start.saturating_add(item_start);
            let mut item = serde_json::Map::new();
            let _: bool = copy_preview_string(&raw, item_abs, &mut item, "type");
            let _: bool = copy_preview_string(&raw, item_abs, &mut item, "status");
            let _: bool = copy_preview_string(&raw, item_abs, &mut item, "errorMessage");
            for field in COMMAND_OUTPUT_FIELDS {
                let _: bool = copy_preview_string(&raw, item_abs, &mut item, field);
            }
            if let Some(code) = json_number_field(&raw, "exitCode", item_abs) {
                drop(item.insert(
                    "exitCode".to_string(),
                    serde_json::Value::Number(code.into()),
                ));
            }
            if !item.is_empty() {
                drop(payload.insert("item".to_string(), serde_json::Value::Object(item)));
            }
        }
        if echo_text_truncated {
            drop(payload.insert(
                "echo_text_truncated".to_string(),
                serde_json::Value::Bool(true),
            ));
        }
        if !payload.is_empty() {
            drop(compact.insert("payload".to_string(), serde_json::Value::Object(payload)));
        }
    }
    serde_json::to_vec(&serde_json::Value::Object(compact)).unwrap_or_default()
}

/// Bounded semantic subset of one fully-parsed raw import record.
#[must_use]
fn compact_import_value(value: &serde_json::Value) -> serde_json::Value {
    let record_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut compact = serde_json::Map::new();
    drop(compact.insert(
        "type".to_string(),
        serde_json::Value::String(record_type.to_string()),
    ));
    match record_type {
        "attachment" => {
            drop(compact.insert(
                "attachment".to_string(),
                value.get("attachment").cloned().unwrap_or_default(),
            ));
        }
        "user" => {
            drop(compact.insert(
                "text".to_string(),
                serde_json::Value::String(first_nested_json_text(value).unwrap_or_default()),
            ));
            let _: bool = copy_bounded_field(value, &mut compact, "interruptedMessageId");
        }
        "assistant" => {
            let mut message = serde_json::Map::new();
            let truncated = value
                .get("message")
                .is_some_and(|source| copy_bounded_field(source, &mut message, "id"));
            if truncated {
                drop(message.remove("id"));
            }
            if !message.is_empty() {
                drop(compact.insert("message".to_string(), serde_json::Value::Object(message)));
            }
        }
        "custom-title" => {
            let _: bool = copy_bounded_field(value, &mut compact, "customTitle");
        }
        "ai-title" => {
            let _: bool = copy_bounded_field(value, &mut compact, "aiTitle");
        }
        "agent-name" => {
            let _: bool = copy_bounded_field(value, &mut compact, "agentName");
        }
        "session_title" => {
            let _: bool = copy_bounded_field(value, &mut compact, "title");
        }
        _ => {}
    }
    if let Some(payload) = value.get("payload") {
        let mut compact_payload = serde_json::Map::new();
        // Whether the echo message text (`payload.message` on an
        // `event_msg`/`agent_message`, or the first `payload.content` text on
        // a `response_item`/`message`) was truncated by the display budget.
        // Truncated text must never participate in exact duplicate pairing,
        // so the classifier is told explicitly instead of guessing from the
        // ellipsis.
        let mut echo_text_truncated = false;
        if record_type == "session_meta" {
            let _: bool = copy_bounded_field(payload, &mut compact_payload, "model_provider");
            let _: bool = copy_bounded_field(payload, &mut compact_payload, "agent_nickname");
        }
        copy_token_accounting_payload(record_type, payload, &mut compact_payload);
        copy_string_field(payload, &mut compact_payload, "type");
        copy_string_field(payload, &mut compact_payload, "role");
        echo_text_truncated |= copy_bounded_field(payload, &mut compact_payload, "message");
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "arguments");
        copy_structured_payload_field(payload, &mut compact_payload, "arguments");
        copy_structured_payload_field(payload, &mut compact_payload, "input");
        copy_structured_payload_field(payload, &mut compact_payload, "parameters");
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "output");
        for field in COMMAND_OUTPUT_FIELDS {
            let _: bool = copy_bounded_field(payload, &mut compact_payload, field);
        }
        copy_codex_exec_output_header(payload, &mut compact_payload);
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "status");
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "errorMessage");
        copy_i64_field(payload, &mut compact_payload, "exitCode");
        if let Some(content) = payload.get("content") {
            let (compact, truncated) = compact_content(content);
            echo_text_truncated |= truncated;
            drop(compact_payload.insert("content".to_string(), compact));
        }
        if let Some(summary) = payload.get("summary") {
            let compact = compact_summary(summary);
            match compact.as_array() {
                Some(items) if !items.is_empty() => {
                    drop(compact_payload.insert("summary".to_string(), compact));
                }
                _ => {}
            }
        }
        if let Some(item) = payload.get("item") {
            let mut compact_item = serde_json::Map::new();
            copy_string_field(item, &mut compact_item, "type");
            let _: bool = copy_bounded_field(item, &mut compact_item, "status");
            let _: bool = copy_bounded_field(item, &mut compact_item, "errorMessage");
            for field in COMMAND_OUTPUT_FIELDS {
                let _: bool = copy_bounded_field(item, &mut compact_item, field);
            }
            copy_i64_field(item, &mut compact_item, "exitCode");
            if !compact_item.is_empty() {
                drop(
                    compact_payload
                        .insert("item".to_string(), serde_json::Value::Object(compact_item)),
                );
            }
        }
        if echo_text_truncated {
            drop(compact_payload.insert(
                "echo_text_truncated".to_string(),
                serde_json::Value::Bool(true),
            ));
        }
        if !compact_payload.is_empty() {
            drop(compact.insert(
                "payload".to_string(),
                serde_json::Value::Object(compact_payload),
            ));
        }
    }
    serde_json::Value::Object(compact)
}

/// Provider spellings that can carry the readable result of a completed
/// command. Every retained value passes through the bounded text-preview path.
const COMMAND_OUTPUT_FIELDS: [&str; 5] = [
    "stdout",
    "formatted_output",
    "formattedOutput",
    "aggregated_output",
    "aggregatedOutput",
];

/// Copy one JSON string field verbatim into a compact payload object.
fn copy_string_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    if let Some(value) = source.get(key).and_then(serde_json::Value::as_str) {
        drop(out.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        ));
    }
}

/// Copy one JSON text field, bounded to the display preview limit.
///
/// Returns whether the copied text was truncated by the display budget, so
/// callers can flag echo message text that must not participate in exact
/// duplicate pairing.
fn copy_bounded_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> bool {
    let Some(value) = source.get(key).and_then(serde_json::Value::as_str) else {
        return false;
    };
    if value.trim().is_empty() {
        return false;
    }
    let (compact, truncated) = compact_text_with_signal(value);
    drop(out.insert(key.to_string(), serde_json::Value::String(compact)));
    truncated
}

/// Retain only the canonical three-line status header from a fully parsed
/// Codex custom-exec output array.
///
/// The normalized Tool child keeps the complete bounded result preview. This
/// small raw-envelope copy exists solely so projection outcome classification
/// does not lose its structured evidence during service compaction.
fn copy_codex_exec_output_header(
    payload: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if payload.get("type").and_then(serde_json::Value::as_str) != Some("custom_tool_call_output") {
        return;
    }
    let Some(first) = payload
        .get("output")
        .and_then(serde_json::Value::as_array)
        .and_then(|blocks| blocks.first())
    else {
        return;
    };
    if first.get("type").and_then(serde_json::Value::as_str) != Some("input_text") {
        return;
    }
    let Some(header) = first
        .get("text")
        .and_then(serde_json::Value::as_str)
        .and_then(codex_exec_output_header)
    else {
        return;
    };
    drop(out.insert(
        "output".to_string(),
        serde_json::json!([{ "type": "input_text", "text": header }]),
    ));
}

/// Recover the same canonical header when a blob preview ends before the raw
/// JSON record closes and therefore cannot be fully parsed.
fn copy_codex_exec_output_header_from_prefix(
    raw: &str,
    payload_start: usize,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if out.get("type").and_then(serde_json::Value::as_str) != Some("custom_tool_call_output") {
        return;
    }
    let Some(output_rel) = raw
        .get(payload_start..)
        .and_then(|tail| tail.find("\"output\""))
    else {
        return;
    };
    let output_start = payload_start.saturating_add(output_rel);
    let Some((encoded, _)) = json_string_field_preview(raw, "text", output_start) else {
        return;
    };
    let decoded = decode_json_string_preview(encoded);
    let Some(header) = codex_exec_output_header(&decoded) else {
        return;
    };
    drop(out.insert(
        "output".to_string(),
        serde_json::json!([{ "type": "input_text", "text": header }]),
    ));
}

/// Validate and bound Codex's machine-generated execution-result header.
#[must_use]
fn codex_exec_output_header(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let status = lines.next()?;
    if !matches!(status, "Script completed" | "Script failed") {
        return None;
    }
    let wall_time = lines.next()?;
    if !wall_time.starts_with("Wall time ")
        || !wall_time.ends_with(" seconds")
        || lines.next() != Some("Output:")
    {
        return None;
    }
    Some(format!("{status}\n{wall_time}\nOutput:"))
}

/// Copy one structured tool-payload carrier (`arguments`/`input`/
/// `parameters`) into a compact payload, bounded to the display preview
/// limits. A carrier holding a non-empty object/array, non-empty string, or
/// scalar boolean/number keeps a bounded content signal so a childless
/// tool-like envelope is not misread as empty transport after compaction.
fn copy_structured_payload_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    let Some(value) = source.get(key) else {
        return;
    };
    if !is_meaningful_carrier_value(value) {
        return;
    }
    drop(out.insert(key.to_string(), compact_structured(value)));
}

/// Maximum nesting depth retained in a structured tool-payload carrier
/// preview. Deeper input is pruned so pathological nesting cannot recurse
/// without bound.
const STRUCTURED_CARRIER_MAX_DEPTH: usize = 16;
/// Maximum total entries retained across the whole structured tool-payload
/// carrier preview. The budget is shared globally (not per object/array), so
/// the retained output is deterministically bounded.
const STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT: usize = 64;

/// Bounded copy of a structured tool-payload carrier.
///
/// Retains a bounded structural/content signal for the classifier: strings
/// are cut to the display preview limit, object keys are cut the same way
/// (Unicode-scalar safe, with an ellipsis on cut), nesting is pruned at
/// `STRUCTURED_CARRIER_MAX_DEPTH`, and the total number of retained object
/// keys plus array items never exceeds `STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT`
/// across the whole carrier.
#[must_use]
fn compact_structured(value: &serde_json::Value) -> serde_json::Value {
    let mut budget = STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT;
    compact_structured_bounded(value, 0, &mut budget)
}

/// Depth- and budget-bounded recursive step of [`compact_structured`].
#[must_use]
fn compact_structured_bounded(
    value: &serde_json::Value,
    depth: usize,
    budget: &mut usize,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) if depth < STRUCTURED_CARRIER_MAX_DEPTH => {
            let mut compact = serde_json::Map::new();
            for (key, child) in map {
                if *budget == 0 {
                    break;
                }
                // Bound the retained key text the same way as string values;
                // otherwise a few arbitrarily huge keys would keep unbounded
                // raw payload despite the entry budget. Keep the first key
                // when truncation makes two distinct original keys collide,
                // so no retained entry is silently overwritten.
                let bounded_key = compact_text(key);
                if compact.contains_key(&bounded_key) {
                    continue;
                }
                *budget = budget.saturating_sub(1);
                drop(compact.insert(
                    bounded_key,
                    compact_structured_bounded(child, depth.saturating_add(1), budget),
                ));
            }
            serde_json::Value::Object(compact)
        }
        serde_json::Value::Array(items) if depth < STRUCTURED_CARRIER_MAX_DEPTH => {
            let mut compact = Vec::new();
            for item in items {
                if *budget == 0 {
                    break;
                }
                *budget = budget.saturating_sub(1);
                compact.push(compact_structured_bounded(
                    item,
                    depth.saturating_add(1),
                    budget,
                ));
            }
            serde_json::Value::Array(compact)
        }
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => serde_json::Value::Null,
        serde_json::Value::String(text) => serde_json::Value::String(compact_text(text)),
        other @ (serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)) => other.clone(),
    }
}

/// Whether a JSON value carries a meaningful tool-payload signal.
///
/// Non-empty objects/arrays, non-empty strings, and scalar booleans/numbers
/// all carry signal; null, empty strings, and empty objects/arrays do not.
fn is_meaningful_carrier_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => !map.is_empty(),
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::Null => false,
    }
}

/// Retain only numeric fields needed to label Codex token-accounting rows.
fn copy_token_accounting_payload(
    record_type: &str,
    payload: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if record_type == "token_usage_record" {
        for field in [
            "thread_id",
            "turn_id",
            "session_id",
            "root_turn_id",
            "response_id",
        ] {
            let _: bool = copy_bounded_field(payload, out, field);
        }
        for field in ["usage", "turn_token_usage", "thread_token_usage"] {
            copy_token_usage_field(payload, out, field);
        }
    } else if record_type == "event_msg"
        && payload.get("type").and_then(serde_json::Value::as_str) == Some("token_count")
    {
        if let Some(info) = payload.get("info") {
            copy_token_count_info(info, out);
        }
    }
}

/// Copy one usage object while discarding every field except `total_tokens`.
fn copy_token_usage_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) {
    if let Some(usage) = source.get(field).and_then(compact_token_usage) {
        drop(out.insert(field.to_string(), usage));
    }
}

/// Compact a token-usage object to its total while retaining an empty object
/// marker for legacy schema recognition.
fn compact_token_usage(value: &serde_json::Value) -> Option<serde_json::Value> {
    if !value.is_object() {
        return None;
    }
    let mut compact = serde_json::Map::new();
    copy_u64_field(value, &mut compact, "total_tokens");
    Some(serde_json::Value::Object(compact))
}

/// Copy the latest active-context total and context limit from a token event.
fn copy_token_count_info(
    info: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    if !info.is_object() {
        return;
    }
    let mut compact = serde_json::Map::new();
    copy_token_usage_field(info, &mut compact, "last_token_usage");
    copy_token_usage_field(info, &mut compact, "total_token_usage");
    copy_u64_field(info, &mut compact, "model_context_window");
    if !compact.is_empty() {
        drop(out.insert("info".to_string(), serde_json::Value::Object(compact)));
    }
}

/// Copy one non-negative JSON integer field verbatim.
fn copy_u64_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    if let Some(value) = source.get(key).and_then(serde_json::Value::as_u64) {
        drop(out.insert(key.to_string(), serde_json::Value::Number(value.into())));
    }
}

/// Copy one JSON integer field verbatim.
fn copy_i64_field(
    source: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    if let Some(value) = source.get(key).and_then(serde_json::Value::as_i64) {
        drop(out.insert(key.to_string(), serde_json::Value::Number(value.into())));
    }
}

/// Bounded copy of a `payload.content` array for the classifier.
///
/// Keeps only the first text-bearing item (bounded), which is all the prefix
/// classifier and content-presence checks read; the rest of the content stays
/// deferred to the durable record. The returned flag reports whether the
/// retained text was truncated by the display budget, so callers can flag
/// response-item echo message text that must not participate in exact
/// duplicate pairing.
#[must_use]
fn compact_content(content: &serde_json::Value) -> (serde_json::Value, bool) {
    let Some(items) = content.as_array() else {
        return (serde_json::Value::Array(Vec::new()), false);
    };
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let mut compact_item = serde_json::Map::new();
        if let Some(kind) = obj.get("type").and_then(serde_json::Value::as_str) {
            drop(compact_item.insert(
                "type".to_string(),
                serde_json::Value::String(kind.to_string()),
            ));
        }
        let mut truncated = false;
        for key in ["text", "input_text", "output_text"] {
            if let Some(text) = obj.get(key).and_then(serde_json::Value::as_str) {
                let (compact, cut) = compact_text_with_signal(text);
                truncated |= cut;
                drop(compact_item.insert(key.to_string(), serde_json::Value::String(compact)));
            }
        }
        if compact_item.contains_key("text")
            || compact_item.contains_key("input_text")
            || compact_item.contains_key("output_text")
        {
            return (
                serde_json::Value::Array(vec![serde_json::Value::Object(compact_item)]),
                truncated,
            );
        }
    }
    (serde_json::Value::Array(Vec::new()), false)
}

/// Bounded copy of a `payload.summary` array for response-item labeling.
///
/// Reasoning response items carry a `summary` array of summary-text blocks;
/// keeps only the first text-bearing item (bounded), which is all the
/// response-item label logic reads. The rest of the summary stays deferred
/// to the durable record.
#[must_use]
fn compact_summary(summary: &serde_json::Value) -> serde_json::Value {
    let Some(items) = summary.as_array() else {
        return serde_json::Value::Array(Vec::new());
    };
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let mut compact_item = serde_json::Map::new();
        if let Some(kind) = obj.get("type").and_then(serde_json::Value::as_str) {
            drop(compact_item.insert(
                "type".to_string(),
                serde_json::Value::String(kind.to_string()),
            ));
        }
        if let Some(text) = obj.get("text").and_then(serde_json::Value::as_str) {
            if !text.trim().is_empty() {
                drop(compact_item.insert(
                    "text".to_string(),
                    serde_json::Value::String(compact_text(text)),
                ));
            }
        }
        if compact_item.contains_key("text") {
            return serde_json::Value::Array(vec![serde_json::Value::Object(compact_item)]);
        }
    }
    serde_json::Value::Array(Vec::new())
}

/// Copy one simple string field recovered from a truncated JSON prefix,
/// bounded to the display preview limit.
///
/// Returns whether the copied value was truncated by the preview read limit
/// or the display budget, so callers can flag echo message text that must not
/// participate in exact duplicate pairing.
fn copy_preview_string(
    raw: &str,
    start: usize,
    out: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> bool {
    let Some((value, cut_by_read_limit)) = json_string_field_preview(raw, field, start) else {
        return false;
    };
    let decoded = decode_json_string_preview(value);
    if decoded.trim().is_empty() {
        return false;
    }
    let (compact, cut_by_char_limit) = compact_text_with_signal(&decoded);
    drop(out.insert(field.to_string(), serde_json::Value::String(compact)));
    cut_by_read_limit || cut_by_char_limit
}

/// Decode the JSON escapes in one recovered string fragment.
///
/// Prefix recovery sees the bytes *inside* the source JSON quotes. Wrapping a
/// fragment in a fresh pair of quotes lets serde decode complete escapes (for
/// example `\n` and `\"`) so display summaries match the fully parsed path.
/// A preview can end in an incomplete escape; in that case parsing fails and
/// the raw fragment is retained while the caller's read-limit flag preserves
/// the conservative truncation semantics.
fn decode_json_string_preview(fragment: &str) -> String {
    let mut wrapped = String::with_capacity(fragment.len().saturating_add(2));
    wrapped.push('"');
    wrapped.push_str(fragment);
    wrapped.push('"');
    serde_json::from_str::<String>(&wrapped).unwrap_or_else(|_| fragment.to_string())
}

/// Copy one structured tool-payload carrier recovered from a truncated JSON
/// prefix, bounded to whatever lies within the preview prefix. A carrier that
/// starts but does not close before the cutoff keeps the incomplete-carrier
/// sentinel so a large childless tool call is not misread as empty transport.
fn copy_preview_structured(
    raw: &str,
    start: usize,
    out: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) {
    let Some(value) = json_value_field(raw, field, start) else {
        return;
    };
    if !is_meaningful_carrier_value(&value) {
        return;
    }
    drop(out.insert(field.to_string(), compact_structured(&value)));
}

/// Sentinel retained for a structured tool-payload carrier that begins inside
/// the bounded preview but closes after the read cutoff.
#[must_use]
fn incomplete_carrier_sentinel() -> serde_json::Value {
    serde_json::json!({ "truncated": true })
}

/// Extract one JSON object/array/scalar field value from a truncated prefix.
///
/// An object/array must close within the bounded prefix to parse; one cut off
/// by the preview read limit yields the incomplete-carrier sentinel instead
/// of nothing, so a large childless tool call still carries a content signal.
/// Numbers and booleans are recovered as bounded scalar tokens; null and
/// absent fields yield values the caller's meaningfulness check drops.
fn json_value_field(raw: &str, field: &str, start: usize) -> Option<serde_json::Value> {
    let tail = raw.get(start..)?;
    let needle = format!("\"{field}\"");
    let field_offset = tail.find(&needle)?.saturating_add(needle.len());
    let after_field = tail.get(field_offset..)?;
    let colon = after_field.find(':')?;
    let value = after_field.get(colon.saturating_add(1)..)?.trim_start();
    match value.chars().next()? {
        '{' | '[' => {
            let mut depth: i64 = 0;
            let mut in_string = false;
            let mut escaped = false;
            for (idx, ch) in value.char_indices() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        in_string = false;
                    }
                    continue;
                }
                match ch {
                    '"' => in_string = true,
                    '{' | '[' => depth = depth.saturating_add(1),
                    '}' | ']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            let end = idx.saturating_add(ch.len_utf8());
                            return serde_json::from_str(value.get(..end)?).ok();
                        }
                    }
                    _ => {}
                }
            }
            Some(incomplete_carrier_sentinel())
        }
        _ => {
            let end = value
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '+' | '.')))
                .unwrap_or(value.len());
            serde_json::from_str(value.get(..end)?).ok()
        }
    }
}

/// Find the first non-empty nested JSON `text` field.
fn first_nested_json_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(serde_json::Value::as_str) {
                if !text.trim().is_empty() {
                    return Some(compact_text(text));
                }
            }
            map.values().find_map(first_nested_json_text)
        }
        serde_json::Value::Array(values) => values.iter().find_map(first_nested_json_text),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => None,
    }
}

/// Extract one simple JSON string field from a prefix, reporting whether the
/// value was cut before its closing quote by the preview read limit.
fn json_string_field_preview<'a>(
    raw: &'a str,
    field: &str,
    start: usize,
) -> Option<(&'a str, bool)> {
    let tail = raw.get(start..)?;
    let needle = format!("\"{field}\"");
    let field_offset = tail.find(&needle)?.saturating_add(needle.len());
    let after_field = tail.get(field_offset..)?;
    let colon = after_field.find(':')?;
    let value = after_field.get(colon.saturating_add(1)..)?.trim_start();
    let quoted = value.strip_prefix('"')?;
    // A large blob is read only up to the preview limit, so a long value may
    // be cut before its closing quote; the bounded remainder is still the
    // value's prefix (the marker lives at the start).
    // A quote closes the JSON string only when the immediately preceding run
    // of backslashes has even length. A plain `find('"')` mistakes `\"` for
    // the closing delimiter and can turn a read-limit-cut echo into an
    // apparently complete value, defeating the truncation safety flag.
    let mut odd_backslash_run = false;
    for (offset, byte) in quoted.bytes().enumerate() {
        if byte == b'\\' {
            odd_backslash_run = !odd_backslash_run;
            continue;
        }
        if byte == b'"' && !odd_backslash_run {
            return Some((quoted.get(..offset)?, false));
        }
        odd_backslash_run = false;
    }
    Some((quoted, true))
}

/// Extract one simple JSON string field from a prefix.
fn json_string_field<'a>(raw: &'a str, field: &str, start: usize) -> Option<&'a str> {
    json_string_field_preview(raw, field, start).map(|(value, _)| value)
}

/// Extract one simple JSON integer field from a prefix.
fn json_number_field(raw: &str, field: &str, start: usize) -> Option<i64> {
    let tail = raw.get(start..)?;
    let needle = format!("\"{field}\"");
    let field_offset = tail.find(&needle)?.saturating_add(needle.len());
    let after_field = tail.get(field_offset..)?;
    let colon = after_field.find(':')?;
    let value = after_field.get(colon.saturating_add(1)..)?.trim_start();
    let end = value
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(value.len());
    value.get(..end)?.parse().ok()
}

/// Bound one inline byte vector as UTF-8 display text.
fn compact_inline_bytes(bytes: &mut Vec<u8>) {
    *bytes = compact_text_bytes(bytes);
}

/// Bound arbitrary bytes as lossy UTF-8 display text.
fn compact_text_bytes(bytes: &[u8]) -> Vec<u8> {
    compact_text(&String::from_utf8_lossy(bytes)).into_bytes()
}

/// Bound display text by Unicode scalar count, appending an ellipsis on cut,
/// reporting whether the text was truncated.
#[must_use]
fn compact_text_with_signal(text: &str) -> (String, bool) {
    let mut chars = text.chars();
    let mut compact: String = chars.by_ref().take(DISPLAY_PREVIEW_CHAR_LIMIT).collect();
    let truncated = chars.next().is_some();
    if truncated {
        compact.push('…');
    }
    (compact, truncated)
}

/// Bound display text by Unicode scalar count, appending an ellipsis on cut.
#[must_use]
fn compact_text(text: &str) -> String {
    compact_text_with_signal(text).0
}

/// Record a blob-backed field whose operation shape has no inline payload slot.
fn defer_blob_ref(blob_ref: &BlobRef, resolver: &BlobResolver, stats: &mut BlobHydrationStats) {
    stats.deferred = stats.deferred.saturating_add(1);
    match resolver.preview(blob_ref, 0) {
        BlobPreviewResolution::Found(_) => {}
        BlobPreviewResolution::Missing => stats.missing = stats.missing.saturating_add(1),
        BlobPreviewResolution::Corrupt => stats.corrupt = stats.corrupt.saturating_add(1),
        BlobPreviewResolution::Unresolvable => {
            stats.unresolved = stats.unresolved.saturating_add(1);
        }
    }
}

/// Session-level Git evidence retained by an importer.
#[derive(Debug, Clone)]
struct SessionGitContext {
    cwd: Option<PathBuf>,
    repository: RepositoryId,
    commit_oid: GitOid,
}

/// Complete file evidence carried by Codex's raw `item_completed/FileChange`
/// record. Reading this additive source lane keeps version-four chains useful:
/// their legacy normalized `FileOp` collapsed a multi-path change onto the
/// first path, while the byte-exact raw record still retains every path.
#[derive(Debug, Clone)]
enum RecordedCodexFileEdit {
    Add(String),
    Delete(String),
    Update(String),
}

#[derive(Debug, Clone)]
struct RecordedCodexFileChange {
    path: String,
    edit: RecordedCodexFileEdit,
}

#[derive(Debug, Clone, Copy)]
struct AgentFileEvidence {
    status: FileChangeStatus,
    binary: bool,
    partial: bool,
}

struct AgentFileIndexContext<'a> {
    workspace_root: &'a Path,
    session_contexts: &'a HashMap<SessionId, SessionGitContext>,
    repositories: &'a [editchain_git::RepositoryDiscovery],
}

impl RecordedCodexFileChange {
    const fn status(&self) -> FileChangeStatus {
        match &self.edit {
            RecordedCodexFileEdit::Add(_) => FileChangeStatus::Added,
            RecordedCodexFileEdit::Delete(_) => FileChangeStatus::Deleted,
            RecordedCodexFileEdit::Update(_) => FileChangeStatus::Modified,
        }
    }

    const fn partial(&self) -> bool {
        matches!(&self.edit, RecordedCodexFileEdit::Update(_))
    }

    fn binary(&self) -> bool {
        let content = match &self.edit {
            RecordedCodexFileEdit::Add(content)
            | RecordedCodexFileEdit::Delete(content)
            | RecordedCodexFileEdit::Update(content) => content,
        };
        bytes_are_binary(content.as_bytes())
    }
}

/// Build the backward-compatible agent file-row index from operations that are
/// already present in the chain. Claude edits are recovered from structured
/// tool inputs. Codex prefers the byte-exact raw `FileChange` record (which
/// repairs historical multi-path normalization loss at read time), then falls
/// back to normalized `FileOp`s plus their explicit path annotation notes.
fn agent_file_change_index(
    ops: &[Op],
    workspace_root: &Path,
    resolver: Option<&BlobResolver>,
    repositories: &[editchain_git::RepositoryDiscovery],
) -> HashMap<OpId, Vec<FileChangeDto>> {
    let import_ids: std::collections::HashSet<OpId> = ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::Import(_)))
        .map(|op| op.id)
        .collect();
    let raw_codex_candidates: std::collections::HashSet<OpId> = ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::File(_)))
        .flat_map(|op| op.parents.iter())
        .filter(|parent| import_ids.contains(parent))
        .copied()
        .collect();
    let path_notes = agent_path_notes(ops);
    let session_contexts = session_git_contexts(ops, resolver);
    let op_by_id: HashMap<OpId, &Op> = ops.iter().map(|op| (op.id, op)).collect();
    let mut changes: HashMap<OpId, Vec<FileChangeDto>> = HashMap::new();
    let mut raw_codex_owners = std::collections::HashSet::new();
    let index_context = AgentFileIndexContext {
        workspace_root,
        session_contexts: &session_contexts,
        repositories,
    };

    // Old Codex chains remain append-only and cannot replace the legacy first
    // FileOp at the same deterministic ID. Recover the authoritative list from
    // its retained raw record and suppress only that record's lossy normalized
    // children. Fresh version-five imports take this path too, keeping one
    // canonical descriptor/materializer across generations. Restrict full raw
    // hydration to imports that own a normalized FileOp: unrelated command and
    // tool-result records can contain very large blob payloads.
    for op in ops {
        let OpKind::Import(import) = &op.kind else {
            continue;
        };
        if !raw_codex_candidates.contains(&op.id) {
            continue;
        }
        let Some(raw) = complete_payload_text(&import.raw_ref, resolver) else {
            continue;
        };
        let recorded = recorded_codex_file_changes(&raw);
        if recorded.is_empty() {
            continue;
        }
        let _inserted = raw_codex_owners.insert(op.id);
        let rows = changes.entry(op.id).or_default();
        for change in recorded {
            rows.push(agent_file_change_dto(
                op.id,
                op,
                &change.path,
                AgentFileEvidence {
                    status: change.status(),
                    binary: change.binary(),
                    partial: change.partial(),
                },
                &index_context,
            ));
        }
    }

    for op in ops {
        let owner = op
            .parents
            .iter()
            .find(|parent| import_ids.contains(parent))
            .copied()
            .unwrap_or(op.id);
        if raw_codex_owners.contains(&owner) {
            continue;
        }
        let path = match &op.kind {
            OpKind::Tool(tool)
                if matches!(tool.stage, editchain_core::op::ToolStage::Start)
                    && is_file_edit_tool(&payload_text(&tool.tool_name)) =>
            {
                let input = payload_preview_text(&tool.content, resolver);
                edit_tool_path(&input)
            }
            OpKind::File(file)
                if !matches!(file.edit, editchain_core::op::FileEdit::None)
                    || file.base.is_some()
                    || file.after.is_some() =>
            {
                path_notes.get(&op.id).cloned()
            }
            OpKind::ChainStart(_)
            | OpKind::Actor(_)
            | OpKind::Message(_)
            | OpKind::Tool(_)
            | OpKind::Command(_)
            | OpKind::File(_)
            | OpKind::Reflection(_)
            | OpKind::Import(_)
            | OpKind::Note(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_) => None,
        };
        let Some(path) = path.filter(|path| !path.trim().is_empty()) else {
            continue;
        };
        let status = match &op.kind {
            OpKind::File(file) if matches!(file.stage, editchain_core::op::FileStage::Deleted) => {
                FileChangeStatus::Deleted
            }
            OpKind::ChainStart(_)
            | OpKind::Actor(_)
            | OpKind::Message(_)
            | OpKind::Tool(_)
            | OpKind::Command(_)
            | OpKind::File(_)
            | OpKind::Reflection(_)
            | OpKind::Import(_)
            | OpKind::Note(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_) => FileChangeStatus::Modified,
        };
        let partial = !matches!(
            &op.kind,
            OpKind::File(file) if file.base.is_some() && file.after.is_some()
        );
        let owner_op = op_by_id.get(&owner).copied().unwrap_or(op);
        changes
            .entry(owner)
            .or_default()
            .push(agent_file_change_dto(
                op.id,
                owner_op,
                &path,
                AgentFileEvidence {
                    status,
                    binary: false,
                    partial,
                },
                &index_context,
            ));
    }
    for rows in changes.values_mut() {
        rows.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.op_id.cmp(&right.op_id))
        });
    }
    changes
}

fn agent_file_change_dto(
    source_op: OpId,
    owner: &Op,
    path: &str,
    evidence: AgentFileEvidence,
    index: &AgentFileIndexContext<'_>,
) -> FileChangeDto {
    let session = match owner.scope {
        ScopeRef::Session(session) => Some(session),
        ScopeRef::None | ScopeRef::Chain(_) | ScopeRef::Turn(_) | ScopeRef::File(_) => None,
    };
    let context = session.and_then(|session| index.session_contexts.get(&session));
    let (repository, commit_oid, repository_path) = context.map_or((None, None, None), |context| {
        let repository_path = index
            .repositories
            .iter()
            .find(|repo| repo.id == context.repository)
            .and_then(|repo| agent_repository_path(path, context.cwd.as_deref(), repo));
        (
            Some(context.repository.0.to_string()),
            Some(context.commit_oid.to_hex()),
            repository_path,
        )
    });
    FileChangeDto {
        source: FileChangeSource::Agent,
        path: display_agent_path(path, index.workspace_root),
        old_path: None,
        status: evidence.status,
        binary: evidence.binary,
        partial: evidence.partial,
        op_id: Some(source_op.to_string()),
        repository,
        repository_path,
        commit_oid,
        old_oid: None,
        new_oid: None,
        old_mode: None,
        new_mode: None,
    }
}

/// Compute immutable Git file rows once per loaded projection. The render
/// snapshot persists these additive DTOs, so reopening the same HEAD does not
/// repeat every historical tree diff.
fn git_file_change_index(
    projection: &HistoryProjection,
    repositories: &[editchain_git::RepositoryDiscovery],
) -> HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>> {
    let mut index = HashMap::new();
    for discovery in repositories {
        let Ok(handle) = open_repository(discovery) else {
            continue;
        };
        for commit in projection
            .git()
            .commits()
            .values()
            .filter(|commit| commit.repository == discovery.id)
        {
            let Ok(changes) = commit_file_changes(&handle, &commit.oid) else {
                continue;
            };
            let rows = changes
                .into_iter()
                .map(|change| git_file_change_dto(commit.repository, commit.oid, change))
                .collect();
            drop(index.insert((commit.repository, commit.oid), rows));
        }
    }
    index
}

fn git_file_change_dto(
    repository: RepositoryId,
    commit_oid: GitOid,
    change: GitFileChange,
) -> FileChangeDto {
    FileChangeDto {
        source: FileChangeSource::Git,
        repository: Some(repository.0.to_string()),
        repository_path: Some(change.path.clone()),
        commit_oid: Some(commit_oid.to_hex()),
        path: change.path,
        old_path: change.old_path,
        status: protocol_git_file_status(change.status),
        binary: change.binary,
        partial: false,
        op_id: None,
        old_oid: change.old_oid.map(|oid| oid.to_hex()),
        new_oid: change.new_oid.map(|oid| oid.to_hex()),
        old_mode: change.old_mode,
        new_mode: change.new_mode,
    }
}

#[must_use]
const fn protocol_git_file_status(status: GitFileStatus) -> FileChangeStatus {
    match status {
        GitFileStatus::Added => FileChangeStatus::Added,
        GitFileStatus::Deleted => FileChangeStatus::Deleted,
        GitFileStatus::Modified => FileChangeStatus::Modified,
        GitFileStatus::Renamed => FileChangeStatus::Renamed,
        GitFileStatus::Copied => FileChangeStatus::Copied,
        GitFileStatus::TypeChanged => FileChangeStatus::TypeChanged,
    }
}

fn agent_path_notes(ops: &[Op]) -> HashMap<OpId, String> {
    let mut paths = HashMap::new();
    for op in ops {
        let OpKind::Note(note) = &op.kind else {
            continue;
        };
        if note.relationship != editchain_core::op::NoteRelationship::Explains {
            continue;
        }
        let path = payload_text(&note.content);
        if path.trim().is_empty() {
            continue;
        }
        for target in &note.target_ids {
            let _path = paths.entry(*target).or_insert_with(|| path.clone());
        }
    }
    paths
}

fn session_git_contexts(
    ops: &[Op],
    resolver: Option<&BlobResolver>,
) -> HashMap<SessionId, SessionGitContext> {
    let mut cwd_by_session: HashMap<SessionId, PathBuf> = HashMap::new();
    for op in ops {
        let (ScopeRef::Session(session), OpKind::Import(import)) = (op.scope, &op.kind) else {
            continue;
        };
        if cwd_by_session.contains_key(&session) {
            continue;
        }
        let raw = payload_preview_text(&import.raw_ref, resolver);
        if let Some(cwd) = import_cwd(&raw) {
            drop(cwd_by_session.insert(session, PathBuf::from(cwd)));
        }
    }
    let mut contexts = HashMap::new();
    for op in ops {
        let (ScopeRef::Session(session), OpKind::GitLink(link)) = (op.scope, &op.kind) else {
            continue;
        };
        if link.kind != editchain_core::GitLinkKind::BasedOn {
            continue;
        }
        let _context = contexts
            .entry(session)
            .or_insert_with(|| SessionGitContext {
                cwd: cwd_by_session.get(&session).cloned(),
                repository: link.target_repo,
                commit_oid: link.target_oid,
            });
    }
    contexts
}

fn payload_preview_text(payload: &Payload, resolver: Option<&BlobResolver>) -> String {
    match payload {
        Payload::Inline(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Payload::Blob(blob) => resolver
            .and_then(|resolver| match resolver.preview(blob, 16 * 1024) {
                BlobPreviewResolution::Found(bytes) => {
                    Some(String::from_utf8_lossy(&bytes).into_owned())
                }
                BlobPreviewResolution::Missing
                | BlobPreviewResolution::Corrupt
                | BlobPreviewResolution::Unresolvable => None,
            })
            .unwrap_or_default(),
        Payload::Empty => String::new(),
    }
}

/// Resolve a complete UTF-8 payload for exact source-evidence parsing.
fn complete_payload_text(payload: &Payload, resolver: Option<&BlobResolver>) -> Option<String> {
    let bytes = match payload {
        Payload::Inline(bytes) => bytes.clone(),
        Payload::Blob(blob) => match resolver?.resolve(blob) {
            BlobResolution::Found(bytes) => bytes,
            BlobResolution::Missing | BlobResolution::Corrupt | BlobResolution::Unresolvable => {
                return None
            }
        },
        Payload::Empty => return None,
    };
    String::from_utf8(bytes).ok()
}

/// Parse the exact Codex rollout shape that carries completed file content.
/// Unknown generations/shapes remain on the normalized `FileOp` fallback.
fn recorded_codex_file_changes(raw: &str) -> Vec<RecordedCodexFileChange> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    if root.get("type").and_then(serde_json::Value::as_str) != Some("event_msg") {
        return Vec::new();
    }
    let Some(payload) = root.get("payload") else {
        return Vec::new();
    };
    if payload.get("type").and_then(serde_json::Value::as_str) != Some("item_completed") {
        return Vec::new();
    }
    let Some(item) = payload.get("item") else {
        return Vec::new();
    };
    if !item
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("filechange"))
    {
        return Vec::new();
    }
    let Some(changes) = item.get("changes").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    changes
        .iter()
        .filter_map(|(path, change)| {
            if path.trim().is_empty() {
                return None;
            }
            let kind = change
                .get("type")
                .and_then(serde_json::Value::as_str)?
                .to_ascii_lowercase();
            let edit = match kind.as_str() {
                "add" => RecordedCodexFileEdit::Add(json_text(change.get("content"))?),
                "delete" => RecordedCodexFileEdit::Delete(json_text(change.get("content"))?),
                "update" => RecordedCodexFileEdit::Update(json_text(
                    change.get("unified_diff").or_else(|| change.get("diff")),
                )?),
                _ => return None,
            };
            Some(RecordedCodexFileChange {
                path: path.clone(),
                edit,
            })
        })
        .collect()
}

#[must_use]
fn is_file_edit_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "multiedit" | "notebookedit"
    )
}

fn edit_tool_path(input: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|value| {
            ["file_path", "notebook_path", "path"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
                .map(str::to_owned)
        })
        .or_else(|| {
            ["file_path", "notebook_path", "path"]
                .into_iter()
                .find_map(|key| json_string_field(input, key, 0).map(str::to_owned))
        })
}

fn import_cwd(raw: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("cwd")
                .or_else(|| value.get("payload").and_then(|payload| payload.get("cwd")))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| json_string_field(raw, "cwd", 0).map(str::to_owned))
}

fn display_agent_path(path: &str, workspace_root: &Path) -> String {
    let path_buf = Path::new(path);
    let displayed = if path_buf.is_absolute() {
        path_buf.strip_prefix(workspace_root).unwrap_or(path_buf)
    } else {
        path_buf
    };
    displayed.to_string_lossy().replace('\\', "/")
}

fn agent_repository_path(
    path: &str,
    cwd: Option<&Path>,
    repository: &editchain_git::RepositoryDiscovery,
) -> Option<String> {
    repository
        .relative_worktree_path(Path::new(path), cwd)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

impl Workspace {
    /// Identity that scopes every request and response for this opened view.
    #[must_use]
    pub const fn snapshot_id(&self) -> &SnapshotId {
        &self.snapshot_id
    }

    /// Read the loaded projection without allowing independent cache mutation.
    #[must_use]
    pub const fn projection(&self) -> &HistoryProjection {
        &self.projection
    }

    /// Repository locations fixed when this workspace was opened.
    #[must_use]
    pub const fn repositories(&self) -> &RepositoryCatalog {
        &self.repositories
    }

    /// Create a workspace from an existing projection (used in tests).
    #[must_use]
    pub fn from_projection(projection: HistoryProjection) -> Self {
        let source_ops = projection.ops().to_vec();
        let session_metadata = session_metadata_index(projection.ops());
        let source_op_index = source_ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.id, index))
            .collect();
        Self {
            projection,
            source_ops,
            source_op_index,
            session_metadata,
            agent_file_changes: HashMap::new(),
            git_file_changes: HashMap::new(),
            source_op_locations: Vec::new(),
            blob_resolver: None,
            repositories: RepositoryCatalog::default(),
            diagnostics: OpenDiagnostics::default(),
            root_path: PathBuf::new(),
            chain_path: PathBuf::new(),
            source_identity: None,
            snapshot_id: unique_snapshot_id("memory"),
            backend: WorkspaceBackend::Projected,
            current_view: None,
        }
    }

    /// Load a workspace from a chain directory and discover git repos.
    ///
    /// # Errors
    ///
    /// Returns an error if the chain cannot be read or repos cannot be discovered.
    pub fn open(workspace_path: &str, chain_dir: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_with_cache(workspace_path, chain_dir, true)
    }

    /// Reopen authoritative history without reusing a derived render cache.
    ///
    /// Every explicit refresh receives a new request identity even if the
    /// captured source fingerprint is unchanged. This includes availability
    /// changes outside that fingerprint, such as an alternate Git object store.
    ///
    /// # Errors
    ///
    /// Returns source, repository, or projection failures while opening history.
    pub fn refresh(
        workspace_path: &str,
        chain_dir: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut workspace = Self::open_with_cache(workspace_path, chain_dir, false)?;
        workspace.snapshot_id =
            unique_snapshot_id(&format!("refresh:{}", workspace.snapshot_id.as_str()));
        Ok(workspace)
    }

    fn open_with_cache(
        workspace_path: &str,
        chain_dir: &str,
        reuse_cache: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Resolve the chain directory relative to the workspace when it is a
        // relative path (e.g. ".editchain"). The service process's CWD is not
        // necessarily the workspace root, so we must join explicitly.
        let chain_path = if PathBuf::from(chain_dir).is_absolute() {
            PathBuf::from(chain_dir)
        } else {
            PathBuf::from(workspace_path).join(chain_dir)
        };
        let workspace_path = PathBuf::from(workspace_path);
        let repositories = RepositoryCatalog::discover(&workspace_path)?;
        if let Some(identity) = (reuse_cache && repositories.is_complete())
            .then(|| {
                SnapshotIdentity::capture(&workspace_path, &chain_path, repositories.entries())
            })
            .and_then(Result::ok)
        {
            if let Ok(Some(snapshot)) = RenderSnapshot::open(&chain_path, &identity) {
                let diagnostics = snapshot.diagnostics();
                let workspace = Self {
                    projection: HistoryProjection::from_ops(Vec::new()),
                    source_ops: Vec::new(),
                    source_op_index: HashMap::new(),
                    session_metadata: HashMap::new(),
                    agent_file_changes: HashMap::new(),
                    git_file_changes: HashMap::new(),
                    source_op_locations: Vec::new(),
                    blob_resolver: Some(BlobResolver::open(&chain_path)?),
                    repositories,
                    diagnostics,
                    root_path: workspace_path,
                    chain_path,
                    snapshot_id: snapshot.snapshot_id(),
                    source_identity: Some(identity),
                    backend: WorkspaceBackend::Cached(Box::new(snapshot)),
                    current_view: None,
                };
                workspace.ensure_sources_current()?;
                return Ok(workspace);
            }
        }
        Self::open_projection(workspace_path, chain_path, repositories)
    }

    /// Load the authoritative projection, bypassing any derived render cache.
    fn open_projection(
        workspace_path: PathBuf,
        chain_path: PathBuf,
        repositories: RepositoryCatalog,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let identity =
            SnapshotIdentity::capture(&workspace_path, &chain_path, repositories.entries())?;
        let (source_ops, chain_stats, source_op_locations) = read_chain_ops(&chain_path)?;
        // Keep durable references in the canonical source corpus. The graph
        // projection receives only bounded display previews, preventing large
        // payload bytes from being multiplied by collapse/view/layout clones.
        // Details and search hydrate a single source op at a time on demand.
        let resolver = BlobResolver::open(&chain_path)?;
        let (projection_ops, blob_stats, incomplete) =
            projection_ops_with_previews(&source_ops, &resolver);
        let mut diagnostics = OpenDiagnostics {
            chain: chain_stats,
            blobs: blob_stats,
            git: GitReadStats {
                unavailable_repositories: repositories.issues().len(),
                history_errors: 0,
            },
        };
        let mut projection = HistoryProjection::from_preview_ops(projection_ops, &incomplete);
        // Walk each discovered repo's history into the projection.
        for discovery in &repositories {
            let opened = open_repository(discovery);
            let Ok(handle) = opened else {
                diagnostics.git.unavailable_repositories =
                    diagnostics.git.unavailable_repositories.saturating_add(1);
                continue;
            };
            let walked = walk_history(&handle, 0);
            if let Ok(history) = walked {
                if !history.is_complete() {
                    diagnostics.git.history_errors =
                        diagnostics.git.history_errors.saturating_add(1);
                }
                projection.merge_git_commits(history.commits);
            } else {
                diagnostics.git.history_errors = diagnostics.git.history_errors.saturating_add(1);
            }
        }
        // A session may have started on a commit that is no longer reachable
        // from the repository's current HEAD. Resolve only the exact OIDs
        // carried by durable GitLink ops; never guess from timestamps or text.
        let unresolved_targets =
            merge_exact_git_link_targets(&mut projection, repositories.entries());
        diagnostics.git.history_errors = diagnostics
            .git
            .history_errors
            .saturating_add(unresolved_targets);
        let source_op_index = source_ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.id, index))
            .collect();
        let session_metadata = session_metadata_index(projection.ops());
        let agent_file_changes = agent_file_change_index(
            &source_ops,
            &workspace_path,
            Some(&resolver),
            repositories.entries(),
        );
        let git_file_changes = git_file_change_index(&projection, repositories.entries());
        let workspace = Self {
            projection,
            source_ops,
            source_op_index,
            session_metadata,
            agent_file_changes,
            git_file_changes,
            source_op_locations,
            blob_resolver: Some(resolver),
            repositories,
            diagnostics,
            root_path: workspace_path,
            chain_path,
            snapshot_id: SnapshotId::new(identity.hash()?),
            source_identity: Some(identity),
            backend: WorkspaceBackend::Projected,
            current_view: None,
        };
        workspace.ensure_sources_current()?;
        Ok(workspace)
    }

    /// Materialize the complete projection when details, diffs, or find need
    /// source data that is not stored in the render snapshot.
    pub(crate) fn ensure_projection_loaded(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if matches!(self.backend, WorkspaceBackend::Projected) {
            return Ok(());
        }
        self.ensure_sources_current()?;
        let loaded = Self::open_projection(
            self.root_path.clone(),
            self.chain_path.clone(),
            self.repositories.clone(),
        )?;
        if loaded.source_identity != self.source_identity {
            return Err(stale_snapshot().into());
        }
        // Publish the complete replacement only after validating its sources.
        // All subsequent pages and search use this same computed backend.
        *self = loaded;
        Ok(())
    }

    /// Guard work that reads authoritative files after an opened view exists.
    /// Pure paging and searches of an already-built index retain pinned data.
    pub(crate) fn ensure_sources_current(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(identity) = &self.source_identity {
            let current = SnapshotIdentity::capture(
                &self.root_path,
                &self.chain_path,
                self.repositories.entries(),
            )?;
            if &current != identity {
                return Err(stale_snapshot().into());
            }
        }
        Ok(())
    }

    /// Node count for the Open handshake, independent of backend.
    pub(crate) fn node_count(&self) -> u64 {
        match &self.backend {
            WorkspaceBackend::Cached(snapshot) => snapshot.projection_nodes(),
            WorkspaceBackend::Projected => u64::try_from(self.projection.len()).unwrap_or(u64::MAX),
        }
    }

    /// Accepted chain generation for the Open handshake, independent of backend.
    pub(crate) fn chain_generation(&self) -> u64 {
        match &self.backend {
            WorkspaceBackend::Cached(snapshot) => snapshot.chain_generation(),
            WorkspaceBackend::Projected => {
                u64::try_from(self.projection.ops().len()).unwrap_or(u64::MAX)
            }
        }
    }

    /// Human-readable render-cache status for diagnostics and performance tests.
    pub(crate) const fn render_snapshot_status(&self) -> &'static str {
        if matches!(self.backend, WorkspaceBackend::Cached(_)) {
            "hit"
        } else {
            "miss"
        }
    }

    /// Get a window from the fixed opened Activity view.
    ///
    /// # Errors
    ///
    /// Returns an error if cached rows cannot be read. Failures never become
    /// an apparently successful empty history.
    pub fn history_window(
        &mut self,
        options: HistoryWindowOptions,
    ) -> Result<HistoryWindow, Box<dyn std::error::Error>> {
        if let WorkspaceBackend::Cached(snapshot) = &mut self.backend {
            return snapshot.history_window(options.offset, options.limit, options.include_layout);
        }
        Ok(self.projection_history_window(options))
    }

    /// Compute a history window from the complete in-memory projection.
    #[must_use]
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::needless_borrow,
        reason = "expanded-slot prefix sums are bounded by node count; indexing is bounds-checked by partition_point; node is a &editchain_project::HistoryNode reference"
    )]
    fn projection_history_window(&mut self, options: HistoryWindowOptions) -> HistoryWindow {
        let HistoryWindowOptions {
            offset,
            limit,
            include_layout,
        } = options;
        let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        self.ensure_view_snapshot();
        let Some(snapshot) = self.current_view.as_ref() else {
            return HistoryWindow {
                snapshot_id: self.snapshot_id.clone(),
                rows: Vec::new(),
                total: 0,
                chain_generation: u64::try_from(self.projection.ops().len()).unwrap_or(u64::MAX),
                max_lane: 0,
                sub_op_counts: (offset == 0).then(Vec::new),
                expansion_spans: (offset == 0).then(Vec::new),
                layout_ready: include_layout,
            };
        };
        let filtered = snapshot.entries();
        let ctx = if include_layout {
            Some(snapshot.ensure_layout())
        } else {
            snapshot.layout()
        };

        // The service emits a FIXED fully-expanded depth-first list: every
        // top-level graph node always occupies one parent slot followed by all
        // presentation descendants. Fetch/cache indices therefore never move;
        // collapse/expand is purely a client decision driven by expansion spans.
        let starts = snapshot.starts();
        let expanded_total = snapshot.expanded_total();

        // Find the first top-level node whose expanded block overlaps [offset, end).
        let end_usize = offset_usize.saturating_add(limit_usize);
        let first_node = starts
            .partition_point(|&s| s <= offset_usize)
            .saturating_sub(1)
            .min(filtered.len());
        let mut rows: Vec<HistoryRow> = Vec::new();
        for abs_idx in first_node..filtered.len() {
            if starts[abs_idx] >= end_usize {
                break;
            }
            let entry = &filtered[abs_idx];
            let node = entry.node();
            let block_start = starts[abs_idx];
            // Per-row graph geometry from the layout context (absolute row index
            // into the full sorted list).
            let (lane, above, below, transitions, muted_above, muted_below, muted_transitions) =
                ctx.map_or_else(
                    || {
                        (
                            0,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )
                    },
                    |layout| {
                        (
                            layout.lanes.get(abs_idx).map_or(0, |row| row.lane),
                            layout.row_above.get(abs_idx).cloned().unwrap_or_default(),
                            layout.row_below.get(abs_idx).cloned().unwrap_or_default(),
                            layout
                                .row_transitions
                                .get(abs_idx)
                                .cloned()
                                .unwrap_or_default(),
                            layout
                                .row_muted_above
                                .get(abs_idx)
                                .cloned()
                                .unwrap_or_default(),
                            layout
                                .row_muted_below
                                .get(abs_idx)
                                .cloned()
                                .unwrap_or_default(),
                            layout
                                .row_muted_transitions
                                .get(abs_idx)
                                .cloned()
                                .unwrap_or_default(),
                        )
                    },
                );
            let parent_row = block_start;
            let group = node.group();
            let session_meta = self.session_metadata.get(&group).cloned();
            // Emit the parent row if it falls inside the window.
            if block_start >= offset_usize && block_start < end_usize {
                let parents = snapshot
                    .graph()
                    .parents(node.key())
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                let parent_relations = snapshot
                    .graph()
                    .relations(node.key())
                    .iter()
                    .map(|relation| ParentRelationDto {
                        parent: relation.parent.to_string(),
                        kind: protocol_relation_kind(relation.kind),
                    })
                    .collect();
                rows.push(HistoryRow {
                    op_id: node.op_id().map(|id| id.to_string()),
                    git_oid: node.git_oid().map(|oid| oid.to_hex()),
                    repository: node.repository().map(|rid| rid.0.to_string()),
                    summary: ContentTextDto::new(node.summary(), false).text,
                    content: Some(row_content_dto(node.display_content())),
                    timestamp_ms: node.timestamp_ms(),
                    group: group.clone(),
                    group_end: filtered
                        .get(abs_idx.saturating_add(1))
                        .is_none_or(|next| next.node().group() != group),
                    node_key: node.node_key(),
                    parents,
                    parent_relations,
                    is_submodule: node
                        .repository()
                        .is_some_and(|rid| self.repo_is_submodule(rid)),
                    is_system: node_is_system(&node),
                    author: ContentTextDto::new(node_author(&node), false).text,
                    commit_id: node_commit_id(&node),
                    kind: node.kind(),
                    lane,
                    above,
                    below,
                    transitions,
                    muted_above,
                    muted_below,
                    muted_transitions,
                    sub_ops: entry
                        .children(0)
                        .map(ExpandedChildRow::summary_dto)
                        .collect(),
                    is_subop: false,
                    hierarchy_depth: 0,
                    parent_row: None,
                    subop_kind: None,
                    record_role: node.record_role(),
                    activity_kind: node.activity_kind(),
                    visibility: node.visibility(),
                    outcome: node.outcome(),
                    chain_state: node.chain_state(),
                    turn_id: node.turn_id().map(|id| id.0.to_string()),
                    session_meta: session_meta.clone(),
                    session_summary: entry
                        .annotation()
                        .session_summary
                        .as_ref()
                        .map(session_summary_dto),
                    work_unit: Some(work_unit_dto(&entry.annotation().work_unit)),
                    promoted: entry.annotation().promoted,
                    activity_bundle: node_activity_bundle(node),
                    file_change: None,
                });
            }
            // Emit the fixed depth-first descendant rows immediately after the
            // graph parent. Work groups use depth 1 for activities and depth 2
            // for an activity's pre-existing bundle/detail rows.
            // Lanes passing straight through this sub-op region (between this
            // parent and the next top-level node): any lane with a vertical line
            // leaving this parent downward AND entering the next node from above
            // spans the whole region continuously. Sub-op rows draw these as
            // full-height straight lines with no dot.
            let below_parent = ctx
                .and_then(|layout| layout.row_below.get(abs_idx))
                .map_or(&[][..], Vec::as_slice);
            let above_next = ctx
                .and_then(|layout| layout.row_above.get(abs_idx + 1))
                .map_or(&[][..], Vec::as_slice);
            let region_lanes = intersect_sorted(below_parent, above_next);
            let muted_below_parent = ctx
                .and_then(|layout| layout.row_muted_below.get(abs_idx))
                .map_or(&[][..], Vec::as_slice);
            let muted_above_next = ctx
                .and_then(|layout| layout.row_muted_above.get(abs_idx + 1))
                .map_or(&[][..], Vec::as_slice);
            let muted_region_lanes = intersect_sorted(muted_below_parent, muted_above_next);
            for (i, presentation_row) in entry.descendants().iter().enumerate() {
                let child = presentation_row.content();
                let slot = block_start + 1 + i;
                if slot < offset_usize || slot >= end_usize {
                    continue;
                }
                rows.push(HistoryRow {
                    op_id: (!child.op_id.is_empty()).then(|| child.op_id.clone()),
                    git_oid: child.git_oid.clone(),
                    repository: child.repository.clone(),
                    summary: child.summary.clone(),
                    content: Some(child.content.clone()),
                    timestamp_ms: child.timestamp_ms,
                    group: group.clone(),
                    group_end: false,
                    // Nested rows are not graph nodes; stable synthetic keys
                    // keep group-start detection and click routing unambiguous.
                    node_key: format!("{}::child:{i}", node.node_key()),
                    parents: Vec::new(),
                    parent_relations: Vec::new(),
                    is_submodule: false,
                    is_system: child.is_system,
                    author: child.author.clone(),
                    commit_id: child.commit_id.clone(),
                    kind: child.kind.clone(),
                    // No dot of its own — draw every pass-through lane as a
                    // full-height straight line (both halves meet at midY).
                    lane,
                    above: region_lanes.clone(),
                    below: region_lanes.clone(),
                    transitions: Vec::new(),
                    muted_above: muted_region_lanes.clone(),
                    muted_below: muted_region_lanes.clone(),
                    muted_transitions: Vec::new(),
                    sub_ops: entry
                        .children(i.saturating_add(1))
                        .map(ExpandedChildRow::summary_dto)
                        .collect(),
                    is_subop: true,
                    hierarchy_depth: presentation_row.depth(),
                    parent_row: Some(parent_row.saturating_add(presentation_row.parent_relative())),
                    subop_kind: Some(subop_semantic_class(&child.kind)),
                    record_role: child.record_role,
                    activity_kind: child.activity_kind,
                    visibility: child.visibility,
                    outcome: child.outcome,
                    chain_state: child.chain_state,
                    turn_id: child.turn_id.clone(),
                    session_meta: session_meta.clone(),
                    session_summary: None,
                    work_unit: None,
                    promoted: child.promoted,
                    activity_bundle: child.activity_bundle.clone(),
                    file_change: child.file_change.clone(),
                });
            }
        }
        HistoryWindow {
            snapshot_id: self.snapshot_id.clone(),
            rows,
            total: u64::try_from(expanded_total).unwrap_or(u64::MAX),
            chain_generation: u64::try_from(self.projection.ops().len()).unwrap_or(u64::MAX),
            max_lane: snapshot.max_lane(),
            // The renderer always establishes snapshot state from offset zero;
            // ship the O(V) expansion index once for that snapshot, not with
            // every O(window) page.
            sub_op_counts: (offset == 0).then(|| snapshot.sub_op_counts()),
            expansion_spans: (offset == 0).then(|| {
                snapshot
                    .expansion_spans()
                    .into_iter()
                    .map(expansion_span_dto)
                    .collect()
            }),
            layout_ready: snapshot.layout().is_some(),
        }
    }

    /// Build and cache the immutable fixed Activity-view snapshot.
    fn ensure_view_snapshot(&mut self) {
        if self.current_view.is_some() {
            return;
        }
        let presentation = ServicePresentation {
            agent_changes: &self.agent_file_changes,
            git_changes: &self.git_file_changes,
        };
        self.current_view = Some(self.projection.build_activity_view(
            |repository| !self.repo_is_submodule(repository),
            &presentation,
        ));
    }

    /// Get details for a specific node by operation ID or git OID.
    #[must_use]
    pub fn node_details(
        &self,
        op_id: Option<String>,
        git_oid: Option<GitOid>,
    ) -> Option<NodeDetails> {
        if let Some(op_id_str) = op_id {
            let op_id = OpId::from_display_str(&op_id_str)?;
            let op = self.source_op(op_id)?;
            return Some(node_details_from_op(&op));
        }
        if let Some(oid) = git_oid {
            let commit = self
                .projection
                .git()
                .commits()
                .values()
                .find(|c| c.oid == oid)?;
            return Some(node_details_from_commit(commit));
        }
        None
    }

    /// Load and fully hydrate one canonical source operation from either the
    /// live corpus or a render snapshot's exact segment locator.
    fn source_op(&self, op_id: OpId) -> Option<Op> {
        let mut op = if let Some(index) = self.source_op_index.get(&op_id).copied() {
            self.source_ops.get(index)?.clone()
        } else {
            let WorkspaceBackend::Cached(snapshot) = &self.backend else {
                return None;
            };
            let location = snapshot.op_location(op_id)?;
            let decoded = read_op_at(snapshot.chain_dir(), location).ok()?;
            if decoded.id != op_id {
                return None;
            }
            decoded
        };
        if let Some(resolver) = &self.blob_resolver {
            let mut stats = BlobHydrationStats::default();
            hydrate_kind(&mut op.kind, resolver, &mut stats);
        }
        Some(op)
    }

    /// Revalidate and materialize a file-row identity for VS Code's native
    /// diff editor.
    pub(crate) fn file_diff(&self, change: &FileChangeDto) -> Result<FileDiffDto, String> {
        match change.source {
            FileChangeSource::Git => self.git_file_diff(change),
            FileChangeSource::Agent => self.agent_file_diff(change),
            FileChangeSource::Unknown => Err("unknown file-change source".to_string()),
        }
    }

    fn git_file_diff(&self, requested: &FileChangeDto) -> Result<FileDiffDto, String> {
        let repository = requested
            .repository
            .as_deref()
            .ok_or_else(|| "git file change has no repository".to_string())
            .and_then(parse_repository_id)?;
        let commit_oid = requested
            .commit_oid
            .as_deref()
            .ok_or_else(|| "git file change has no commit".to_string())
            .and_then(parse_git_oid)?;
        let discovery = self
            .repositories
            .iter()
            .find(|discovery| discovery.id == repository)
            .ok_or_else(|| "repository not found".to_string())?;
        let handle = open_repository(discovery).map_err(|error| error.to_string())?;
        let advertised = commit_file_changes(&handle, &commit_oid)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|change| git_file_change_dto(repository, commit_oid, change))
            .find(|actual| same_git_file_change(actual, requested))
            .ok_or_else(|| "file change is not present in the commit".to_string())?;

        if advertised.binary {
            return Ok(FileDiffDto {
                path: advertised.path,
                old_path: advertised.old_path,
                status: advertised.status,
                binary: true,
                partial: false,
                before: String::new(),
                after: String::new(),
                hunks: Vec::new(),
                note: Some("Binary Git blobs cannot be opened as text.".to_string()),
            });
        }
        let before = git_diff_side(
            &handle,
            advertised.old_oid.as_deref(),
            advertised.old_mode.as_deref(),
        )?;
        let after = git_diff_side(
            &handle,
            advertised.new_oid.as_deref(),
            advertised.new_mode.as_deref(),
        )?;
        Ok(FileDiffDto {
            path: advertised.path,
            old_path: advertised.old_path,
            status: advertised.status,
            binary: false,
            partial: false,
            before,
            after,
            hunks: Vec::new(),
            note: None,
        })
    }

    fn agent_file_diff(&self, requested: &FileChangeDto) -> Result<FileDiffDto, String> {
        let op_id = requested
            .op_id
            .as_deref()
            .and_then(OpId::from_display_str)
            .ok_or_else(|| "agent file change has no valid operation id".to_string())?;
        let known = self
            .agent_file_changes
            .values()
            .flatten()
            .any(|change| change == requested);
        if !known {
            return Err("agent file change is not present in the canonical history".to_string());
        }
        let op = self
            .source_op(op_id)
            .ok_or_else(|| "agent edit operation not found".to_string())?;
        match &op.kind {
            OpKind::Tool(tool) => materialize_tool_diff(self, tool, requested),
            OpKind::File(file) => materialize_file_op_diff(self, file, requested),
            OpKind::Import(import) => materialize_codex_raw_file_diff(self, import, requested),
            OpKind::ChainStart(_)
            | OpKind::Actor(_)
            | OpKind::Message(_)
            | OpKind::Command(_)
            | OpKind::Reflection(_)
            | OpKind::Note(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_) => Err("operation is not a retained file edit".to_string()),
        }
    }

    /// Returns true if a repository is nested inside another discovered repo
    /// (i.e. a submodule or vendored nested repo, not the workspace root).
    #[must_use]
    fn is_submodule(&self, discovery: &editchain_git::RepositoryDiscovery) -> bool {
        self.repositories.is_nested(discovery.id)
    }

    /// Returns true if the repository with the given id is a submodule.
    #[must_use]
    fn repo_is_submodule(&self, repository_id: RepositoryId) -> bool {
        self.repositories
            .iter()
            .any(|d| d.id == repository_id && self.is_submodule(d))
    }
}

/// Require the complete immutable identity that was advertised in a Git file
/// row. This prevents a webview message from swapping paths or object IDs
/// before the service reads blob content.
#[must_use]
fn same_git_file_change(actual: &FileChangeDto, requested: &FileChangeDto) -> bool {
    actual == requested
}

fn git_diff_side(
    handle: &RepositoryHandle,
    oid: Option<&str>,
    mode: Option<&str>,
) -> Result<String, String> {
    let Some(oid) = oid else {
        return Ok(String::new());
    };
    if mode == Some("commit") {
        return Ok(format!("{oid}\n"));
    }
    let oid = parse_git_oid(oid)?;
    let blob = resolve_git_blob(handle, &oid).map_err(|error| error.to_string())?;
    if blob.binary {
        return Err("Git blob is not UTF-8 text".to_string());
    }
    String::from_utf8(blob.bytes).map_err(|error| format!("Git blob is not UTF-8 text: {error}"))
}

#[derive(Debug)]
enum AgentBaseline {
    Missing,
    Text(String),
    Binary,
}

#[derive(Debug)]
struct RecordedReplacement {
    old: String,
    new: String,
    replace_all: bool,
}

#[derive(Debug)]
struct AgentDiffContent {
    before: String,
    after: String,
    binary: bool,
    note: String,
}

fn materialize_tool_diff(
    workspace: &Workspace,
    tool: &editchain_core::op::ToolOp,
    requested: &FileChangeDto,
) -> Result<FileDiffDto, String> {
    let input = serde_json::from_str::<serde_json::Value>(&payload_text(&tool.content))
        .map_err(|error| format!("invalid retained tool input: {error}"))?;
    let name = payload_text(&tool.tool_name).to_ascii_lowercase();
    let baseline = agent_git_baseline(workspace, requested);
    let content = match name.as_str() {
        "edit" => materialize_replacements(&input, baseline, false)?,
        "multiedit" => materialize_replacements(&input, baseline, true)?,
        "write" => materialize_write(&input, baseline)?,
        "notebookedit" => materialize_notebook_edit(&input),
        _ => return Err("operation is not a supported file-edit tool".to_string()),
    };
    Ok(FileDiffDto {
        path: requested.path.clone(),
        old_path: requested.old_path.clone(),
        status: requested.status,
        binary: content.binary,
        partial: true,
        before: content.before,
        after: content.after,
        hunks: Vec::new(),
        note: Some(content.note),
    })
}

fn materialize_replacements(
    input: &serde_json::Value,
    baseline: Option<AgentBaseline>,
    multiple: bool,
) -> Result<AgentDiffContent, String> {
    let replacements = if multiple {
        input
            .get("edits")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "retained MultiEdit input has no edits".to_string())?
            .iter()
            .filter_map(recorded_replacement)
            .collect::<Vec<_>>()
    } else {
        recorded_replacement(input).into_iter().collect()
    };
    if replacements.is_empty() {
        return Err("retained edit input has no replacement text".to_string());
    }
    match baseline {
        Some(AgentBaseline::Binary) => Ok(binary_agent_diff(
            "The Git-anchored session baseline is binary; the recorded text edit cannot be previewed.",
        )),
        Some(AgentBaseline::Text(before)) => {
            if let Some(after) = apply_recorded_replacements(&before, &replacements) {
                return Ok(AgentDiffContent {
                    before,
                    after,
                    binary: false,
                    note: "Reconstructed from recorded edit arguments against the session's exact Git baseline; intervening agent edits may not be represented.".to_string(),
                });
            }
            let (before, after) = replacement_snippets(&replacements);
            Ok(AgentDiffContent {
                before,
                after,
                binary: false,
                note: "Recorded edit snippets; they did not apply uniquely to the session's Git baseline.".to_string(),
            })
        }
        Some(AgentBaseline::Missing) | None => {
            let (before, after) = replacement_snippets(&replacements);
            Ok(AgentDiffContent {
                before,
                after,
                binary: false,
                note: "Recorded edit snippets; full before/after file snapshots were not retained.".to_string(),
            })
        }
    }
}

fn materialize_write(
    input: &serde_json::Value,
    baseline: Option<AgentBaseline>,
) -> Result<AgentDiffContent, String> {
    let after = json_text(input.get("content"))
        .ok_or_else(|| "retained Write input has no content".to_string())?;
    match baseline {
        Some(AgentBaseline::Binary) => Ok(binary_agent_diff(
            "The Git-anchored session baseline is binary; the recorded Write content is text.",
        )),
        Some(AgentBaseline::Text(before)) => Ok(AgentDiffContent {
            before,
            after,
            binary: false,
            note: "Recorded full Write content compared with the session's exact Git baseline; intervening agent edits may not be represented.".to_string(),
        }),
        Some(AgentBaseline::Missing) => Ok(AgentDiffContent {
            before: String::new(),
            after,
            binary: false,
            note: "Recorded full Write content; the path did not exist in the session's Git baseline.".to_string(),
        }),
        None => Ok(AgentDiffContent {
            before: String::new(),
            after,
            binary: false,
            note: "Recorded full Write content; the preceding file snapshot was not retained.".to_string(),
        }),
    }
}

fn materialize_notebook_edit(input: &serde_json::Value) -> AgentDiffContent {
    let before = ["old_source", "old_content"]
        .into_iter()
        .find_map(|key| json_text(input.get(key)))
        .unwrap_or_default();
    let after = ["new_source", "new_content", "source"]
        .into_iter()
        .find_map(|key| json_text(input.get(key)))
        .unwrap_or_default();
    AgentDiffContent {
        before,
        after,
        binary: false,
        note: "Recorded notebook cell content; a complete notebook before/after snapshot was not retained.".to_string(),
    }
}

fn recorded_replacement(value: &serde_json::Value) -> Option<RecordedReplacement> {
    let old = json_text(value.get("old_string"))?;
    let new = json_text(value.get("new_string"))?;
    Some(RecordedReplacement {
        old,
        new,
        replace_all: value
            .get("replace_all")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn json_text(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(lines) => lines
            .iter()
            .map(serde_json::Value::as_str)
            .collect::<Option<Vec<_>>>()
            .map(|lines| lines.join("\n")),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Object(_) => None,
    }
}

fn apply_recorded_replacements(
    baseline: &str,
    replacements: &[RecordedReplacement],
) -> Option<String> {
    let mut after = baseline.to_string();
    for replacement in replacements {
        if replacement.old.is_empty() {
            return None;
        }
        let occurrences = after.match_indices(&replacement.old).count();
        if occurrences == 0 || (!replacement.replace_all && occurrences != 1) {
            return None;
        }
        after = if replacement.replace_all {
            after.replace(&replacement.old, &replacement.new)
        } else {
            after.replacen(&replacement.old, &replacement.new, 1)
        };
    }
    Some(after)
}

fn replacement_snippets(replacements: &[RecordedReplacement]) -> (String, String) {
    const SEPARATOR: &str = "\n\n… next recorded edit …\n\n";
    (
        replacements
            .iter()
            .map(|replacement| replacement.old.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR),
        replacements
            .iter()
            .map(|replacement| replacement.new.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR),
    )
}

fn binary_agent_diff(note: &str) -> AgentDiffContent {
    AgentDiffContent {
        before: String::new(),
        after: String::new(),
        binary: true,
        note: note.to_string(),
    }
}

fn agent_git_baseline(workspace: &Workspace, requested: &FileChangeDto) -> Option<AgentBaseline> {
    let repository = parse_repository_id(requested.repository.as_deref()?).ok()?;
    let commit_oid = parse_git_oid(requested.commit_oid.as_deref()?).ok()?;
    let repository_path = requested.repository_path.as_deref()?;
    let discovery = workspace
        .repositories
        .iter()
        .find(|discovery| discovery.id == repository)?;
    let handle = open_repository(discovery).ok()?;
    match resolve_path_at_commit(&handle, &commit_oid, repository_path).ok()? {
        None => Some(AgentBaseline::Missing),
        Some(object) => match object.blob {
            Some(blob) if blob.binary => Some(AgentBaseline::Binary),
            Some(blob) => String::from_utf8(blob.bytes)
                .ok()
                .map(AgentBaseline::Text)
                .or(Some(AgentBaseline::Binary)),
            None => Some(AgentBaseline::Binary),
        },
    }
}

fn materialize_codex_raw_file_diff(
    workspace: &Workspace,
    import: &editchain_core::op::ImportOp,
    requested: &FileChangeDto,
) -> Result<FileDiffDto, String> {
    let raw = payload_text(&import.raw_ref);
    let recorded = recorded_codex_file_changes(&raw)
        .into_iter()
        .find(|change| display_agent_path(&change.path, &workspace.root_path) == requested.path)
        .ok_or_else(|| "Codex file evidence is not present in the raw operation".to_string())?;
    match recorded.edit {
        RecordedCodexFileEdit::Add(after) => file_diff_from_bytes(
            requested,
            Some(Vec::new()),
            Some(after.into_bytes()),
            false,
            None,
        ),
        RecordedCodexFileEdit::Delete(before) => file_diff_from_bytes(
            requested,
            Some(before.into_bytes()),
            Some(Vec::new()),
            false,
            None,
        ),
        RecordedCodexFileEdit::Update(diff) => {
            if bytes_are_binary(diff.as_bytes()) {
                return Ok(FileDiffDto {
                    path: requested.path.clone(),
                    old_path: requested.old_path.clone(),
                    status: requested.status,
                    binary: true,
                    partial: true,
                    before: String::new(),
                    after: String::new(),
                    hunks: Vec::new(),
                    note: Some(
                        "Recorded Codex update evidence is binary and cannot be opened as text."
                            .to_string(),
                    ),
                });
            }
            Ok(recorded_unified_diff(
                requested,
                &diff,
                "Recorded Codex unified-diff hunks; complete sequential file snapshots were not retained.",
            ))
        }
    }
}

fn materialize_file_op_diff(
    workspace: &Workspace,
    file: &editchain_core::op::FileOp,
    requested: &FileChangeDto,
) -> Result<FileDiffDto, String> {
    let base = file
        .base
        .and_then(|id| workspace.blob_resolver.as_ref()?.resolve_content(id));
    let retained_after = file
        .after
        .and_then(|id| workspace.blob_resolver.as_ref()?.resolve_content(id));
    if base.is_some() && retained_after.is_some() {
        return file_diff_from_bytes(requested, base, retained_after, false, None);
    }
    if matches!(file.stage, editchain_core::op::FileStage::Deleted) && base.is_some() {
        return file_diff_from_bytes(requested, base, Some(Vec::new()), false, None);
    }

    match &file.edit {
        editchain_core::op::FileEdit::ReplaceBytes { range, bytes } => {
            let replacement = payload_bytes(workspace, bytes)
                .ok_or_else(|| "replacement bytes are unavailable".to_string())?;
            if let Some(before) = base {
                let after = apply_byte_replacement(&before, *range, &replacement)?;
                file_diff_from_bytes(requested, Some(before), Some(after), false, None)
            } else {
                file_diff_from_bytes(
                    requested,
                    None,
                    Some(replacement),
                    true,
                    Some(
                        "Recorded replacement bytes; the complete preceding file was not retained.",
                    ),
                )
            }
        }
        editchain_core::op::FileEdit::UnifiedDiff(payload) => {
            let bytes = payload_bytes(workspace, payload)
                .ok_or_else(|| "unified diff payload is unavailable".to_string())?;
            let diff = String::from_utf8(bytes)
                .map_err(|error| format!("unified diff payload is not UTF-8: {error}"))?;
            Ok(recorded_unified_diff(
                requested,
                &diff,
                "Recorded unified-diff hunks; complete before/after file snapshots were not retained.",
            ))
        }
        editchain_core::op::FileEdit::Blob(blob) => {
            let after = workspace
                .blob_resolver
                .as_ref()
                .and_then(|resolver| match resolver.resolve(blob) {
                    BlobResolution::Found(bytes) => Some(bytes),
                    BlobResolution::Missing
                    | BlobResolution::Corrupt
                    | BlobResolution::Unresolvable => None,
                })
                .ok_or_else(|| "result blob is unavailable".to_string())?;
            let partial = base.is_none();
            file_diff_from_bytes(
                requested,
                base,
                Some(after),
                partial,
                partial.then_some(
                    "Recorded result content; the complete preceding file was not retained.",
                ),
            )
        }
        editchain_core::op::FileEdit::None => file_diff_from_bytes(
            requested,
            base,
            retained_after,
            true,
            Some("Only one retained file snapshot is available for this operation."),
        ),
    }
}

fn payload_bytes(workspace: &Workspace, payload: &Payload) -> Option<Vec<u8>> {
    match payload {
        Payload::Inline(bytes) => Some(bytes.clone()),
        Payload::Empty => Some(Vec::new()),
        Payload::Blob(blob) => workspace
            .blob_resolver
            .as_ref()
            .and_then(|resolver| match resolver.resolve(blob) {
                BlobResolution::Found(bytes) => Some(bytes),
                BlobResolution::Missing
                | BlobResolution::Corrupt
                | BlobResolution::Unresolvable => None,
            }),
    }
}

fn apply_byte_replacement(
    before: &[u8],
    range: editchain_core::op::ByteRange,
    replacement: &[u8],
) -> Result<Vec<u8>, String> {
    let start = usize::try_from(range.start)
        .map_err(|error| format!("replacement range start is too large: {error}"))?;
    let end = usize::try_from(range.end)
        .map_err(|error| format!("replacement range end is too large: {error}"))?;
    if start > end || end > before.len() {
        return Err("replacement range is outside the retained base".to_string());
    }
    let mut after = Vec::with_capacity(
        before
            .len()
            .saturating_sub(end.saturating_sub(start))
            .saturating_add(replacement.len()),
    );
    let prefix = before
        .get(..start)
        .ok_or_else(|| "replacement start is outside the retained base".to_string())?;
    let suffix = before
        .get(end..)
        .ok_or_else(|| "replacement end is outside the retained base".to_string())?;
    after.extend_from_slice(prefix);
    after.extend_from_slice(replacement);
    after.extend_from_slice(suffix);
    Ok(after)
}

fn file_diff_from_bytes(
    requested: &FileChangeDto,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    partial: bool,
    note: Option<&str>,
) -> Result<FileDiffDto, String> {
    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    let binary = bytes_are_binary(&before) || bytes_are_binary(&after);
    if binary {
        return Ok(FileDiffDto {
            path: requested.path.clone(),
            old_path: requested.old_path.clone(),
            status: requested.status,
            binary: true,
            partial,
            before: String::new(),
            after: String::new(),
            hunks: Vec::new(),
            note: Some("Retained file content is binary and cannot be opened as text.".to_string()),
        });
    }
    let before = String::from_utf8(before)
        .map_err(|error| format!("before content is not UTF-8: {error}"))?;
    let after =
        String::from_utf8(after).map_err(|error| format!("after content is not UTF-8: {error}"))?;
    Ok(FileDiffDto {
        path: requested.path.clone(),
        old_path: requested.old_path.clone(),
        status: requested.status,
        binary: false,
        partial,
        before,
        after,
        hunks: Vec::new(),
        note: note.map(str::to_string),
    })
}

#[must_use]
fn bytes_are_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn recorded_unified_diff(requested: &FileChangeDto, diff: &str, note: &str) -> FileDiffDto {
    let hunks = unified_diff_hunks(diff);
    let (before, after) = match hunks.as_slice() {
        [hunk] => (hunk.before.clone(), hunk.after.clone()),
        [] => (String::new(), diff.to_string()),
        _ => (String::new(), String::new()),
    };
    FileDiffDto {
        path: requested.path.clone(),
        old_path: requested.old_path.clone(),
        status: requested.status,
        binary: false,
        partial: true,
        before,
        after,
        hunks,
        note: Some(note.to_string()),
    }
}

fn unified_diff_hunks(diff: &str) -> Vec<FileDiffHunkDto> {
    let mut hunks = Vec::new();
    let mut header = None;
    let mut before = Vec::new();
    let mut after = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            if let Some(previous_header) = header.replace(line.to_string()) {
                hunks.push(FileDiffHunkDto {
                    header: previous_header,
                    before: before.join("\n"),
                    after: after.join("\n"),
                });
                before.clear();
                after.clear();
            }
            continue;
        }
        if header.is_none() {
            continue;
        }
        if let Some(context) = line.strip_prefix(' ') {
            before.push(context);
            after.push(context);
        } else if let Some(removed) = line.strip_prefix('-') {
            before.push(removed);
        } else if let Some(added) = line.strip_prefix('+') {
            after.push(added);
        }
    }
    if let Some(header) = header {
        hunks.push(FileDiffHunkDto {
            header,
            before: before.join("\n"),
            after: after.join("\n"),
        });
    }
    hunks
}

/// Pregenerate the immutable fixed-view render snapshot used by the extension.
///
/// The source segment log and Git HEADs are fingerprinted before and after the
/// build. A concurrent append or checkout therefore aborts publication instead
/// of exposing rows derived from a mixed source generation.
///
/// # Errors
///
/// Returns an error when the chain/projection cannot be read, the source
/// changes during generation, or the snapshot cannot be written durably.
pub fn prepare_render_snapshot(
    workspace_path: &Path,
    chain_dir: &Path,
) -> Result<RenderSnapshotReport, Box<dyn std::error::Error>> {
    let chain_path = if chain_dir.is_absolute() {
        chain_dir.to_path_buf()
    } else {
        workspace_path.join(chain_dir)
    };
    let repositories = RepositoryCatalog::discover(workspace_path)?;
    if let Some(issue) = repositories.issues().first() {
        return Err(format!(
            "repository discovery is incomplete at {}: {}",
            issue.path.display(),
            issue.message
        )
        .into());
    }
    let identity = SnapshotIdentity::capture(workspace_path, &chain_path, repositories.entries())?;
    if let Ok(Some(snapshot)) = RenderSnapshot::open(&chain_path, &identity) {
        return snapshot.report();
    }

    let mut workspace = Workspace::open_projection(
        workspace_path.to_path_buf(),
        chain_path.clone(),
        repositories.clone(),
    )?;
    let page_limit = 4_096u64;
    let first = workspace.history_window(HistoryWindowOptions {
        offset: 0,
        limit: page_limit,
        include_layout: true,
    })?;
    let sub_op_counts = first
        .sub_op_counts
        .clone()
        .ok_or("snapshot first window omitted expansion index")?;
    let expansion_spans = first
        .expansion_spans
        .clone()
        .ok_or("snapshot first window omitted nested expansion index")?;
    let total = first.total;
    let max_lane = first.max_lane;
    let mut builder = SnapshotBuilder::new(&chain_path, identity.clone())?;
    builder.write_rows(&first.rows)?;
    let mut offset = u64::try_from(first.rows.len())?;
    while offset < total {
        let window = workspace.history_window(HistoryWindowOptions {
            offset,
            limit: page_limit,
            include_layout: true,
        })?;
        if window.rows.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "render snapshot projection ended before its declared total",
            )
            .into());
        }
        builder.write_rows(&window.rows)?;
        offset = offset.saturating_add(u64::try_from(window.rows.len())?);
    }

    let final_identity =
        SnapshotIdentity::capture(workspace_path, &chain_path, repositories.entries())?;
    if final_identity != identity {
        return Err(
            io::Error::other("chain or Git HEAD changed while preparing render snapshot").into(),
        );
    }
    builder.finish(
        SnapshotManifestData {
            projection_nodes: u64::try_from(workspace.projection.len()).unwrap_or(u64::MAX),
            chain_generation: u64::try_from(workspace.projection.ops().len()).unwrap_or(u64::MAX),
            max_lane,
            diagnostics: workspace.diagnostics,
        },
        &sub_op_counts,
        &expansion_spans,
        &workspace.source_op_locations,
    )
}

/// Parse an exact decimal `RepositoryId` string, rejecting anything else.
///
/// # Errors
///
/// Returns an error message when the string is not a valid `u64`.
pub fn parse_repository_id(s: &str) -> Result<RepositoryId, String> {
    s.parse::<u64>()
        .map(RepositoryId)
        .map_err(|_err| format!("invalid repository id: {s:?}"))
}

/// Parse a lowercase hex git OID string (40 chars SHA-1 / 64 SHA-256).
///
/// # Errors
///
/// Returns an error message when the string is not valid hex of a supported
/// length.
pub fn parse_git_oid(s: &str) -> Result<GitOid, String> {
    GitOid::from_hex(s).ok_or_else(|| format!("invalid git oid: {s:?}"))
}

/// Whether a history node is a system-generated artifact (tool results, raw
/// import records) rather than user-facing text.
///
/// The viewer uses this to dim or hide such rows. It is derived from the node's
/// kind — not from sniffing the summary text — so it stays correct regardless
/// of content.
#[must_use]
fn node_is_system(node: &editchain_project::HistoryNode) -> bool {
    match node {
        editchain_project::HistoryNode::EditOperation { op, .. } => {
            matches!(op.kind, OpKind::Tool(_) | OpKind::Import(_))
        }
        // Collapsed imports fold a raw import + its children into one node; the
        // dominant child kind tells us whether it is user-facing text or a
        // system artifact.
        editchain_project::HistoryNode::CollapsedImport { kind, .. } => {
            matches!(
                kind.as_str(),
                "tool" | "import" | "token_count" | "token_usage_record"
            )
        }
        // Execute-run bundles summarize tool/command rows: dim them like the
        // individual tool rows they fold.
        editchain_project::HistoryNode::ExecuteBundle { .. } => true,
        // Work groups and Plan-repeat bundles are navigational/prose-first
        // summary rows; Git is likewise user-facing source history.
        editchain_project::HistoryNode::WorkGroup { .. }
        | editchain_project::HistoryNode::PlanBundle { .. }
        | editchain_project::HistoryNode::GitCommit { .. } => false,
    }
}

/// Author display value for a history node.
///
/// Git commits show the commit author's name. `EditChain` ops have no stored
/// author name (their actor is a derived hash), so they show a tag-derived
/// label (`human` / `agent` / `system`) so the Author column reads uniformly
/// across both row types instead of being blank for ops.
#[must_use]
fn node_author(node: &editchain_project::HistoryNode) -> String {
    match node {
        editchain_project::HistoryNode::EditOperation { op, .. } => op_author_label(op.tags),
        // Collapsed imports carry their author label directly (derived from the
        // children's tags in the projection), since the raw import op's own tags
        // only carry `IMPORT`.
        editchain_project::HistoryNode::CollapsedImport { author, .. }
        | editchain_project::HistoryNode::ExecuteBundle { author, .. }
        | editchain_project::HistoryNode::PlanBundle { author, .. } => author.clone(),
        editchain_project::HistoryNode::WorkGroup { .. } => "agent".to_string(),
        editchain_project::HistoryNode::GitCommit { commit, .. } => {
            payload_text(&commit.author.name)
        }
    }
}

/// Semantic role/activity for a bundled sub-op row.
///
/// Tool-result sub-ops (Finish stage) are results of the enclosing call;
/// bundled metadata imports are lifecycle/system records. Everything else
/// stays conservatively unknown.
#[must_use]
fn sub_op_meta(op: &Op) -> (RecordRole, ActivityKind) {
    match &op.kind {
        OpKind::Tool(t) if matches!(t.stage, editchain_core::op::ToolStage::Finish) => {
            (RecordRole::Result, ActivityKind::Execute)
        }
        OpKind::Tool(_) => (RecordRole::Action, ActivityKind::Execute),
        OpKind::Import(_) => (RecordRole::Lifecycle, ActivityKind::System),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::Command(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => (RecordRole::Unknown, ActivityKind::Unknown),
    }
}

/// Build the bundled sub-op summaries for a row from its attached metadata ops.
///
/// Each bundled sub-op is a raw `Import` op tagged `META`. Its summary is the
/// record type (derived from the raw JSONL's `type` field when parseable, else
/// the raw reference text), so the viewer can label each revealed sub-row.
#[must_use]
fn sub_op_summaries(sub_ops: &[std::sync::Arc<Op>]) -> Vec<SubOpSummary> {
    sub_ops
        .iter()
        .map(|op| {
            let (summary, kind) = sub_op_label(op);
            SubOpSummary {
                op_id: op.id.to_string(),
                summary,
                kind,
                timestamp_ms: op.observed_unix_ms().unwrap_or(0),
            }
        })
        .collect()
}

/// Convert a row's Activity annotation into the protocol's wire DTO.
#[must_use]
fn work_unit_dto(marker: &WorkUnitMarker) -> WorkUnitDto {
    WorkUnitDto {
        id: marker.id.clone(),
        is_start: marker.is_start,
        is_end: marker.is_end,
        title: marker
            .title
            .clone()
            .map(|title| ContentTextDto::new(title, false).text),
        count: marker.count,
    }
}

/// Convert a row's whole-session marker into the additive wire DTO.
#[must_use]
const fn session_summary_dto(marker: &SessionSummaryMarker) -> SessionSummaryDto {
    SessionSummaryDto {
        count: marker.count,
    }
}

/// Collect the bounded session-provenance labels the history UI is allowed to
/// surface. Provider metadata records remain authoritative; this index only
/// avoids making the webview parse provider JSON or repeat large payloads.
#[must_use]
fn session_metadata_index(ops: &[Op]) -> HashMap<String, SessionMetaDto> {
    type Rank = (u8, u64, u16, OpId);
    #[derive(Default)]
    struct RankedMetadata {
        metadata: SessionMetaDto,
        title_rank: Option<Rank>,
        model_rank: Option<Rank>,
        agent_rank: Option<Rank>,
    }

    fn merge_field(
        target: &mut Option<String>,
        target_rank: &mut Option<Rank>,
        incoming: Option<String>,
        rank: Rank,
    ) {
        if incoming.is_some() && target_rank.is_none_or(|current| rank >= current) {
            *target = incoming;
            *target_rank = Some(rank);
        }
    }

    let mut by_group = HashMap::<String, RankedMetadata>::new();
    for op in ops {
        let ScopeRef::Session(session_id) = op.scope else {
            continue;
        };
        let Some((found, title_priority)) = session_metadata_from_op(op) else {
            continue;
        };
        let entry = by_group
            .entry(format!("session:{}", session_id.0))
            .or_default();
        let base_rank = (0, op.clock.as_u64(), op.clock.sub(), op.id);
        merge_field(
            &mut entry.metadata.session_title,
            &mut entry.title_rank,
            found.session_title,
            (title_priority, base_rank.1, base_rank.2, base_rank.3),
        );
        merge_field(
            &mut entry.metadata.model_provider,
            &mut entry.model_rank,
            found.model_provider,
            base_rank,
        );
        merge_field(
            &mut entry.metadata.agent_nickname,
            &mut entry.agent_rank,
            found.agent_nickname,
            base_rank,
        );
    }
    by_group
        .into_iter()
        .map(|(group, mut ranked)| {
            if ranked
                .metadata
                .session_title
                .as_deref()
                .is_some_and(|title| {
                    ranked
                        .metadata
                        .agent_nickname
                        .as_deref()
                        .is_some_and(|agent| title.eq_ignore_ascii_case(agent))
                })
            {
                ranked.metadata.agent_nickname = None;
            }
            (group, ranked.metadata)
        })
        .collect()
}

/// Parse one bounded provider metadata import. The returned priority keeps an
/// explicit custom title ahead of an AI-generated fallback regardless of the
/// records' source ordering.
#[must_use]
fn session_metadata_from_op(op: &Op) -> Option<(SessionMetaDto, u8)> {
    let OpKind::Import(import) = &op.kind else {
        return None;
    };
    let Payload::Inline(raw) = &import.raw_ref else {
        return None;
    };
    let value = serde_json::from_slice::<serde_json::Value>(raw).ok()?;
    let record_type = value.get("type").and_then(serde_json::Value::as_str)?;
    let display_field = |source: &serde_json::Value, name: &str| {
        source
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let (metadata, title_priority) = match record_type {
        "session_meta" => {
            let payload = value.get("payload")?;
            (
                SessionMetaDto {
                    session_title: None,
                    model_provider: display_field(payload, "model_provider"),
                    agent_nickname: display_field(payload, "agent_nickname"),
                },
                0,
            )
        }
        "custom-title" => (
            SessionMetaDto {
                session_title: display_field(&value, "customTitle"),
                ..SessionMetaDto::default()
            },
            2,
        ),
        "ai-title" => (
            SessionMetaDto {
                session_title: display_field(&value, "aiTitle"),
                ..SessionMetaDto::default()
            },
            1,
        ),
        "agent-name" => (
            SessionMetaDto {
                agent_nickname: display_field(&value, "agentName"),
                ..SessionMetaDto::default()
            },
            0,
        ),
        "session_title" => (
            SessionMetaDto {
                session_title: display_field(&value, "title"),
                ..SessionMetaDto::default()
            },
            2,
        ),
        _ => return None,
    };
    (metadata.session_title.is_some()
        || metadata.model_provider.is_some()
        || metadata.agent_nickname.is_some())
    .then_some((metadata, title_priority))
}

/// Typed Activity-view bundle metadata for a synthetic Activity row.
///
/// `Some` only for synthetic Activity bundles, carrying the ORIGINAL top-level
/// member count (`member_nodes.len()`, never the flattened
/// metadata-subop count) so the viewer can render faithful bundle labels from
/// structured data without parsing the summary string. Inner execute/plan
/// bundles keep this metadata when nested beneath a work group. `None` for
/// ordinary rows and expandable Activity bundles.
#[must_use]
fn node_activity_bundle(
    node: &editchain_project::HistoryNode,
) -> Option<editchain_protocol::ActivityBundleDto> {
    match node {
        editchain_project::HistoryNode::WorkGroup { .. } => {
            Some(editchain_protocol::ActivityBundleDto {
                kind: editchain_protocol::ActivityBundleKind::WorkGroup,
                member_count: u64::try_from(node.represented_activity_count()).unwrap_or(u64::MAX),
            })
        }
        editchain_project::HistoryNode::ExecuteBundle { member_nodes, .. } => {
            Some(editchain_protocol::ActivityBundleDto {
                kind: editchain_protocol::ActivityBundleKind::ExecuteRun,
                member_count: u64::try_from(member_nodes.len()).unwrap_or(u64::MAX),
            })
        }
        editchain_project::HistoryNode::PlanBundle { member_nodes, .. } => {
            Some(editchain_protocol::ActivityBundleDto {
                kind: editchain_protocol::ActivityBundleKind::PlanRepeat,
                member_count: u64::try_from(member_nodes.len()).unwrap_or(u64::MAX),
            })
        }
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::CollapsedImport { .. }
        | editchain_project::HistoryNode::GitCommit { .. } => None,
    }
}

fn row_content_dto(content: editchain_project::content::DisplayContent) -> RowContentDto {
    RowContentDto {
        tool_label: content
            .tool_label
            .map(|text| ContentTextDto::tool_label(text.text, text.complete)),
        authored_summary: content
            .authored_summary
            .map(|text| ContentTextDto::new(text.text, text.complete)),
        output_preview: content
            .output_preview
            .map(|text| ContentTextDto::new(text.text, text.complete)),
    }
}

fn child_content_dto(op: Option<&Op>, summary: &str) -> RowContentDto {
    let content = op
        .filter(|op| matches!(op.kind, OpKind::Tool(_)))
        .map_or_else(
            || editchain_project::content::DisplayContent::summary(summary.to_owned()),
            |op| editchain_project::content::operation(op, false).display,
        );
    row_content_dto(content)
}

struct ServicePresentation<'a> {
    agent_changes: &'a HashMap<OpId, Vec<FileChangeDto>>,
    git_changes: &'a HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>>,
}

impl ActivityPresentation for ServicePresentation<'_> {
    type Row = ExpandedChildRow;

    fn activity(&self, member: &editchain_project::HistoryNode) -> ExpandedChildRow {
        ExpandedChildRow {
            op_id: member.op_id().map_or_else(String::new, |id| id.to_string()),
            git_oid: member.git_oid().map(|oid| oid.to_hex()),
            repository: member.repository().map(|id| id.0.to_string()),
            summary: ContentTextDto::new(member.summary(), false).text,
            content: row_content_dto(member.display_content()),
            timestamp_ms: member.timestamp_ms(),
            kind: member.kind(),
            author: ContentTextDto::new(node_author(member), false).text,
            commit_id: node_commit_id(member),
            is_system: node_is_system(member),
            record_role: member.record_role(),
            activity_kind: member.activity_kind(),
            visibility: member.visibility(),
            outcome: member.outcome(),
            chain_state: member.chain_state(),
            turn_id: member.turn_id().map(|id| id.0.to_string()),
            promoted: matches!(
                member.outcome(),
                Outcome::Warning | Outcome::Failure | Outcome::Cancelled
            ) || matches!(
                member.activity_kind(),
                ActivityKind::Change | ActivityKind::Verify
            ),
            activity_bundle: node_activity_bundle(member),
            file_change: None,
        }
    }

    fn details(&self, node: &editchain_project::HistoryNode) -> Vec<ExpandedChildRow> {
        let mut rows = flat_op_child_rows(node);
        let changes = node_file_changes(node, self.agent_changes, self.git_changes);
        rows.extend(file_change_rows(&changes, node));
        rows
    }
}

impl ExpandedChildRow {
    fn summary_dto(&self) -> SubOpSummary {
        SubOpSummary {
            op_id: self.op_id.clone(),
            summary: self.summary.clone(),
            kind: self.kind.clone(),
            timestamp_ms: self.timestamp_ms,
        }
    }
}

fn expansion_span_dto(span: editchain_project::activity_view::ExpansionSpan) -> ExpansionSpanDto {
    ExpansionSpanDto {
        row: u64::try_from(span.row).unwrap_or(u64::MAX),
        descendant_count: u64::try_from(span.descendant_count).unwrap_or(u64::MAX),
    }
}

/// File changes represented by one row, recursively collecting synthetic
/// bundle members while keeping ordinary rows and Git commits direct.
fn node_file_changes(
    node: &editchain_project::HistoryNode,
    agent_changes: &HashMap<OpId, Vec<FileChangeDto>>,
    git_changes: &HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>>,
) -> Vec<FileChangeDto> {
    match node {
        editchain_project::HistoryNode::EditOperation { op, .. }
        | editchain_project::HistoryNode::CollapsedImport { op, .. } => {
            agent_changes.get(&op.id).cloned().unwrap_or_default()
        }
        editchain_project::HistoryNode::GitCommit { commit, .. } => git_changes
            .get(&(commit.repository, commit.oid))
            .cloned()
            .unwrap_or_default(),
        editchain_project::HistoryNode::ExecuteBundle { member_nodes, .. }
        | editchain_project::HistoryNode::PlanBundle { member_nodes, .. }
        | editchain_project::HistoryNode::WorkGroup { member_nodes, .. } => member_nodes
            .iter()
            .flat_map(|member| node_file_changes(member, agent_changes, git_changes))
            .collect(),
    }
}

fn file_change_rows(
    changes: &[FileChangeDto],
    node: &editchain_project::HistoryNode,
) -> Vec<ExpandedChildRow> {
    changes
        .iter()
        .cloned()
        .map(|change| ExpandedChildRow {
            op_id: change.op_id.clone().unwrap_or_default(),
            git_oid: change.commit_oid.clone(),
            repository: change.repository.clone(),
            summary: ContentTextDto::new(change.path.clone(), true).text,
            content: row_content_dto(editchain_project::content::DisplayContent::summary(
                change.path.clone(),
            )),
            timestamp_ms: node.timestamp_ms(),
            kind: "file".to_string(),
            author: String::new(),
            commit_id: String::new(),
            is_system: false,
            record_role: RecordRole::Artifact,
            activity_kind: ActivityKind::Change,
            visibility: Visibility::Supporting,
            outcome: Outcome::Unknown,
            chain_state: node.chain_state(),
            turn_id: node.turn_id().map(|id| id.0.to_string()),
            promoted: false,
            activity_bundle: None,
            file_change: Some(change),
        })
        .collect()
}

/// Established flat detail/member rows for one ordinary or inner bundle node.
#[must_use]
fn flat_op_child_rows(node: &editchain_project::HistoryNode) -> Vec<ExpandedChildRow> {
    let summaries = node_sub_op_summaries(node);
    let member_meta = node_sub_op_meta_index(node);
    summaries
        .into_iter()
        .enumerate()
        .map(|(index, summary)| {
            let (record_role, activity_kind) = node
                .sub_ops()
                .get(index)
                .map_or((RecordRole::Unknown, ActivityKind::Unknown), |op| {
                    node_sub_op_meta(op.as_ref(), &member_meta)
                });
            ExpandedChildRow {
                op_id: summary.op_id,
                git_oid: None,
                repository: None,
                content: child_content_dto(
                    node.sub_ops().get(index).map(AsRef::as_ref),
                    &summary.summary,
                ),
                summary: ContentTextDto::new(summary.summary, false).text,
                timestamp_ms: summary.timestamp_ms,
                kind: summary.kind,
                author: String::new(),
                commit_id: String::new(),
                is_system: true,
                record_role,
                activity_kind,
                visibility: Visibility::Supporting,
                outcome: Outcome::Unknown,
                chain_state: node.chain_state(),
                turn_id: node.turn_id().map(|id| id.0.to_string()),
                promoted: false,
                activity_bundle: None,
                file_change: None,
            }
        })
        .collect()
}

/// Build the expanded sub-op summaries for a top-level node.
///
/// Activity bundles expose their folded member rows, so each member renders
/// with its ORIGINAL row's summary/kind (faithful labels) while the member's
/// own metadata sub-ops keep the generic op-derived labels. All other nodes
/// use the generic op-derived path unchanged.
#[must_use]
fn node_sub_op_summaries(node: &editchain_project::HistoryNode) -> Vec<SubOpSummary> {
    if let editchain_project::HistoryNode::ExecuteBundle {
        member_nodes,
        members,
        ..
    }
    | editchain_project::HistoryNode::PlanBundle {
        member_nodes,
        members,
        ..
    } = node
    {
        member_sub_op_summaries(members, member_nodes)
    } else {
        sub_op_summaries(node.sub_ops())
    }
}

/// Sub-op summaries for one bundle's flattened member ops.
///
/// Each entry is the member's anchor op (labels come from the original row) or
/// one of the member's own bundled metadata ops (generic labels), preserving
/// reveal order.
#[must_use]
fn member_sub_op_summaries(
    members: &[std::sync::Arc<Op>],
    member_nodes: &[editchain_project::HistoryNode],
) -> Vec<SubOpSummary> {
    let key_to_node: HashMap<String, &editchain_project::HistoryNode> = member_nodes
        .iter()
        .map(|node| (node.node_key(), node))
        .collect();
    members
        .iter()
        .map(|op| {
            let op_key = op.id.to_string();
            if let Some(member) = key_to_node.get(&op_key) {
                SubOpSummary {
                    op_id: op_key,
                    summary: ContentTextDto::new(member.summary(), false).text,
                    kind: member.kind(),
                    timestamp_ms: op.observed_unix_ms().unwrap_or(0),
                }
            } else {
                let (summary, kind) = sub_op_label(op);
                SubOpSummary {
                    op_id: op_key,
                    summary,
                    kind,
                    timestamp_ms: op.observed_unix_ms().unwrap_or(0),
                }
            }
        })
        .collect()
}

/// Per-bundle op-key -> member metadata index for expanded sub-op rows.
///
/// Built once per bundle node; entries cover only the folded member rows (their
/// own metadata sub-ops fall through to the generic op-derived classifier).
#[must_use]
fn node_sub_op_meta_index(
    node: &editchain_project::HistoryNode,
) -> HashMap<String, (RecordRole, ActivityKind)> {
    match node {
        editchain_project::HistoryNode::ExecuteBundle { member_nodes, .. }
        | editchain_project::HistoryNode::PlanBundle { member_nodes, .. } => member_nodes
            .iter()
            .map(|member| {
                (
                    member.node_key(),
                    (member.record_role(), member.activity_kind()),
                )
            })
            .collect(),
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::CollapsedImport { .. }
        | editchain_project::HistoryNode::WorkGroup { .. }
        | editchain_project::HistoryNode::GitCommit { .. } => HashMap::new(),
    }
}

/// Semantic role/activity for one expanded sub-op row, using the bundle's
/// precomputed member metadata when the op is a folded member row.
#[must_use]
fn node_sub_op_meta(
    op: &Op,
    member_meta: &HashMap<String, (RecordRole, ActivityKind)>,
) -> (RecordRole, ActivityKind) {
    member_meta
        .get(&op.id.to_string())
        .copied()
        .unwrap_or_else(|| sub_op_meta(op))
}

/// Map the projection's provider-neutral relation kind to the protocol enum.
///
/// The projection derives kinds from exact spawn, reconnect, fork, and
/// produced-commit facts; `Unknown` remains the forward-compatible fallback.
#[must_use]
fn protocol_relation_kind(kind: editchain_project::RelationKind) -> ParentRelationKind {
    match kind {
        editchain_project::RelationKind::Subagent => ParentRelationKind::Subagent,
        editchain_project::RelationKind::Reconnect => ParentRelationKind::Reconnect,
        editchain_project::RelationKind::Fork => ParentRelationKind::Fork,
        editchain_project::RelationKind::ProducedCommit => ParentRelationKind::ProducedCommit,
    }
}

/// Derive a display label for a bundled sub-op.
///
/// Metadata sub-ops are raw Import ops — parse the JSONL record type. Tool-result
/// sub-ops are `Tool` ops with `stage: Finish` — render a content preview.
#[must_use]
fn sub_op_label(op: &Op) -> (String, String) {
    // A tool-result sub-op (grouped under its tool call): show a content preview.
    if let OpKind::Tool(t) = &op.kind {
        if matches!(t.stage, editchain_core::op::ToolStage::Finish) {
            let preview = tool_result_preview(&payload_text(&t.content));
            return (preview, "tool_result".to_string());
        }
    }
    let raw = match &op.kind {
        OpKind::Import(i) => match &i.raw_ref {
            Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
            Payload::Empty | Payload::Blob(_) => String::new(),
        },
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::Tool(_)
        | OpKind::Command(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => String::new(),
    };
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(record_type) = value.get("type").and_then(serde_json::Value::as_str) {
            let token_kind = if record_type == "token_usage_record" {
                Some("token_usage_record")
            } else if record_type == "event_msg"
                && value
                    .get("payload")
                    .and_then(|payload| payload.get("type"))
                    .and_then(serde_json::Value::as_str)
                    == Some("token_count")
            {
                Some("token_count")
            } else {
                None
            };
            if let (Some(kind), Some(summary)) = (
                token_kind,
                editchain_project::import_token_accounting_summary(raw.as_bytes()),
            ) {
                return (summary, kind.to_string());
            }
            let label = if record_type == "assistant" {
                value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(serde_json::Value::as_array)
                    .and_then(|content| content.first())
                    .and_then(|block| {
                        let kind = block.get("type").and_then(serde_json::Value::as_str)?;
                        match kind {
                            "tool_use" => block
                                .get("name")
                                .and_then(serde_json::Value::as_str)
                                .filter(|name| !name.is_empty())
                                .map(|name| format!("tool: {name}")),
                            "text" => block
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .filter(|text| !text.trim().is_empty())
                                .map(tool_result_preview),
                            "thinking" => Some("thinking".to_string()),
                            other if !other.is_empty() => Some(other.to_string()),
                            _ => None,
                        }
                    })
                    .unwrap_or_else(|| record_type.to_string())
            } else if record_type == "event_msg" {
                value
                    .get("payload")
                    .and_then(|payload| payload.get("type"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|event_type| !event_type.is_empty())
                    .unwrap_or(record_type)
                    .to_string()
            } else {
                record_type.to_string()
            };
            let kind = if record_type == "assistant" && label.starts_with("tool:") {
                "tool".to_string()
            } else {
                label.clone()
            };
            return (label, kind);
        }
    }
    (raw, "meta".to_string())
}

/// Map a sub-op's kind tag to a coarse semantic class used to pick its icon.
///
/// The class is intentionally coarse (a handful of buckets) so the client can
/// map it to a small set of Codicons without enumerating every record type.
#[must_use]
fn subop_semantic_class(kind: &str) -> String {
    match kind {
        "tool_result" => "tool_result".to_string(),
        // File-history snapshots and file edits are "edit"-like records.
        "file-history-snapshot" | "edited_text_file" | "file" | "opened_file_in_ide" => {
            "edit".to_string()
        }
        // User-facing text records.
        "message" | "command" | "last-prompt" => "msg".to_string(),
        // Everything else is metadata (mode, permission-mode, custom-title,
        // agent-name, telemetry system subtypes, etc.).
        _ => "meta".to_string(),
    }
}

/// Intersect two sorted lane lists, returning the shared lanes in order.
///
/// Used to find which lanes pass straight through a sub-op region: a lane with a
/// vertical line leaving the parent downward AND entering the next node from
/// above spans the whole region continuously.
#[must_use]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "two-pointer intersection; indices are bounds-checked by the loop condition"
)]
fn intersect_sorted(a: &[usize], b: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Produce a pretty-printed, truncated preview of a tool result's content.
///
/// Strips leading `<digits>\t` line-number prefixes, collapses to the first
/// non-empty line, and truncates to ~1024 chars. Mirrors the projection's
/// `tool_result_summary` so sub-op previews match the main-pane summaries.
#[must_use]
fn tool_result_preview(content: &str) -> String {
    const MAX: usize = 1024;
    let stripped: String = content
        .lines()
        .map(|l| {
            let trimmed = l.trim_start();
            let after_digits = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
            after_digits.strip_prefix('\t').map_or(l, |rest| rest)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut line = stripped
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    if line.chars().count() > MAX {
        let mut cut = line.chars().take(MAX).collect::<String>();
        cut.push('…');
        line = cut;
    }
    line
}

/// Derive a short author label from an op's tags.
///
/// Prefers the actor role tags (`HUMAN` / `AGENT`); falls back to `system` for
/// anything else (imports, tools, commands, etc.).
#[must_use]
fn op_author_label(tags: Tags) -> String {
    if tags.matches_any(Tags::HUMAN) {
        "human".to_string()
    } else if tags.matches_any(Tags::AGENT) {
        "agent".to_string()
    } else {
        "system".to_string()
    }
}

/// Commit/ID display value for a history node.
///
/// Git commits show an abbreviated OID; `EditChain` ops show an abbreviated
/// op id (`node:seq`, dropping the boot counter) so both row types read as a
/// short, uniform identifier in this column.
#[must_use]
fn node_commit_id(node: &editchain_project::HistoryNode) -> String {
    match node {
        editchain_project::HistoryNode::EditOperation { op, .. }
        | editchain_project::HistoryNode::CollapsedImport { op, .. } => abbreviate_op_id(&op.id),
        editchain_project::HistoryNode::ExecuteBundle { anchor, .. }
        | editchain_project::HistoryNode::PlanBundle { anchor, .. }
        | editchain_project::HistoryNode::WorkGroup { anchor, .. } => abbreviate_op_id(&anchor.id),
        editchain_project::HistoryNode::GitCommit { commit, .. } => abbreviate_oid(&commit.oid),
    }
}

/// Abbreviate an op id (`node:boot:seq`) to a short `node:seq` form.
///
/// The boot counter is almost always 0 and adds noise; dropping it keeps the
/// column compact while preserving the distinguishing sequence number.
#[must_use]
fn abbreviate_op_id(id: &OpId) -> String {
    format!("{}:{}", id.node.0, id.seq)
}

/// Abbreviate a git OID to its first 7 hex characters.
#[must_use]
fn abbreviate_oid(oid: &GitOid) -> String {
    let hex = oid.to_hex();
    hex.chars().take(7).collect()
}

/// Build node details from an `EditChain` operation.
#[must_use]
fn node_details_from_op(op: &Op) -> NodeDetails {
    NodeDetails {
        op_id: Some(op.id.to_string()),
        git_oid: None,
        repository: None,
        summary: op_summary(op),
        body: op_body(op),
        parents: op.parents.iter().map(ToString::to_string).collect(),
        git_parents: Vec::new(),
        refs: Vec::new(),
        changed_paths: Vec::new(),
    }
}

/// Build node details from a git commit entity.
#[must_use]
fn node_details_from_commit(commit: &editchain_core::GitCommitEntity) -> NodeDetails {
    let body = match &commit.message {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    };
    let refs = commit
        .live_refs
        .iter()
        .chain(commit.imported_refs.iter())
        .filter_map(|r| match r {
            Payload::Inline(b) => Some(String::from_utf8_lossy(b).to_string()),
            Payload::Empty | Payload::Blob(_) => None,
        })
        .collect();
    let changed_paths = commit
        .changed_paths
        .iter()
        .map(|p| p.0.to_string())
        .collect();
    NodeDetails {
        op_id: commit.imported_record.map(|id| id.to_string()),
        git_oid: Some(commit.oid.to_hex()),
        repository: Some(commit.repository.0.to_string()),
        summary: body.clone(),
        body,
        parents: Vec::new(),
        git_parents: commit.parents.iter().map(GitOid::to_hex).collect(),
        refs,
        changed_paths,
    }
}

/// Build the JSON-safe resolved-object DTO from a git commit entity.
///
/// Every identity is carried as an exact string (decimal `RepositoryId` /
/// `PathId`, lowercase-hex `GitOid`, `"node:boot:seq"` `OpId`) so u64 values
/// above 2^53 round-trip through JavaScript without precision loss. Safe
/// enums, timestamps, signatures, and payloads are preserved as-is.
#[must_use]
pub(crate) fn resolved_object_from_commit(
    commit: &editchain_core::GitCommitEntity,
) -> ResolvedObject {
    ResolvedObject {
        repository: commit.repository.0.to_string(),
        object_format: commit.object_format,
        oid: commit.oid.to_hex(),
        imported_record: commit.imported_record.map(|id| id.to_string()),
        availability: commit.availability,
        tree: commit.tree.to_hex(),
        parents: commit.parents.iter().map(GitOid::to_hex).collect(),
        author: commit.author.clone(),
        committer: commit.committer.clone(),
        authored_at: commit.authored_at,
        committed_at: commit.committed_at,
        message: commit.message.clone(),
        imported_refs: commit.imported_refs.clone(),
        live_refs: commit.live_refs.clone(),
        changed_paths: commit
            .changed_paths
            .iter()
            .map(|p| p.0.to_string())
            .collect(),
    }
}

/// Result of [`read_chain_ops`]: accepted ops plus canonicalization stats.
type ChainReadResult =
    Result<(Vec<Op>, ChainReadStats, Vec<SnapshotOpLocator>), Box<dyn std::error::Error>>;

/// Read canonical operations and their durable detail locations through the
/// shared store. Conflicted IDs are absent from every consumer's valid corpus.
fn read_chain_ops(chain_dir: &Path) -> ChainReadResult {
    let chain = CanonicalChain::read(chain_dir)?;
    let stats = chain.stats();
    let mut ops = Vec::with_capacity(stats.accepted);
    let mut locations = Vec::with_capacity(stats.accepted);
    for (op, location) in chain.into_located_ops() {
        let location = location.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "stored operation has no location",
            )
        })?;
        locations.push(SnapshotOpLocator {
            id: op.id,
            location,
        });
        ops.push(op);
    }
    Ok((ops, stats, locations))
}

fn read_op_at(
    chain_dir: &Path,
    location: OpRecordLocation,
) -> Result<Op, Box<dyn std::error::Error>> {
    editchain_store::read_op_at(chain_dir, location).map_err(Into::into)
}

/// Resolve a git commit by OID in a discovered repository.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or the object cannot
/// be resolved.
pub fn resolve_git_commit(
    workspace: &Workspace,
    repository_id: RepositoryId,
    oid: &GitOid,
) -> Result<Option<editchain_core::GitCommitEntity>, Box<dyn std::error::Error>> {
    let Some(discovery) = workspace
        .repositories
        .iter()
        .find(|d| d.id == repository_id)
    else {
        return Ok(None);
    };
    let handle = open_repository(discovery)?;
    match resolve_commit(&handle, oid) {
        Ok(commit) => Ok(Some(commit)),
        Err(editchain_git::ResolutionError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Resolve exact durable Git-link targets that the current HEAD walk did not
/// include (for example, a session started on a branch that was later switched).
fn merge_exact_git_link_targets(
    projection: &mut HistoryProjection,
    repositories: &[editchain_git::RepositoryDiscovery],
) -> usize {
    let targets: std::collections::BTreeSet<(RepositoryId, GitOid)> = projection
        .git()
        .links()
        .values()
        .flatten()
        .map(|link| (link.target_repo, link.target_oid))
        .filter(|target| !projection.git().commits().contains_key(target))
        .collect();

    let mut unresolved = targets.len();
    for discovery in repositories {
        let repository_targets: Vec<GitOid> = targets
            .iter()
            .filter_map(|(repository, oid)| (*repository == discovery.id).then_some(*oid))
            .collect();
        if repository_targets.is_empty() {
            continue;
        }
        let Ok(handle) = open_repository(discovery) else {
            continue;
        };
        let commits: Vec<_> = repository_targets
            .iter()
            .filter_map(|oid| resolve_commit(&handle, oid).ok())
            .collect();
        unresolved = unresolved.saturating_sub(commits.len());
        projection.merge_git_commits(commits);
    }
    unresolved
}

pub(crate) fn stale_snapshot() -> ServiceError {
    ServiceError::new(
        ErrorCode::StaleSnapshot,
        "History sources changed. Reopen history to refresh this view.",
    )
}

fn unique_snapshot_id(prefix: &str) -> SnapshotId {
    static NEXT_SNAPSHOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let serial = NEXT_SNAPSHOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    SnapshotId::new(format!("{prefix}:{}:{serial}", std::process::id()))
}

/// Produce a short summary for an `EditChain` operation.
#[must_use]
fn op_summary(op: &Op) -> String {
    match &op.kind {
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.tool_name),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::File(f) => format!("file:{}", f.path.0),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(cs) => String::from_utf8_lossy(&cs.name).to_string(),
        OpKind::Actor(a) => payload_text(&a.label),
        OpKind::Import(i) => payload_text(&i.raw_ref),
        OpKind::GitCommit(c) => payload_text(&c.message),
        OpKind::GitLink(l) => format!("git:{}", l.target_oid),
        OpKind::Unknown(u) => format!("unknown kind={}", u.kind_discriminant),
    }
}

/// Produce the full body text for an `EditChain` operation.
#[must_use]
fn op_body(op: &Op) -> String {
    match &op.kind {
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.content),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::File(_)
        | OpKind::Import(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => String::new(),
    }
}

/// Extract text from a payload, or empty string.
#[must_use]
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "Tests index into vectors whose length is asserted immediately before"
)]
mod tests;
