//! Native Rust stdio service for the `EditChain` VS Code extension.
//!
//! Reads framed requests from stdin, dispatches them against the chain reader,
//! git resolver, and unified search, and writes framed responses to stdout.

#[cfg(test)]
use tempfile as _;

mod snapshot;

pub use snapshot::RenderSnapshotReport;

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_import as _;
use editchain_query as _;
use serde as _;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use editchain_codec::frame::decode_op;
use editchain_codec::page::PAGE_MAGIC;
use editchain_core::{
    ActorId, BlobRef, Clock, ContentId, GitOid, NodeId, Op, OpId, OpKind, OpSet, ParentSet,
    Payload, RepositoryId, ScopeRef, SessionId, Tags,
};
use editchain_git::{
    commit_file_changes, discover_repositories, resolve_blob as resolve_git_blob, resolve_commit,
    resolve_path_at_commit, walk_history, GitFileChange, GitFileStatus, RepositoryHandle,
};
use editchain_import::{hash_raw, FsBlobSink};
use editchain_index::LexicalIndex;
use editchain_project::activity::{ActivityRowAnnotation, SessionSummaryMarker, WorkUnitMarker};
use editchain_project::filter::ChainFilter;
use editchain_project::taxonomy::{ActivityKind, ChainState, Outcome, RecordRole, Visibility};
use editchain_project::HistoryProjection;
use editchain_protocol::{
    ChainFilterDto, ExpansionSpanDto, FileChangeDto, FileChangeSource, FileChangeStatus,
    FileDiffDto, FindInHistoryMatch, FindInHistoryResponse, GraphLayout as ProtocolGraphLayout,
    HistoryRow, HistoryWindow, LayoutEdge, LayoutPoint, LayoutRow, NodeDetails, ParentRelationDto,
    ParentRelationKind, RepositoryInfo, Request, RequestBody, ResolvedObject, Response,
    ResponseBody, SearchFiltersDto, SearchHit, SearchResponse, SessionMetaDto, SessionSummaryDto,
    SubOpSummary, WorkUnitDto,
};
use editchain_query::search::{ScoredChunk, SearchFilters, Source};

use snapshot::{RenderSnapshot, SnapshotBuilder, SnapshotIdentity, SnapshotManifestData};

/// A loaded workspace: chain ops + git repositories.
#[derive(Debug)]
pub struct Workspace {
    /// The unified history projection.
    pub projection: HistoryProjection,
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
    pub repositories: Vec<editchain_git::RepositoryDiscovery>,
    /// Diagnostics for this open: chain canonicalization and bounded blob
    /// preview/deferred-hydration outcomes.
    pub diagnostics: OpenDiagnostics,
    /// Absolute workspace root used if a non-default request must lazily load
    /// the complete projection after a snapshot-backed Open.
    root_path: PathBuf,
    /// Absolute authoritative chain directory.
    chain_path: PathBuf,
    /// Valid immutable render snapshot for the fixed default view, if present.
    snapshot: Option<RenderSnapshot>,
    /// Whether `projection`/`source_ops` contain the authoritative live model.
    projection_loaded: bool,
    /// The single currently cached filtered snapshot, keyed by
    /// `(hide_submodules, filter)`.
    ///
    /// The extension keeps one active view at a time, so the cache holds a
    /// single O(V) snapshot: window and layout paging reuse it for the same
    /// filter key, and switching filters rebuilds it in place. Arbitrary
    /// regex/filter churn therefore never accumulates unbounded snapshots.
    current_view: Option<(ViewKey, ViewSnapshot)>,
}

/// Cache key for one filtered history snapshot.
type ViewKey = (bool, editchain_project::filter::ChainFilterKey);

/// Parameters for one history-window read.
#[derive(Debug, Clone, Copy)]
pub struct HistoryWindowOptions<'a> {
    /// Expanded-row offset (zero is newest).
    pub offset: u64,
    /// Maximum expanded rows to return.
    pub limit: u64,
    /// Exclude rows from nested repositories.
    pub hide_submodules: bool,
    /// Active graph/content filter.
    pub filter: &'a ChainFilter,
    /// Compute and attach global lane geometry before returning.
    pub include_layout: bool,
}

/// Immutable per-filter projection consumed by both history and layout paging.
#[derive(Debug)]
struct ViewSnapshot {
    /// Canonical top-level rows in display order.
    nodes: Vec<editchain_project::HistoryNode>,
    /// Per-row Activity-view annotations (work-unit markers + promotion),
    /// parallel to `nodes` 1:1.
    annotations: Vec<ActivityRowAnnotation>,
    /// Graph geometry over `nodes`, built only after the first row window has
    /// painted. `None` is a valid provisional row-only snapshot.
    context: Option<editchain_project::layout::LayoutContext>,
    /// Total depth-first descendant count attached to each top-level row.
    sub_op_counts: Vec<usize>,
    /// Precomputed depth-first descendants for each top-level row. Existing
    /// bundles nested in a work group retain their own direct children.
    expansions: Vec<NodeExpansion>,
    /// Global absolute expandable intervals, shipped once with offset zero.
    expansion_spans: Vec<ExpansionSpanDto>,
    /// Expanded absolute slot where each top-level row starts, plus a sentinel.
    starts: Vec<usize>,
    /// Total number of fully expanded slots.
    expanded_total: usize,
    /// Maximum graph lane in this view.
    max_lane: usize,
    /// Cached op id → visible top-level row index for every op represented by
    /// this snapshot: each row's own op id (or bundle anchor) plus every
    /// bundled sub-op/member op id. Built lazily on the first Find-in-Chain
    /// request so arrow-key navigation never rebuilds an O(V) map per keypress.
    op_rows: Option<HashMap<OpId, usize>>,
    /// Cached git `(repository, oid)` → visible top-level row index for the
    /// git commits in this snapshot (submodule rows are absent when the view
    /// hides them). Built lazily together with [`Self::op_rows`].
    git_rows: Option<HashMap<(RepositoryId, GitOid), usize>>,
}

/// One top-level row's fully expanded, depth-first presentation descendants.
#[derive(Debug)]
struct NodeExpansion {
    /// Direct children advertised on the top-level row for disclosure chrome.
    direct: Vec<SubOpSummary>,
    /// All descendants in stable depth-first slot order.
    rows: Vec<ExpandedChildRow>,
}

/// One nested presentation row. It is not a graph node; `parent_relative`
/// points at its direct parent within the enclosing top-level block (`0` is
/// the top-level row, descendants start at relative slot `1`).
#[derive(Debug)]
struct ExpandedChildRow {
    op_id: String,
    git_oid: Option<String>,
    repository: Option<String>,
    summary: String,
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
    direct: Vec<SubOpSummary>,
    parent_relative: usize,
    depth: u8,
    descendant_count: usize,
}

/// Canonicalization outcome for the records decoded from a chain's segments.
///
/// Records are admitted through [`OpSet`], which ignores exact replays of an
/// accepted op and quarantines same-id records with conflicting bytes, so a
/// crash-replayed import page never double-counts or silently mutates an op.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct ChainReadStats {
    /// Successfully decoded records handed to the `OpSet`.
    pub records: usize,
    /// Unique operations accepted into the workspace.
    pub accepted: usize,
    /// Exact replay records ignored (same `OpId`, same bytes).
    pub duplicates: usize,
    /// Conflicting records quarantined (same `OpId`, different bytes).
    pub quarantined: usize,
}

/// Exact location of one encoded operation inside an append-only segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpRecordLocation {
    /// Numeric sequence from `<sequence>.eclog`.
    pub(crate) segment_seq: u32,
    /// Absolute byte offset of the encoded operation (after length + flags).
    pub(crate) data_offset: u64,
    /// Encoded operation length in bytes.
    pub(crate) data_len: u32,
}

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
}

impl OpenDiagnostics {
    /// Human-readable warnings for integrity gaps discovered during open.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
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

/// The outcome of resolving one blob reference against the durable store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobResolution {
    /// Verified content found; carries the bytes to hydrate inline.
    Found(Vec<u8>),
    /// No blob file exists for the reference.
    Missing,
    /// A blob file exists but failed declared-length or BLAKE3 validation.
    Corrupt,
    /// The reference cannot be addressed by the durable store.
    Unresolvable,
}

/// Read-only resolver over a chain's durable blob store.
///
/// Lookup and filename derivation are delegated to [`FsBlobSink`] so the
/// `<chain>/blobs/<lowercase blake3 hex>` naming convention stays in exactly
/// one place. Full resolution validates declared length and BLAKE3; bounded
/// row previews validate file length and defer full hashing until content is
/// explicitly requested.
#[derive(Debug, Clone)]
pub struct BlobResolver {
    /// The durable store; `None` when the chain has no `blobs/` directory.
    sink: Option<FsBlobSink>,
}

impl BlobResolver {
    /// Open the blob store for a chain directory without creating it.
    ///
    /// # Errors
    ///
    /// Returns an IO error if `chain_dir/blobs` exists but cannot be read.
    pub fn open(chain_dir: &Path) -> io::Result<Self> {
        Ok(Self {
            sink: FsBlobSink::open_read_only(chain_dir.join("blobs"))?,
        })
    }

    /// Resolve a blob reference, validating declared length and hash.
    ///
    /// Non-`Found` outcomes leave the caller's [`BlobRef`] untouched so legacy
    /// chains with missing or corrupt blobs still open.
    #[must_use]
    pub fn resolve(&self, blob: &BlobRef) -> BlobResolution {
        let Some(hash) = addressable_hash(blob.id) else {
            return BlobResolution::Unresolvable;
        };
        match self
            .sink
            .as_ref()
            .and_then(|sink| sink.get(&hash).transpose())
        {
            Some(Ok(bytes)) if blob_matches(&bytes, blob, hash) => BlobResolution::Found(bytes),
            Some(Ok(_) | Err(_)) => BlobResolution::Corrupt,
            None => BlobResolution::Missing,
        }
    }

    /// Resolve a full content-addressed payload when only its `ContentId` is
    /// stored (as with `FileOp.base` / `FileOp.after`).
    #[must_use]
    fn resolve_content(&self, id: ContentId) -> Option<Vec<u8>> {
        let hash = addressable_hash(id)?;
        let bytes = self.sink.as_ref()?.get(&hash).ok().flatten()?;
        (hash_raw(&bytes) == hash).then_some(bytes)
    }

    /// Read at most `limit` bytes for a display preview without hydrating or
    /// hashing the complete payload.
    ///
    /// File length is checked against the reference up front. Full BLAKE3
    /// validation remains deferred to [`Self::resolve`] when details/search
    /// actually request the complete payload.
    #[must_use]
    fn preview(&self, blob: &BlobRef, limit: usize) -> BlobPreviewResolution {
        let Some(hash) = addressable_hash(blob.id) else {
            return BlobPreviewResolution::Unresolvable;
        };
        let Some(sink) = self.sink.as_ref() else {
            return BlobPreviewResolution::Missing;
        };
        let path = sink.path_for(&hash);
        let Ok(metadata) = fs::metadata(&path) else {
            return if path.exists() {
                BlobPreviewResolution::Corrupt
            } else {
                BlobPreviewResolution::Missing
            };
        };
        if metadata.len() != u64::from(blob.len) {
            return BlobPreviewResolution::Corrupt;
        }
        let Ok(file) = File::open(path) else {
            return BlobPreviewResolution::Corrupt;
        };
        let mut bytes = Vec::with_capacity(limit);
        let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
        if file.take(limit_u64).read_to_end(&mut bytes).is_err() {
            return BlobPreviewResolution::Corrupt;
        }
        BlobPreviewResolution::Found(bytes)
    }
}

/// Outcome of a bounded, length-checked preview read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlobPreviewResolution {
    /// Prefix bytes found (possibly the complete short blob).
    Found(Vec<u8>),
    /// No blob file exists for the reference.
    Missing,
    /// The file exists but its metadata/read failed validation.
    Corrupt,
    /// The reference cannot be addressed by the durable store.
    Unresolvable,
}

/// The full BLAKE3 hash the durable store can address, if the id uses one.
///
/// `FsBlobSink` keys files by the full 256-bit BLAKE3 hash, so truncated
/// `Hash128` and node-local ids cannot be looked up and count as unresolved.
#[must_use]
fn addressable_hash(id: ContentId) -> Option<[u8; 32]> {
    match id {
        ContentId::Hash256(hash) => Some(hash),
        ContentId::Hash128(_) | ContentId::Local { .. } => None,
    }
}

/// Whether `bytes` match a blob reference's declared length and BLAKE3 hash.
#[must_use]
fn blob_matches(bytes: &[u8], blob: &BlobRef, hash: [u8; 32]) -> bool {
    match usize::try_from(blob.len) {
        Ok(declared) => declared == bytes.len() && hash_raw(bytes) == hash,
        Err(_) => false,
    }
}

/// Hydrate every blob payload across a chain's decoded ops in place.
///
/// Only payloads whose declared length and BLAKE3 hash verify are replaced
/// with inline content; every other blob stays a [`Payload::Blob`] reference
/// and is counted in the returned stats. Blob references with no inline
/// representation (`FileEdit::Blob`) are validated and preserved unchanged.
/// Runs before projection and lexical indexing so summaries, details, and
/// search see the actual content.
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
) -> (Vec<Op>, BlobHydrationStats) {
    let mut stats = BlobHydrationStats::default();
    let ops = source_ops
        .iter()
        .map(|source| {
            let mut op = source.clone();
            compact_kind_for_projection(&mut op.kind, resolver, &mut stats);
            op
        })
        .collect();
    (ops, stats)
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
/// summary text (bounded), `payload.role`, `arguments`/`output` previews, a
/// bounded structural/content signal for tool-payload carriers
/// (`arguments`/`input`/`parameters`), and structured outcome evidence
/// (`status`, `exitCode`, `errorMessage` at `payload` or `payload.item`
/// level, plus the canonical three-line Codex execution-result header), and
/// Claude's interrupted-request identity/marker. Codex
/// token-usage records retain only their bounded identity strings and empty
/// usage-object markers so legacy metadata classification can validate the
/// complete schema without retaining accounting values.
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
        if let Some(event_type) = json_string_field(&raw, "type", payload_start) {
            drop(payload.insert(
                "type".to_string(),
                serde_json::Value::String(event_type.to_string()),
            ));
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
            let _: bool = copy_preview_string(&raw, item_abs, &mut item, "status");
            let _: bool = copy_preview_string(&raw, item_abs, &mut item, "errorMessage");
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
        if record_type == "token_usage_record" {
            for field in [
                "thread_id",
                "turn_id",
                "session_id",
                "root_turn_id",
                "response_id",
            ] {
                let _: bool = copy_bounded_field(payload, &mut compact_payload, field);
            }
            for field in ["usage", "turn_token_usage", "thread_token_usage"] {
                if payload.get(field).is_some_and(serde_json::Value::is_object) {
                    drop(compact_payload.insert(
                        field.to_string(),
                        serde_json::Value::Object(serde_json::Map::new()),
                    ));
                }
            }
        }
        copy_string_field(payload, &mut compact_payload, "type");
        copy_string_field(payload, &mut compact_payload, "role");
        echo_text_truncated |= copy_bounded_field(payload, &mut compact_payload, "message");
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "arguments");
        copy_structured_payload_field(payload, &mut compact_payload, "arguments");
        copy_structured_payload_field(payload, &mut compact_payload, "input");
        copy_structured_payload_field(payload, &mut compact_payload, "parameters");
        let _: bool = copy_bounded_field(payload, &mut compact_payload, "output");
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
            let _: bool = copy_bounded_field(item, &mut compact_item, "status");
            let _: bool = copy_bounded_field(item, &mut compact_item, "errorMessage");
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
        let Ok(handle) = open_repository_handle(discovery) else {
            continue;
        };
        for commit in projection
            .git
            .commits
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
    let repo_root = repository.path.parent().unwrap_or(&repository.path);
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd?.join(path)
    };
    absolute
        .strip_prefix(repo_root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .filter(|relative| !relative.is_empty())
}

impl Workspace {
    /// Create a workspace from an existing projection (used in tests).
    #[must_use]
    pub fn from_projection(projection: HistoryProjection) -> Self {
        let source_ops = projection.ops.clone();
        let session_metadata = session_metadata_index(&projection.ops);
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
            repositories: Vec::new(),
            diagnostics: OpenDiagnostics::default(),
            root_path: PathBuf::new(),
            chain_path: PathBuf::new(),
            snapshot: None,
            projection_loaded: true,
            current_view: None,
        }
    }

    /// Load a workspace from a chain directory and discover git repos.
    ///
    /// # Errors
    ///
    /// Returns an error if the chain cannot be read or repos cannot be discovered.
    pub fn open(workspace_path: &str, chain_dir: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Resolve the chain directory relative to the workspace when it is a
        // relative path (e.g. ".editchain"). The service process's CWD is not
        // necessarily the workspace root, so we must join explicitly.
        let chain_path = if PathBuf::from(chain_dir).is_absolute() {
            PathBuf::from(chain_dir)
        } else {
            PathBuf::from(workspace_path).join(chain_dir)
        };
        let workspace_path = PathBuf::from(workspace_path);
        let repositories = discover_repositories(&workspace_path)?;
        if let Ok(identity) = SnapshotIdentity::capture(&chain_path, &repositories) {
            if let Ok(Some(snapshot)) = RenderSnapshot::open(&chain_path, &identity) {
                let diagnostics = snapshot.diagnostics();
                return Ok(Self {
                    projection: HistoryProjection::from_ops_with(Vec::new(), projection_options()),
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
                    snapshot: Some(snapshot),
                    projection_loaded: false,
                    current_view: None,
                });
            }
        }
        Self::open_projection(workspace_path, chain_path, repositories)
    }

    /// Load the authoritative projection, bypassing any derived render cache.
    fn open_projection(
        workspace_path: PathBuf,
        chain_path: PathBuf,
        repositories: Vec<editchain_git::RepositoryDiscovery>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (source_ops, chain_stats, source_op_locations) = read_chain_ops(&chain_path)?;
        // Keep durable references in the canonical source corpus. The graph
        // projection receives only bounded display previews, preventing large
        // payload bytes from being multiplied by collapse/filter/layout clones.
        // Details and search hydrate a single source op at a time on demand.
        let resolver = BlobResolver::open(&chain_path)?;
        let (projection_ops, blob_stats) = projection_ops_with_previews(&source_ops, &resolver);
        let diagnostics = OpenDiagnostics {
            chain: chain_stats,
            blobs: blob_stats,
        };
        // q6 Phase-1: enable per-source-chain META bundling by default in the live
        // viewer. `ProjectionOptions` is passed explicitly so the behavior is
        // deterministic and a real cache key, never a process-global toggle.
        let mut projection = HistoryProjection::from_ops_with(projection_ops, projection_options());
        // Walk each discovered repo's history into the projection.
        for discovery in &repositories {
            let opened = open_repository_handle(discovery);
            let Ok(handle) = opened else {
                continue;
            };
            let walked = walk_history(&handle, 0);
            if let Ok(commits) = walked {
                projection.merge_git_commits(commits);
            }
        }
        // A session may have started on a commit that is no longer reachable
        // from the repository's current HEAD. Resolve only the exact OIDs
        // carried by durable GitLink ops; never guess from timestamps or text.
        merge_exact_git_link_targets(&mut projection, &repositories);
        let source_op_index = source_ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.id, index))
            .collect();
        let session_metadata = session_metadata_index(&projection.ops);
        let agent_file_changes =
            agent_file_change_index(&source_ops, &workspace_path, Some(&resolver), &repositories);
        let git_file_changes = git_file_change_index(&projection, &repositories);
        Ok(Self {
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
            snapshot: None,
            projection_loaded: true,
            current_view: None,
        })
    }

    /// Materialize the complete projection only for compatibility requests
    /// that cannot be served by the fixed-view snapshot.
    fn ensure_projection_loaded(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.projection_loaded {
            return Ok(());
        }
        let loaded = Self::open_projection(
            self.root_path.clone(),
            self.chain_path.clone(),
            self.repositories.clone(),
        )?;
        self.projection = loaded.projection;
        self.source_ops = loaded.source_ops;
        self.source_op_index = loaded.source_op_index;
        self.session_metadata = loaded.session_metadata;
        self.agent_file_changes = loaded.agent_file_changes;
        self.git_file_changes = loaded.git_file_changes;
        self.source_op_locations = loaded.source_op_locations;
        self.blob_resolver = loaded.blob_resolver;
        self.diagnostics = loaded.diagnostics;
        self.projection_loaded = true;
        self.current_view = None;
        Ok(())
    }

    /// Whether this request matches the pregenerated temporary default view.
    fn snapshot_supports_view(&self, hide_submodules: bool, filter: &ChainFilter) -> bool {
        self.snapshot.is_some()
            && hide_submodules == fixed_view_hide_submodules()
            && filter.key() == fixed_view_filter().key()
    }

    /// Node count for the Open handshake, independent of backend.
    fn node_count(&self) -> u64 {
        self.snapshot.as_ref().map_or_else(
            || u64::try_from(self.projection.len()).unwrap_or(u64::MAX),
            RenderSnapshot::projection_nodes,
        )
    }

    /// Accepted chain generation for the Open handshake, independent of backend.
    fn chain_generation(&self) -> u64 {
        self.snapshot.as_ref().map_or_else(
            || u64::try_from(self.projection.ops.len()).unwrap_or(u64::MAX),
            RenderSnapshot::chain_generation,
        )
    }

    /// Human-readable render-cache status for diagnostics and performance tests.
    const fn render_snapshot_status(&self) -> &'static str {
        if self.snapshot.is_some() {
            "hit"
        } else {
            "miss"
        }
    }

    /// Get a window of history rows (newest-first).
    #[must_use]
    pub fn history_window(&mut self, options: HistoryWindowOptions<'_>) -> HistoryWindow {
        let offset = options.offset;
        let include_layout = options.include_layout;
        match self.try_history_window(options) {
            Ok(window) => window,
            Err(_) => HistoryWindow {
                rows: Vec::new(),
                total: 0,
                chain_generation: self.chain_generation(),
                max_lane: 0,
                sub_op_counts: (offset == 0).then(Vec::new),
                expansion_spans: (offset == 0).then(Vec::new),
                layout_ready: include_layout,
            },
        }
    }

    /// Fallible history-window path used by the protocol server and snapshot builder.
    fn try_history_window(
        &mut self,
        options: HistoryWindowOptions<'_>,
    ) -> Result<HistoryWindow, Box<dyn std::error::Error>> {
        if self.snapshot_supports_view(options.hide_submodules, options.filter) {
            let snapshot = self
                .snapshot
                .as_mut()
                .ok_or("render snapshot disappeared during request")?;
            return snapshot.history_window(options.offset, options.limit, options.include_layout);
        }
        self.ensure_projection_loaded()?;
        Ok(self.projection_history_window(options))
    }

    /// Compute a history window from the complete in-memory projection.
    #[must_use]
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::needless_borrow,
        reason = "expanded-slot prefix sums are bounded by node count; indexing is bounds-checked by partition_point; node is a &HistoryNode reference"
    )]
    fn projection_history_window(&mut self, options: HistoryWindowOptions<'_>) -> HistoryWindow {
        let HistoryWindowOptions {
            offset,
            limit,
            hide_submodules,
            filter,
            include_layout,
        } = options;
        let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        self.ensure_view_snapshot(hide_submodules, filter);
        if include_layout {
            self.ensure_view_layout();
        }
        let Some((_, snapshot)) = self.current_view.as_ref() else {
            return HistoryWindow {
                rows: Vec::new(),
                total: 0,
                chain_generation: u64::try_from(self.projection.ops.len()).unwrap_or(u64::MAX),
                max_lane: 0,
                sub_op_counts: (offset == 0).then(Vec::new),
                expansion_spans: (offset == 0).then(Vec::new),
                layout_ready: include_layout,
            };
        };
        let filtered = &snapshot.nodes;
        let ctx = snapshot.context.as_ref();

        // The service emits a FIXED fully-expanded depth-first list: every
        // top-level graph node always occupies one parent slot followed by all
        // presentation descendants. Fetch/cache indices therefore never move;
        // collapse/expand is purely a client decision driven by expansion spans.
        let starts = &snapshot.starts;
        let expanded_total = snapshot.expanded_total;

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
            let node = &filtered[abs_idx];
            let expansion = &snapshot.expansions[abs_idx];
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
                // Parents come from the cached LayoutContext for THIS exact
                // filtered snapshot (not the full projection), so a parent
                // that resolves to a row hidden by the filter is never
                // emitted as a dangling key. Relations are derived by the
                // projection from these same final parent keys, so every
                // relation's parent is guaranteed to be a rendered parent.
                let parents = ctx.map_or_else(
                    || self.projection.lifted_parent_keys(node),
                    |layout| {
                        layout
                            .parents
                            .get(&node.node_key())
                            .cloned()
                            .unwrap_or_default()
                    },
                );
                let parent_relations = self
                    .projection
                    .parent_relations_for(&node, &parents)
                    .into_iter()
                    .map(|r| ParentRelationDto {
                        parent: r.parent,
                        kind: protocol_relation_kind(r.kind),
                    })
                    .collect();
                rows.push(HistoryRow {
                    op_id: node.op_id().map(|id| id.to_string()),
                    git_oid: node.git_oid().map(|oid| oid.to_hex()),
                    repository: node.repository().map(|rid| rid.0.to_string()),
                    summary: node.summary(),
                    timestamp_ms: node.timestamp_ms(),
                    group: group.clone(),
                    group_end: filtered
                        .get(abs_idx.saturating_add(1))
                        .is_none_or(|next| next.group() != group),
                    node_key: node.node_key(),
                    parents,
                    parent_relations,
                    is_submodule: node
                        .repository()
                        .is_some_and(|rid| self.repo_is_submodule(rid)),
                    is_system: node_is_system(&node),
                    author: node_author(&node),
                    commit_id: node_commit_id(&node),
                    kind: node.kind(),
                    lane,
                    above,
                    below,
                    transitions,
                    muted_above,
                    muted_below,
                    muted_transitions,
                    sub_ops: expansion.direct.clone(),
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
                    session_summary: snapshot
                        .annotations
                        .get(abs_idx)
                        .and_then(|annotation| annotation.session_summary.as_ref())
                        .map(session_summary_dto),
                    work_unit: snapshot
                        .annotations
                        .get(abs_idx)
                        .map(|annotation| work_unit_dto(&annotation.work_unit)),
                    promoted: snapshot
                        .annotations
                        .get(abs_idx)
                        .is_some_and(|annotation| annotation.promoted),
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
            for (i, child) in expansion.rows.iter().enumerate() {
                let slot = block_start + 1 + i;
                if slot < offset_usize || slot >= end_usize {
                    continue;
                }
                rows.push(HistoryRow {
                    op_id: (!child.op_id.is_empty()).then(|| child.op_id.clone()),
                    git_oid: child.git_oid.clone(),
                    repository: child.repository.clone(),
                    summary: child.summary.clone(),
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
                    sub_ops: child.direct.clone(),
                    is_subop: true,
                    hierarchy_depth: child.depth,
                    parent_row: Some(parent_row.saturating_add(child.parent_relative)),
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
            rows,
            total: u64::try_from(expanded_total).unwrap_or(u64::MAX),
            chain_generation: u64::try_from(self.projection.ops.len()).unwrap_or(u64::MAX),
            max_lane: snapshot.max_lane,
            // The renderer always establishes a filter state from offset zero;
            // ship the O(V) expansion index once for that snapshot, not with
            // every O(window) page.
            sub_op_counts: (offset == 0).then(|| snapshot.sub_op_counts.clone()),
            expansion_spans: (offset == 0).then(|| snapshot.expansion_spans.clone()),
            layout_ready: snapshot.context.is_some(),
        }
    }

    /// Build and cache the immutable node/layout/expansion snapshot for a filter.
    fn ensure_view_snapshot(&mut self, hide_submodules: bool, filter: &ChainFilter) {
        let key = (hide_submodules, filter.key());
        if self
            .current_view
            .as_ref()
            .is_some_and(|(cached_key, _)| *cached_key == key)
        {
            return;
        }
        let all_nodes = self.projection.filtered_nodes(filter);
        let nodes: Vec<_> = if hide_submodules {
            all_nodes
                .into_iter()
                .filter(|n| {
                    !n.repository()
                        .is_some_and(|rid| self.repo_is_submodule(rid))
                })
                .collect()
        } else {
            all_nodes
        };
        // Activity-view semantics: every view gets deterministic work-unit and
        // promotion annotations. The fixed Activity view additionally keeps
        // context-compaction checkpoints inline, groups adjacent repeated Plan
        // headings, folds safe low-signal execute runs, then wraps every linear
        // non-chat interval in an outer work group (never the Raw profile,
        // whose topology stays exact).
        // Annotations are recomputed on the final list so bundle rows carry
        // their own unit markers.
        let is_activity = filter.key() == fixed_view_filter().key();
        let structural = is_activity.then(|| self.projection.structural_row_keys(&nodes));
        let nodes = if let Some(structural) = structural.as_ref() {
            editchain_project::activity::inline_context_compaction_checkpoints(nodes, structural)
        } else {
            nodes
        };
        // Plan repeats group before promotion is computed: the newest member
        // may itself be the unit-final narrative, and the resulting bundle —
        // not an arbitrary duplicate member — should carry that significance.
        let nodes = if let Some(structural) = structural.as_ref() {
            editchain_project::activity::bundle_activity_plan_repeats(nodes, structural)
        } else {
            nodes
        };
        let mut annotations = editchain_project::activity::annotate_activity_rows(&nodes);
        let nodes = if let Some(structural) = structural.as_ref() {
            editchain_project::activity::bundle_activity_execute_runs(
                nodes,
                &annotations,
                structural,
            )
        } else {
            nodes
        };
        let nodes = if let Some(structural) = structural.as_ref() {
            editchain_project::activity::bundle_claude_response_tool_fragments(nodes, structural)
        } else {
            nodes
        };
        let nodes = if let Some(structural) = structural.as_ref() {
            editchain_project::activity::bundle_activity_work_groups(nodes, structural)
        } else {
            nodes
        };
        annotations = editchain_project::activity::annotate_activity_rows(&nodes);
        let expansions: Vec<NodeExpansion> = nodes
            .iter()
            .map(|node| node_expansion(node, &self.agent_file_changes, &self.git_file_changes))
            .collect();
        let sub_op_counts: Vec<usize> = expansions
            .iter()
            .map(|expansion| expansion.rows.len())
            .collect();
        let mut starts = Vec::with_capacity(nodes.len().saturating_add(1));
        starts.push(0usize);
        for &count in &sub_op_counts {
            starts.push(
                starts
                    .last()
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1)
                    .saturating_add(count),
            );
        }
        let expanded_total = starts.last().copied().unwrap_or(0);
        let mut expansion_spans = Vec::new();
        for (index, expansion) in expansions.iter().enumerate() {
            let block_start = starts.get(index).copied().unwrap_or(0);
            if !expansion.rows.is_empty() {
                expansion_spans.push(ExpansionSpanDto {
                    row: u64::try_from(block_start).unwrap_or(u64::MAX),
                    descendant_count: u64::try_from(expansion.rows.len()).unwrap_or(u64::MAX),
                });
            }
            for (child_index, child) in expansion.rows.iter().enumerate() {
                if child.descendant_count == 0 {
                    continue;
                }
                expansion_spans.push(ExpansionSpanDto {
                    row: u64::try_from(block_start.saturating_add(1).saturating_add(child_index))
                        .unwrap_or(u64::MAX),
                    descendant_count: u64::try_from(child.descendant_count).unwrap_or(u64::MAX),
                });
            }
        }
        self.current_view = Some((
            key,
            ViewSnapshot {
                nodes,
                annotations,
                context: None,
                sub_op_counts,
                expansions,
                expansion_spans,
                starts,
                expanded_total,
                max_lane: 0,
                op_rows: None,
                git_rows: None,
            },
        ));
    }

    /// Build global graph geometry for the current row snapshot on demand.
    fn ensure_view_layout(&mut self) {
        let projection = &self.projection;
        let Some((_, snapshot)) = self.current_view.as_mut() else {
            return;
        };
        if snapshot.context.is_some() {
            return;
        }
        let context = projection.layout_context(&snapshot.nodes);
        snapshot.max_lane = context.lanes.iter().map(|row| row.lane).max().unwrap_or(0);
        snapshot.context = Some(context);
    }

    /// Build (once) the cached op→row / git→row maps for the current view
    /// snapshot, so repeated Find-in-Chain requests reuse the O(V) mapping
    /// instead of rebuilding it per arrow press.
    ///
    /// The op map indexes every op this snapshot renders: each top-level row's
    /// own op id (or bundle anchor) plus every bundled sub-op/member op id, so
    /// a hit inside a folded META sub-op, tool result, execute-run member, or
    /// plan-repeat member resolves directly to its containing top-level row.
    /// The git map indexes visible git commits by `(repository, oid)`; rows
    /// hidden by the active filter or `hide_submodules` are simply absent, so
    /// hits with no row in the active view are filtered out downstream.
    fn build_find_row_maps(&mut self) {
        let Some((_, snapshot)) = self.current_view.as_mut() else {
            return;
        };
        if snapshot.op_rows.is_some() && snapshot.git_rows.is_some() {
            return;
        }
        let mut op_rows = HashMap::with_capacity(snapshot.nodes.len());
        let mut git_rows = HashMap::new();
        for (row, node) in snapshot.nodes.iter().enumerate() {
            if let Some(id) = node.op_id() {
                let _: Option<usize> = op_rows.insert(id, row);
            }
            if let (Some(oid), Some(repository)) = (node.git_oid(), node.repository()) {
                let _: Option<usize> = git_rows.insert((repository, oid), row);
            }
            for op in node.sub_ops() {
                let _: Option<usize> = op_rows.insert(op.id, row);
            }
        }
        snapshot.op_rows = Some(op_rows);
        snapshot.git_rows = Some(git_rows);
    }

    /// Resolve scored search chunks to distinct visible top-level rows of the
    /// active view, deduplicating by row and keeping the best BM25 score.
    ///
    /// The exact `(hide_submodules, filter)` pair must match the view the
    /// client renders (the same values it passed to `GetWindow`/`GetLayout`);
    /// the resolution reuses the cached per-filter `ViewSnapshot` for that key. Every
    /// returned match carries the row's stable real `node_key` and its absolute
    /// expanded-history parent-row offset (from the snapshot's `starts` prefix
    /// sums), so the viewer can cycle matches without auto-expanding anything.
    /// Hits whose op id has no visible row under the active view are dropped;
    /// `Git` hits are resolved by real `(repository, oid)` identity, never the
    /// synthetic index-only op id.
    #[must_use]
    pub fn find_in_history(
        &mut self,
        chunks: &[ScoredChunk],
        git_identities: &std::collections::BTreeMap<OpId, GitHitIdentity>,
        hide_submodules: bool,
        filter: &ChainFilter,
    ) -> Vec<FindInHistoryMatch> {
        self.ensure_view_snapshot(hide_submodules, filter);
        self.build_find_row_maps();
        let Some((_, snapshot)) = self.current_view.as_ref() else {
            return Vec::new();
        };
        let Some(op_rows) = snapshot.op_rows.as_ref() else {
            return Vec::new();
        };
        let Some(git_rows) = snapshot.git_rows.as_ref() else {
            return Vec::new();
        };
        let projection = &self.projection;
        // Distinct visible rows → best (highest) score chunk for that row.
        let mut best: HashMap<usize, (f64, &ScoredChunk)> = HashMap::new();
        for chunk in chunks {
            let Some(row) = resolve_chunk_row(
                op_rows,
                git_rows,
                git_identities,
                &|op_id| projection.visible_op_id(op_id),
                chunk,
            ) else {
                continue;
            };
            let entry = best.entry(row).or_insert_with(|| (chunk.score, chunk));
            if chunk.score > entry.0 {
                *entry = (chunk.score, chunk);
            }
        }
        let starts = &snapshot.starts;
        let mut matches: Vec<FindInHistoryMatch> = best
            .into_iter()
            .filter_map(|(row, (_, chunk))| {
                let node = snapshot.nodes.get(row)?;
                Some(find_match_from_chunk(
                    chunk,
                    git_identities,
                    node.node_key(),
                    u64::try_from(starts.get(row).copied().unwrap_or(0)).unwrap_or(u64::MAX),
                    node.summary(),
                ))
            })
            .collect();
        // Ranked: best score first; ties break to the newest visible row so the
        // ordering is deterministic for the viewer's cycle.
        matches.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.row.cmp(&b.row))
        });
        matches
    }

    /// Fallible compatibility path that materializes projection state as needed.
    fn try_graph_layout(
        &mut self,
        hide_submodules: bool,
        offset: u64,
        limit: u64,
        filter: &ChainFilter,
    ) -> Result<ProtocolGraphLayout, Box<dyn std::error::Error>> {
        self.ensure_projection_loaded()?;
        Ok(self.graph_layout(hide_submodules, offset, limit, filter))
    }

    /// Compute the graph layout for a bounded window of rows.
    ///
    /// The layout context (all O(V) derived data) is cached per `hide_submodules`
    /// and computed once; only edges whose child falls inside `[offset,
    /// offset+limit)` are emitted. This keeps per-scroll cost proportional to the
    /// visible slice rather than the whole graph.
    #[must_use]
    #[expect(
        clippy::print_stderr,
        reason = "Diagnostic logging to the VS Code output pane via service stderr"
    )]
    pub fn graph_layout(
        &mut self,
        hide_submodules: bool,
        offset: u64,
        limit: u64,
        filter: &ChainFilter,
    ) -> ProtocolGraphLayout {
        self.ensure_view_snapshot(hide_submodules, filter);
        self.ensure_view_layout();
        let Some((_, snapshot)) = self.current_view.as_ref() else {
            return ProtocolGraphLayout {
                rows: Vec::new(),
                edges: Vec::new(),
                max_lane: 0,
            };
        };
        let Some(ctx) = snapshot.context.as_ref() else {
            return ProtocolGraphLayout {
                rows: Vec::new(),
                edges: Vec::new(),
                max_lane: 0,
            };
        };
        let offset_usize = usize::try_from(offset).unwrap_or(0);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        let edges = ctx.edges_for_window(offset_usize, limit_usize);
        eprintln!(
            "[layout] GetLayout offset={} limit={} rows={} edges={} max_lane={}",
            offset_usize,
            limit_usize,
            ctx.lanes.len(),
            edges.len(),
            ctx.lanes.iter().map(|r| r.lane).max().unwrap_or(0)
        );

        // Only send the window slice of rows (the webview needs lanes only for
        // visible rows). Sending all V rows would serialize the whole graph on
        // every scroll.
        let row_end = offset_usize
            .saturating_add(limit_usize)
            .min(ctx.lanes.len());
        let rows = ctx
            .lanes
            .get(offset_usize..row_end)
            .unwrap_or(&[])
            .iter()
            .map(|r| LayoutRow {
                node: r.node.clone(),
                lane: r.lane,
            })
            .collect();

        // Global max lane across ALL rows (not just this window), so the client
        // can size the graph column stably regardless of which window is loaded.
        ProtocolGraphLayout {
            rows,
            edges: edges
                .into_iter()
                .map(|e| LayoutEdge {
                    child: e.child,
                    parent: e.parent,
                    points: e
                        .points
                        .into_iter()
                        .map(|p| LayoutPoint {
                            row: p.row,
                            lane: p.lane,
                        })
                        .collect(),
                })
                .collect(),
            max_lane: snapshot.max_lane,
        }
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
                .git
                .commits
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
            let snapshot = self.snapshot.as_ref()?;
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
    fn file_diff(&self, change: &FileChangeDto) -> Result<FileDiffDto, String> {
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
        let handle = open_repository_handle(discovery).map_err(|error| error.to_string())?;
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

    /// List discovered repositories.
    #[must_use]
    pub fn repositories_info(&self) -> Vec<RepositoryInfo> {
        self.repositories
            .iter()
            .map(|d| RepositoryInfo {
                id: d.id.0.to_string(),
                path: d.path.to_string_lossy().to_string(),
                is_worktree: d.is_worktree,
                is_submodule: self.is_submodule(d),
            })
            .collect()
    }

    /// Returns true if a repository is nested inside another discovered repo
    /// (i.e. a submodule or vendored nested repo, not the workspace root).
    #[must_use]
    fn is_submodule(&self, discovery: &editchain_git::RepositoryDiscovery) -> bool {
        // Discovery paths point at `.git`; the repo root is the parent dir.
        let root = discovery.path.parent().unwrap_or(&discovery.path);
        let root_str = root.to_string_lossy();
        self.repositories.iter().any(|other| {
            if other.id == discovery.id || other.path == discovery.path {
                return false;
            }
            let other_root = other.path.parent().unwrap_or(&other.path);
            let other_str = other_root.to_string_lossy();
            // This repo's root is strictly inside another repo's root.
            other_str.len() < root_str.len() && root_str.starts_with(&*other_str)
        })
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
    let handle = open_repository_handle(discovery).ok()?;
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
                    note: Some(
                        "Recorded Codex update evidence is binary and cannot be opened as text."
                            .to_string(),
                    ),
                });
            }
            let (before, after) = unified_diff_fragments(&diff);
            Ok(FileDiffDto {
                path: requested.path.clone(),
                old_path: requested.old_path.clone(),
                status: requested.status,
                binary: false,
                partial: true,
                before,
                after,
                note: Some(
                    "Recorded Codex unified-diff hunks; complete sequential file snapshots were not retained."
                        .to_string(),
                ),
            })
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
            let (before, after) = unified_diff_fragments(&diff);
            Ok(FileDiffDto {
                path: requested.path.clone(),
                old_path: requested.old_path.clone(),
                status: requested.status,
                binary: false,
                partial: true,
                before,
                after,
                note: Some(
                    "Recorded unified-diff hunks; complete before/after file snapshots were not retained."
                        .to_string(),
                ),
            })
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
        note: note.map(str::to_string),
    })
}

#[must_use]
fn bytes_are_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn unified_diff_fragments(diff: &str) -> (String, String) {
    const SEPARATOR: &str = "\n\n… next recorded hunk …\n\n";
    let mut before_hunks = Vec::new();
    let mut after_hunks = Vec::new();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("@@") {
            if in_hunk {
                before_hunks.push(before.join("\n"));
                after_hunks.push(after.join("\n"));
                before.clear();
                after.clear();
            }
            in_hunk = true;
            continue;
        }
        if !in_hunk {
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
    if in_hunk {
        before_hunks.push(before.join("\n"));
        after_hunks.push(after.join("\n"));
    }
    if before_hunks.is_empty() && after_hunks.is_empty() {
        return (String::new(), diff.to_string());
    }
    (before_hunks.join(SEPARATOR), after_hunks.join(SEPARATOR))
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
    let repositories = discover_repositories(workspace_path)?;
    let identity = SnapshotIdentity::capture(&chain_path, &repositories)?;
    if let Ok(Some(snapshot)) = RenderSnapshot::open(&chain_path, &identity) {
        return snapshot.report();
    }

    let mut workspace = Workspace::open_projection(
        workspace_path.to_path_buf(),
        chain_path.clone(),
        repositories.clone(),
    )?;
    let filter = fixed_view_filter();
    let page_limit = 4_096u64;
    let first = workspace.try_history_window(HistoryWindowOptions {
        offset: 0,
        limit: page_limit,
        hide_submodules: fixed_view_hide_submodules(),
        filter: &filter,
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
        let window = workspace.try_history_window(HistoryWindowOptions {
            offset,
            limit: page_limit,
            hide_submodules: fixed_view_hide_submodules(),
            filter: &filter,
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

    let final_identity = SnapshotIdentity::capture(&chain_path, &repositories)?;
    if final_identity != identity {
        return Err(
            io::Error::other("chain or Git HEAD changed while preparing render snapshot").into(),
        );
    }
    builder.finish(
        SnapshotManifestData {
            projection_nodes: u64::try_from(workspace.projection.len()).unwrap_or(u64::MAX),
            chain_generation: u64::try_from(workspace.projection.ops.len()).unwrap_or(u64::MAX),
            max_lane,
            diagnostics: workspace.diagnostics,
        },
        &sub_op_counts,
        &expansion_spans,
        &workspace.source_op_locations,
    )
}

/// Convert an optional protocol filter DTO into a [`ChainFilter`].
///
/// A `None` DTO yields the fixed viewer filter (Activity view: splice on,
/// hide trace and undated rows) so the render snapshot serves it. An empty
/// DTO yields an empty filter that hides nothing; raw mode sends an explicit
/// `hide_trace: false`. `ChainFilter::default()` itself stays the raw
/// baseline (`hide_trace` off) — the Activity view is an explicit choice.
#[must_use]
fn chain_filter_from_dto(dto: Option<&ChainFilterDto>) -> ChainFilter {
    match dto {
        Some(d) => ChainFilter::new(
            d.summary_pattern.clone(),
            d.kind_pattern.clone(),
            d.include_kind_pattern.clone(),
            d.hide_undated,
            d.splice,
            d.hide_trace,
        ),
        None => fixed_view_filter(),
    }
}

/// Projection options shared by live computation and pregeneration.
const fn projection_options() -> editchain_project::ProjectionOptions {
    editchain_project::ProjectionOptions {
        bundle_metadata: true,
    }
}

/// Temporary fixed viewer filter while the filtering UI is being redesigned.
fn fixed_view_filter() -> ChainFilter {
    ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        true,
    )
}

/// The temporary fixed viewer hides nested Git repositories/submodules.
const fn fixed_view_hide_submodules() -> bool {
    true
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

/// Convert protocol search filters into the query crate's internal filters,
/// parsing exact decimal session/actor ID strings.
///
/// # Errors
///
/// Returns an error message when a session or actor ID is not a valid `u64`.
pub fn search_filters_from_dto(dto: &SearchFiltersDto) -> Result<SearchFilters, String> {
    fn parse_ids<T>(
        ids: Option<&Vec<String>>,
        what: &str,
        wrap: impl Fn(u64) -> T + Copy,
    ) -> Result<Option<Vec<T>>, String> {
        ids.map(|values| {
            values
                .iter()
                .map(|s| {
                    s.parse::<u64>()
                        .map(wrap)
                        .map_err(|_err| format!("invalid {what} id: {s:?}"))
                })
                .collect()
        })
        .transpose()
    }
    Ok(SearchFilters {
        kinds: dto.kinds.clone(),
        sources: dto.sources.clone(),
        sessions: parse_ids(dto.sessions.as_ref(), "session", SessionId)?,
        actors: parse_ids(dto.actors.as_ref(), "actor", ActorId)?,
        paths: dto.paths.clone(),
        after: dto.after,
        before: dto.before,
        include_raw: dto.include_raw,
        include_private: dto.include_private,
    })
}

/// Convert a scored search chunk into the protocol's JSON-safe search hit.
///
/// `Git` hits are looked up in `git_identities` — the deterministic map from
/// the synthetic op ids assigned at index time to the real commit identity —
/// so the response navigates by exact `(repository, git_oid)` strings and
/// never by the synthetic, projection-less op id.
#[must_use]
pub fn search_hit_from_chunk(
    chunk: &ScoredChunk,
    git_identities: &std::collections::BTreeMap<OpId, GitHitIdentity>,
) -> SearchHit {
    let git = (chunk.metadata.source == Source::Git)
        .then(|| git_identities.get(&chunk.op_id))
        .flatten();
    SearchHit {
        op_id: chunk.op_id.to_string(),
        chunk_id: chunk.chunk_id.to_string(),
        score: chunk.score,
        text: chunk.text.clone(),
        source: chunk.metadata.source,
        session_id: chunk.metadata.session_id.map(|id| id.0.to_string()),
        actor_id: chunk.metadata.actor_id.0.to_string(),
        kind_tags: chunk.metadata.kind_tags,
        timestamp_ms: chunk.metadata.timestamp_ms,
        generation: chunk.metadata.generation,
        git_oid: git.map(|identity| identity.oid.to_hex()),
        repository: git.map(|identity| identity.repository_id.0.to_string()),
        kind: git.map_or_else(String::new, |_| "git".to_string()),
        is_submodule: git.is_some_and(|identity| identity.is_submodule),
    }
}

/// Resolve one scored chunk to its visible top-level row index in the active
/// view snapshot, or `None` when the hit has no row in that view.
///
/// `EditChain` hits resolve directly when the op (or a bundled sub-op/member
/// that shares the row) is in `op_rows`, otherwise through the projection's
/// semantic-collapse representative map (`visible_op_id`) to the canonical row
/// that renders the op — a folded normalized child, META sub-op, tool result,
/// or copied provider occurrence. A canonical row that is absent from `op_rows` is hidden by
/// the active filter/profile and dropped. `Git` hits resolve by real
/// `(repository, oid)` identity from `git_identities` (never the synthetic
/// index-only op id), so submodule rows hidden by `hide_submodules` are absent
/// from `git_rows` and dropped.
#[must_use]
fn resolve_chunk_row(
    op_rows: &HashMap<OpId, usize>,
    git_rows: &HashMap<(RepositoryId, GitOid), usize>,
    git_identities: &std::collections::BTreeMap<OpId, GitHitIdentity>,
    visible_op_id: &impl Fn(OpId) -> Option<OpId>,
    chunk: &ScoredChunk,
) -> Option<usize> {
    if chunk.metadata.source == Source::Git {
        let identity = git_identities.get(&chunk.op_id)?;
        return git_rows
            .get(&(identity.repository_id, identity.oid))
            .copied();
    }
    if let Some(&row) = op_rows.get(&chunk.op_id) {
        return Some(row);
    }
    visible_op_id(chunk.op_id).and_then(|canonical| op_rows.get(&canonical).copied())
}

/// Convert the best-scoring chunk for one visible row into a Find-in-Chain
/// match, attaching the row's stable identity and absolute parent-row offset.
///
/// Reuses [`search_hit_from_chunk`] so `EditChain`/`Git` identity fields stay
/// byte-identical to the legacy `Search` response.
#[must_use]
fn find_match_from_chunk(
    chunk: &ScoredChunk,
    git_identities: &std::collections::BTreeMap<OpId, GitHitIdentity>,
    node_key: String,
    row: u64,
    summary: String,
) -> FindInHistoryMatch {
    let hit = search_hit_from_chunk(chunk, git_identities);
    FindInHistoryMatch {
        node_key,
        row,
        summary,
        score: hit.score,
        op_id: hit.op_id,
        chunk_id: hit.chunk_id,
        text: hit.text,
        source: hit.source,
        session_id: hit.session_id,
        actor_id: hit.actor_id,
        kind_tags: hit.kind_tags,
        timestamp_ms: hit.timestamp_ms,
        generation: hit.generation,
        git_oid: hit.git_oid,
        repository: hit.repository,
        kind: hit.kind,
        is_submodule: hit.is_submodule,
    }
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
            kind == "tool" || kind == "import"
        }
        // Execute-run bundles summarize tool/command rows: dim them like the
        // individual tool rows they fold.
        editchain_project::HistoryNode::ExecuteBundle { .. } => true,
        // Work groups and Plan-repeat bundles are navigational/prose-first
        // summary rows; Git is likewise user-facing source history.
        editchain_project::HistoryNode::WorkGroup { .. }
        | editchain_project::HistoryNode::PlanBundle { .. }
        | editchain_project::HistoryNode::GitCommit(_) => false,
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
        editchain_project::HistoryNode::GitCommit(commit) => payload_text(&commit.author.name),
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
                timestamp_ms: op.clock.as_u64(),
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
        title: marker.title.clone(),
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
/// ordinary rows and the raw (unbundled) profile.
#[must_use]
fn node_activity_bundle(
    node: &editchain_project::HistoryNode,
) -> Option<editchain_protocol::ActivityBundleDto> {
    match node {
        editchain_project::HistoryNode::WorkGroup { member_nodes, .. } => {
            Some(editchain_protocol::ActivityBundleDto {
                kind: editchain_protocol::ActivityBundleKind::WorkGroup,
                member_count: member_nodes
                    .iter()
                    .map(node_leaf_activity_count)
                    .fold(0u64, u64::saturating_add),
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
        | editchain_project::HistoryNode::GitCommit(_) => None,
    }
}

/// Original activity-row count represented by a possibly nested synthetic
/// node. Work-group count labels describe source activities, not display rows.
#[must_use]
fn node_leaf_activity_count(node: &editchain_project::HistoryNode) -> u64 {
    match node {
        editchain_project::HistoryNode::ExecuteBundle { member_nodes, .. }
        | editchain_project::HistoryNode::PlanBundle { member_nodes, .. }
        | editchain_project::HistoryNode::WorkGroup { member_nodes, .. } => member_nodes
            .iter()
            .map(node_leaf_activity_count)
            .fold(0u64, u64::saturating_add),
        editchain_project::HistoryNode::EditOperation { .. }
        | editchain_project::HistoryNode::CollapsedImport { .. }
        | editchain_project::HistoryNode::GitCommit(_) => 1,
    }
}

/// Build the fixed depth-first expansion tree for one top-level graph row.
///
/// Ordinary rows and top-level execute/plan bundles retain their established
/// one-level details. A `WorkGroup` exposes each pre-grouped activity as a depth-1
/// row; that member's existing details/bundle members become depth-2 rows.
#[must_use]
fn node_expansion(
    node: &editchain_project::HistoryNode,
    agent_changes: &HashMap<OpId, Vec<FileChangeDto>>,
    git_changes: &HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>>,
) -> NodeExpansion {
    let editchain_project::HistoryNode::WorkGroup { member_nodes, .. } = node else {
        let file_changes = node_file_changes(node, agent_changes, git_changes);
        let mut direct = node_sub_op_summaries(node);
        direct.extend(
            file_changes
                .iter()
                .map(|change| file_change_summary(change, node)),
        );
        let mut rows = flat_op_child_rows(node, 0, 1);
        let file_row_context = FileChangeRowContext {
            parent_relative: 0,
            depth: 1,
            timestamp_ms: node.timestamp_ms(),
            chain_state: node.chain_state(),
            turn_id: node.turn_id().map(|id| id.0.to_string()),
        };
        rows.extend(file_change_rows(&file_changes, &file_row_context));
        return NodeExpansion { direct, rows };
    };

    let direct = member_nodes.iter().map(node_summary_dto).collect();
    let mut rows = Vec::new();
    for member in member_nodes {
        let member_relative = rows.len().saturating_add(1);
        let file_changes = node_file_changes(member, agent_changes, git_changes);
        let mut member_direct = node_sub_op_summaries(member);
        member_direct.extend(
            file_changes
                .iter()
                .map(|change| file_change_summary(change, member)),
        );
        let mut descendants = flat_op_child_rows(member, member_relative, 2);
        let file_row_context = FileChangeRowContext {
            parent_relative: member_relative,
            depth: 2,
            timestamp_ms: member.timestamp_ms(),
            chain_state: member.chain_state(),
            turn_id: member.turn_id().map(|id| id.0.to_string()),
        };
        descendants.extend(file_change_rows(&file_changes, &file_row_context));
        rows.push(ExpandedChildRow {
            op_id: member.op_id().map_or_else(String::new, |id| id.to_string()),
            git_oid: member.git_oid().map(|oid| oid.to_hex()),
            repository: member.repository().map(|id| id.0.to_string()),
            summary: member.summary(),
            timestamp_ms: member.timestamp_ms(),
            kind: member.kind(),
            author: node_author(member),
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
            direct: member_direct,
            parent_relative: 0,
            depth: 1,
            descendant_count: descendants.len(),
        });
        rows.extend(descendants);
    }
    NodeExpansion { direct, rows }
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
        editchain_project::HistoryNode::GitCommit(commit) => git_changes
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

fn file_change_summary(
    change: &FileChangeDto,
    node: &editchain_project::HistoryNode,
) -> SubOpSummary {
    SubOpSummary {
        op_id: change.op_id.clone().unwrap_or_default(),
        summary: change.path.clone(),
        kind: "file".to_string(),
        timestamp_ms: node.timestamp_ms(),
    }
}

struct FileChangeRowContext {
    parent_relative: usize,
    depth: u8,
    timestamp_ms: u64,
    chain_state: ChainState,
    turn_id: Option<String>,
}

fn file_change_rows(
    changes: &[FileChangeDto],
    context: &FileChangeRowContext,
) -> Vec<ExpandedChildRow> {
    changes
        .iter()
        .cloned()
        .map(|change| ExpandedChildRow {
            op_id: change.op_id.clone().unwrap_or_default(),
            git_oid: change.commit_oid.clone(),
            repository: change.repository.clone(),
            summary: change.path.clone(),
            timestamp_ms: context.timestamp_ms,
            kind: "file".to_string(),
            author: String::new(),
            commit_id: String::new(),
            is_system: false,
            record_role: RecordRole::Artifact,
            activity_kind: ActivityKind::Change,
            visibility: Visibility::Supporting,
            outcome: Outcome::Unknown,
            chain_state: context.chain_state,
            turn_id: context.turn_id.clone(),
            promoted: false,
            activity_bundle: None,
            file_change: Some(change),
            direct: Vec::new(),
            parent_relative: context.parent_relative,
            depth: context.depth,
            descendant_count: 0,
        })
        .collect()
}

/// One node as a direct-child summary advertised by its enclosing `WorkGroup`.
#[must_use]
fn node_summary_dto(node: &editchain_project::HistoryNode) -> SubOpSummary {
    SubOpSummary {
        op_id: node.op_id().map_or_else(String::new, |id| id.to_string()),
        summary: node.summary(),
        kind: node.kind(),
        timestamp_ms: node.timestamp_ms(),
    }
}

/// Established flat detail/member rows for one ordinary or inner bundle node.
#[must_use]
fn flat_op_child_rows(
    node: &editchain_project::HistoryNode,
    parent_relative: usize,
    depth: u8,
) -> Vec<ExpandedChildRow> {
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
                summary: summary.summary,
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
                direct: Vec::new(),
                parent_relative,
                depth,
                descendant_count: 0,
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
                    summary: member.summary(),
                    kind: member.kind(),
                    timestamp_ms: op.clock.as_u64(),
                }
            } else {
                let (summary, kind) = sub_op_label(op);
                SubOpSummary {
                    op_id: op_key,
                    summary,
                    kind,
                    timestamp_ms: op.clock.as_u64(),
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
        | editchain_project::HistoryNode::GitCommit(_) => HashMap::new(),
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
        editchain_project::HistoryNode::GitCommit(commit) => abbreviate_oid(&commit.oid),
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
fn resolved_object_from_commit(commit: &editchain_core::GitCommitEntity) -> ResolvedObject {
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

/// Encoded chain record paired with its exact durable location.
#[derive(Debug)]
struct LocatedChainRecord {
    data: Vec<u8>,
    location: OpRecordLocation,
}

/// Read all decoded operations from a chain directory, canonicalized through
/// [`OpSet`].
///
/// Replayed pages (e.g. after an interrupted import retried a page the first
/// run had already synced) are deduplicated by exact bytes, and same-id
/// records with conflicting bytes are quarantined rather than silently
/// overwriting the accepted op. Returns the accepted ops in canonical key
/// order plus the read stats.
///
/// # Errors
///
/// Returns an error if the chain directory cannot be read.
fn read_chain_ops(chain_dir: &Path) -> ChainReadResult {
    if chain_dir.as_os_str().is_empty() {
        return Ok((Vec::new(), ChainReadStats::default(), Vec::new()));
    }
    let records = read_chain_records(chain_dir)?;
    let mut opset = OpSet::new();
    let mut accepted: Vec<(Op, SnapshotOpLocator)> = Vec::new();
    let mut stats = ChainReadStats::default();
    for record in records {
        let Ok(op) = decode_op(&record.data) else {
            continue;
        };
        stats.records = stats.records.saturating_add(1);
        match opset.insert(op.id, record.data) {
            Ok(true) => {
                stats.accepted = stats.accepted.saturating_add(1);
                let id = op.id;
                accepted.push((
                    op,
                    SnapshotOpLocator {
                        id,
                        location: record.location,
                    },
                ));
            }
            Ok(false) => stats.duplicates = stats.duplicates.saturating_add(1),
            Err(_) => stats.quarantined = stats.quarantined.saturating_add(1),
        }
    }
    // Match the OpSet's canonical `OpId` key order without a second decode
    // pass — decoding 100k+ records twice is the dominant Open cost in debug.
    accepted.sort_by_key(|(op, _)| op.id);
    let (ops, locations) = accepted.into_iter().unzip();
    Ok((ops, stats, locations))
}

/// Scan every complete segment record while retaining byte offsets.
fn read_chain_records(chain_dir: &Path) -> io::Result<Vec<LocatedChainRecord>> {
    let mut records = Vec::new();
    let mut segment_seq = 0u32;
    loop {
        let path = chain_dir.join(format!("{segment_seq:06}.eclog"));
        if !path.exists() {
            break;
        }
        scan_segment_records(segment_seq, &fs::read(path)?, &mut records)?;
        segment_seq = segment_seq.saturating_add(1);
    }
    Ok(records)
}

/// Scan complete pages and records from one segment, ignoring a partial tail.
fn scan_segment_records(
    segment_seq: u32,
    bytes: &[u8],
    records: &mut Vec<LocatedChainRecord>,
) -> io::Result<()> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        let Some(magic) = bytes.get(offset..offset.saturating_add(4)) else {
            break;
        };
        if magic != PAGE_MAGIC {
            break;
        }
        if bytes.get(offset..offset.saturating_add(8)).is_none() {
            break;
        }
        offset = offset.saturating_add(8);
        loop {
            let Some(length_bytes) = bytes.get(offset..offset.saturating_add(4)) else {
                return Ok(());
            };
            if length_bytes == PAGE_MAGIC {
                break;
            }
            let length_array: [u8; 4] = length_bytes.try_into().map_err(|_error| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid record length")
            })?;
            let data_len = u32::from_le_bytes(length_array);
            let data_offset = offset.saturating_add(5);
            let data_end =
                data_offset.saturating_add(usize::try_from(data_len).unwrap_or(usize::MAX));
            let Some(data) = bytes.get(data_offset..data_end) else {
                return Ok(());
            };
            records.push(LocatedChainRecord {
                data: data.to_vec(),
                location: OpRecordLocation {
                    segment_seq,
                    data_offset: u64::try_from(data_offset).unwrap_or(u64::MAX),
                    data_len,
                },
            });
            offset = data_end;
        }
    }
    Ok(())
}

/// Decode one operation directly from its indexed segment-record location.
fn read_op_at(
    chain_dir: &Path,
    location: OpRecordLocation,
) -> Result<Op, Box<dyn std::error::Error>> {
    let path = chain_dir.join(format!("{:06}.eclog", location.segment_seq));
    let mut file = File::open(path)?;
    let _: u64 = file.seek(SeekFrom::Start(location.data_offset))?;
    let mut encoded = vec![0u8; usize::try_from(location.data_len)?];
    file.read_exact(&mut encoded)?;
    decode_op(&encoded).map_err(Into::into)
}

/// Deterministic git identity for a synthetic search-indexed op.
///
/// Git commits are indexed as synthetic ops whose numeric ids exist only
/// inside the index; this record is the single source of truth for the real
/// identity a search hit must navigate to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHitIdentity {
    /// Repository identity.
    pub repository_id: RepositoryId,
    /// Commit OID (lowercase hex when serialized).
    pub oid: GitOid,
    /// Whether the commit's repository is a nested/submodule repository.
    pub is_submodule: bool,
}

/// A lexical search index plus the synthetic-op → git identity map built
/// alongside it, so search responses stay deterministic and exact.
#[derive(Debug)]
pub struct SearchIndexState {
    /// The underlying Tantivy lexical index.
    pub index: LexicalIndex,
    /// Map from each synthetic indexed `OpId` to the real git commit identity
    /// (`BTreeMap`: deterministic order, no hasher dependency).
    pub git_identities: std::collections::BTreeMap<OpId, GitHitIdentity>,
}

/// Build a lexical index over all chain ops and git commits.
///
/// # Errors
///
/// Returns an error if the index cannot be created or populated.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Generation counter increments are bounded by the number of indexed ops"
)]
pub fn build_lexical_index(
    workspace: &Workspace,
) -> Result<SearchIndexState, Box<dyn std::error::Error>> {
    let mut index = LexicalIndex::new()?;
    let mut git_identities = std::collections::BTreeMap::new();
    let mut generation = 0u64;
    for source in &workspace.source_ops {
        let mut op = source.clone();
        if let Some(resolver) = &workspace.blob_resolver {
            let mut stats = BlobHydrationStats::default();
            hydrate_kind(&mut op.kind, resolver, &mut stats);
        }
        drop(index.index_op(&op, generation)?);
        generation += 1;
    }
    // Index git commits as synthetic ops, recording the deterministic mapping
    // from each synthetic op id back to the real commit identity so search
    // responses navigate by (repository, oid) — never the synthetic id.
    for commit in workspace.projection.git.commits.values() {
        let op = Op {
            id: OpId::new(NodeId(0), 0, generation),
            parents: ParentSet::None,
            actor: ActorId(0),
            clock: Clock::UnixMs(u64::try_from(commit.committed_at).unwrap_or(0)),
            scope: ScopeRef::None,
            tags: Tags::IMPORT,
            kind: OpKind::GitCommit(Box::new(commit.clone())),
        };
        drop(index.index_op(&op, generation)?);
        let _: Option<GitHitIdentity> = git_identities.insert(
            op.id,
            GitHitIdentity {
                repository_id: commit.repository,
                oid: commit.oid,
                is_submodule: workspace.repo_is_submodule(commit.repository),
            },
        );
        generation += 1;
    }
    index.commit()?;
    Ok(SearchIndexState {
        index,
        git_identities,
    })
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
    let Ok(handle) = open_repository_handle(discovery) else {
        return Ok(None);
    };
    match resolve_commit(&handle, oid) {
        Ok(res) => Ok(Some(res.commit)),
        Err(_) => Ok(None),
    }
}

/// Open a discovered repository as a `RepositoryHandle`.
fn open_repository_handle(
    discovery: &editchain_git::RepositoryDiscovery,
) -> Result<RepositoryHandle, Box<dyn std::error::Error>> {
    let open_path = if discovery.is_worktree {
        discovery
            .path
            .parent()
            .unwrap_or(&discovery.path)
            .to_path_buf()
    } else {
        discovery.path.clone()
    };
    let repo = gix::open(&open_path)?;
    Ok(RepositoryHandle {
        repo,
        discovery: discovery.clone(),
    })
}

/// Resolve exact durable Git-link targets that the current HEAD walk did not
/// include (for example, a session started on a branch that was later switched).
fn merge_exact_git_link_targets(
    projection: &mut HistoryProjection,
    repositories: &[editchain_git::RepositoryDiscovery],
) {
    let targets: std::collections::BTreeSet<(RepositoryId, GitOid)> = projection
        .git
        .links
        .values()
        .flatten()
        .map(|link| (link.target_repo, link.target_oid))
        .filter(|target| !projection.git.commits.contains_key(target))
        .collect();

    for discovery in repositories {
        let repository_targets: Vec<GitOid> = targets
            .iter()
            .filter_map(|(repository, oid)| (*repository == discovery.id).then_some(*oid))
            .collect();
        if repository_targets.is_empty() {
            continue;
        }
        let Ok(handle) = open_repository_handle(discovery) else {
            continue;
        };
        let commits: Vec<_> = repository_targets
            .iter()
            .filter_map(|oid| {
                resolve_commit(&handle, oid)
                    .ok()
                    .map(|result| result.commit)
            })
            .collect();
        projection.merge_git_commits(commits);
    }
}

/// A stateful server that owns a loaded workspace across requests.
#[derive(Debug)]
pub struct Server {
    /// The currently loaded workspace (None until `Open`).
    pub workspace: Option<Workspace>,
    /// The lexical search index plus synthetic-op → git identity map (built
    /// lazily on first `Search`).
    pub lexical: Option<SearchIndexState>,
}

impl Server {
    /// Create a new empty server.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workspace: None,
            lexical: None,
        }
    }

    /// Handle a single request against the current state.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be handled.
    pub fn handle(&mut self, request: &Request) -> Result<Response, Box<dyn std::error::Error>> {
        let id = request.id;
        let body = match &request.body {
            RequestBody::Open(req) => {
                let workspace = Workspace::open(&req.workspace_path, &req.chain_dir)?;
                let diagnostics = workspace.diagnostics;
                let warnings = workspace.diagnostics.warnings();
                // The lexical index is built lazily on first Search (it is
                // expensive for large chains and unnecessary for the graph view).
                self.workspace = Some(workspace);
                self.lexical = None;
                ResponseBody::Ok(serde_json::json!({
                    "workspace": req.workspace_path,
                    "chain": req.chain_dir,
                    "repos": self.workspace.as_ref().map_or(0, |w| w.repositories.len()),
                    "nodes": self.workspace.as_ref().map_or(0, Workspace::node_count),
                    "chain_generation": self.workspace.as_ref().map_or(0, Workspace::chain_generation),
                    "render_snapshot": self.workspace.as_ref().map_or("miss", Workspace::render_snapshot_status),
                    // Canonicalization + lazy blob access outcomes for this open.
                    // New keys: backward-compatible; older clients ignore them.
                    "diagnostics": serde_json::to_value(diagnostics)?,
                    "warnings": warnings,
                }))
            }
            RequestBody::GetWindow(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let window = ws.try_history_window(HistoryWindowOptions {
                    offset: req.offset,
                    limit: req.limit,
                    hide_submodules: req.hide_submodules,
                    filter: &filter,
                    include_layout: req.include_layout,
                })?;
                ResponseBody::Ok(serde_json::to_value(window)?)
            }
            RequestBody::GetLayout(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let layout =
                    ws.try_graph_layout(req.hide_submodules, req.offset, req.limit, &filter)?;
                ResponseBody::Ok(serde_json::to_value(layout)?)
            }
            RequestBody::GetNodeDetails(req) => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                match ws.node_details(Some(req.op_id.clone()), None) {
                    Some(details) => ResponseBody::Ok(serde_json::to_value(details)?),
                    None => ResponseBody::Error("node not found".to_string()),
                }
            }
            RequestBody::GetRepositories => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                ResponseBody::Ok(serde_json::to_value(ws.repositories_info())?)
            }
            RequestBody::ResolveObject(req) => {
                let ws = self.workspace.as_ref().ok_or("no workspace open")?;
                let parsed = parse_repository_id(&req.repository).and_then(|repository_id| {
                    parse_git_oid(&req.oid).map(|oid| (repository_id, oid))
                });
                match parsed {
                    Ok((repository_id, oid)) => {
                        match resolve_git_commit(ws, repository_id, &oid)? {
                            Some(commit) => ResponseBody::Ok(serde_json::to_value(
                                resolved_object_from_commit(&commit),
                            )?),
                            None => ResponseBody::Error("object not found".to_string()),
                        }
                    }
                    Err(msg) => ResponseBody::Error(msg),
                }
            }
            RequestBody::GetFileDiff(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                if req.change.source == FileChangeSource::Agent {
                    ws.ensure_projection_loaded()?;
                }
                match ws.file_diff(&req.change) {
                    Ok(diff) => ResponseBody::Ok(serde_json::to_value(diff)?),
                    Err(message) => ResponseBody::Error(message),
                }
            }
            RequestBody::SetFilters(_) => ResponseBody::Error("filters not yet wired".to_string()),
            RequestBody::Search(req) => {
                let filters = match search_filters_from_dto(&req.filters) {
                    Ok(filters) => filters,
                    Err(msg) => {
                        return Ok(Response {
                            id,
                            body: ResponseBody::Error(msg),
                        });
                    }
                };
                // Build the lexical index lazily on first search.
                if self.lexical.is_none() {
                    let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                    ws.ensure_projection_loaded()?;
                    self.lexical = Some(build_lexical_index(ws)?);
                }
                let lexical = self.lexical.as_ref().ok_or("no index built")?;
                let results = lexical
                    .index
                    .search_internal(&req.query, &filters, req.top_k)?;
                let response = SearchResponse {
                    results: results
                        .iter()
                        .map(|chunk| search_hit_from_chunk(chunk, &lexical.git_identities))
                        .collect(),
                };
                ResponseBody::Ok(serde_json::to_value(response)?)
            }
            RequestBody::FindInHistory(req) => {
                let filters = match search_filters_from_dto(&req.filters) {
                    Ok(filters) => filters,
                    Err(msg) => {
                        return Ok(Response {
                            id,
                            body: ResponseBody::Error(msg),
                        });
                    }
                };
                // Build the lexical index lazily on first search.
                if self.lexical.is_none() {
                    let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                    ws.ensure_projection_loaded()?;
                    self.lexical = Some(build_lexical_index(ws)?);
                }
                let lexical = self.lexical.as_ref().ok_or("no index built")?;
                let chunks = lexical
                    .index
                    .search_internal(&req.query, &filters, req.top_k)?;
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                // The exact ChainFilterDto + hide_submodules the client used for
                // GetWindow/GetLayout, so resolution reuses the cached snapshot.
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let matches = ws.find_in_history(
                    &chunks,
                    &lexical.git_identities,
                    req.hide_submodules,
                    &filter,
                );
                // `more` reports only whether the candidate/top_k limit may have
                // truncated retrieval; the response never claims an exact total.
                let more = req.top_k > 0 && chunks.len() >= req.top_k;
                let response = FindInHistoryResponse {
                    returned: matches.len(),
                    more,
                    matches,
                };
                ResponseBody::Ok(serde_json::to_value(response)?)
            }
        };
        Ok(Response { id, body })
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
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
mod tests {
    use super::*;
    use editchain_codec::frame::encode_op;
    use editchain_codec::page::{encode_page, Page};
    use editchain_core::{ImportOp, MessageOp, PathId};
    use editchain_import::BlobSink as _;
    use std::collections::BTreeMap;

    /// 2^53 + 1 — the first integer JavaScript's IEEE-754 doubles round.
    const OVER_2_53: u64 = 9_007_199_254_740_993;

    /// Write ops into a chain directory as a single segment page.
    fn write_chain(chain_dir: &Path, ops: &[Op]) {
        let mut page = Page::new(0);
        for op in ops {
            page.add_record(0, encode_op(op).unwrap());
        }
        fs::create_dir_all(chain_dir).unwrap();
        fs::write(chain_dir.join("000000.eclog"), encode_page(&page)).unwrap();
    }

    /// Store a blob in a chain's durable blob store, returning its reference.
    fn store_blob(chain_dir: &Path, data: &[u8]) -> BlobRef {
        let mut blobs = FsBlobSink::new(chain_dir.join("blobs")).unwrap();
        blobs.put(data).unwrap()
    }

    /// Wrap `kind` in a standalone operation envelope.
    fn op_envelope(node: u64, seq: u64, kind: OpKind) -> Op {
        Op {
            id: OpId::new(NodeId(node), 0, seq),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(10)),
            tags: Tags::IMPORT,
            kind,
        }
    }

    /// Build a raw import op.
    fn import_op(node: u64, seq: u64, meta: bool) -> Op {
        let mut tags = Tags::IMPORT;
        if meta {
            tags |= Tags::META;
        }
        let record_type = if meta { "last-prompt" } else { "user" };
        Op {
            id: OpId::new(NodeId(node), 0, seq),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(10)),
            tags,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(
                    format!(r#"{{"type":"{record_type}","seq":{seq}}}"#).into_bytes(),
                ),
                raw_hash: None,
            }),
        }
    }

    /// Build a normalized message op whose parent is `parent`.
    fn message_op(node: u64, seq: u64, parent: OpId) -> Op {
        Op {
            id: OpId::new(NodeId(node), 0, seq),
            parents: ParentSet::One(parent),
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(10)),
            tags: Tags::HUMAN | Tags::MESSAGE,
            kind: OpKind::Message(MessageOp {
                content: Payload::Inline(b"hello world".to_vec()),
                content_type: Payload::Empty,
            }),
        }
    }

    /// Build a versioned exact relationship fact: causal parent `parent`,
    /// targets `targets`, META-tagged so the projection folds it out of rendered
    /// rows and reads it as a virtual edge.
    fn structural_note(
        id: OpId,
        parent: OpId,
        targets: Vec<OpId>,
        relationship: editchain_core::NoteRelationship,
        session: u64,
    ) -> Op {
        Op {
            id,
            parents: ParentSet::One(parent),
            actor: ActorId(0),
            clock: Clock::None,
            scope: ScopeRef::Session(SessionId(session)),
            tags: Tags::META | Tags::IMPORT,
            kind: OpKind::Note(editchain_core::op::NoteOp {
                target_ids: targets,
                relationship,
                content: Payload::Inline(
                    br#"{"confidence":"exact","resolver":"service-test-v1"}"#.to_vec(),
                ),
            }),
        }
    }

    #[test]
    fn history_window_exposes_structural_relationship_kinds() {
        // A parent thread (node 1) spawns a subagent thread (node 2) and
        // reconnects into it; a third thread (node 3) forks off the parent.
        // Exact relationship facts drive typed virtual edges in the projection;
        // the service must preserve their provider-neutral kinds.
        let trunk = import_op(1, 1, false);
        let spawn_marker = message_op(1, 3, trunk.id);
        let sub_meta = import_op(2, 1, true);
        let mut sub_first = import_op(2, 2, false);
        sub_first.parents = ParentSet::One(sub_meta.id);
        let sub_last = message_op(2, 5, sub_first.id);
        let completion = message_op(1, 7, spawn_marker.id);
        let branch_first = import_op(3, 1, false);

        let ops = vec![
            trunk.clone(),
            spawn_marker.clone(),
            sub_meta.clone(),
            sub_first.clone(),
            sub_last.clone(),
            completion.clone(),
            branch_first.clone(),
            structural_note(
                OpId::new(NodeId(1), 0, 0xFFFC),
                sub_meta.id,
                vec![spawn_marker.id],
                editchain_core::NoteRelationship::SpawnedBy,
                2,
            ),
            structural_note(
                OpId::new(NodeId(1), 0, 0xFFFB),
                completion.id,
                vec![sub_last.id],
                editchain_core::NoteRelationship::ReconnectsTo,
                1,
            ),
            structural_note(
                OpId::new(NodeId(1), 0, 0xFFFA),
                branch_first.id,
                vec![trunk.id],
                editchain_core::NoteRelationship::ForkOf,
                3,
            ),
        ];
        let options = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection = HistoryProjection::from_ops_with(ops, options);
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::default();
        let window = ws.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            hide_submodules: false,
            filter: &filter,
            include_layout: true,
        });

        // SpawnedBy: the exact anchor is bundled session metadata, matching a
        // Codex child rollout. Its first surviving row carries the "subagent"
        // relation to the CANONICAL spawn anchor. The raw target (the folded
        // spawn marker op) resolves through the representative map to the
        // trunk's visible import row, which is the parent the row actually
        // renders.
        let sub_row = window
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(sub_first.id.to_string().as_str()))
            .expect("subagent first op row");
        assert_eq!(sub_row.parents, vec![trunk.id.to_string()]);
        assert_eq!(
            sub_row.parent_relations,
            vec![ParentRelationDto {
                parent: trunk.id.to_string(),
                kind: ParentRelationKind::Subagent,
            }]
        );

        // ReconnectsTo: the parent thread's completion row (a standalone
        // message row on the trunk) carries a "reconnect" relation back into
        // the subagent's last op — again resolved to the visible subagent
        // import row. Its stored parent (the folded spawn marker) lifts to the
        // trunk row, so both parent keys are canonical visible rows.
        let completion_row = window
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(completion.id.to_string().as_str()))
            .expect("completion row");
        assert_eq!(
            completion_row.parents,
            vec![trunk.id.to_string(), sub_first.id.to_string()]
        );
        assert_eq!(
            completion_row.parent_relations,
            vec![ParentRelationDto {
                parent: sub_first.id.to_string(),
                kind: ParentRelationKind::Reconnect,
            }]
        );

        // ForkOf: the fork thread's first op carries a "fork" relation to the
        // trunk op it branches off.
        let fork_row = window
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(branch_first.id.to_string().as_str()))
            .expect("fork first op row");
        assert_eq!(fork_row.parents, vec![trunk.id.to_string()]);
        assert_eq!(
            fork_row.parent_relations,
            vec![ParentRelationDto {
                parent: trunk.id.to_string(),
                kind: ParentRelationKind::Fork,
            }]
        );

        // The trunk row itself carries no structural relations (no note is
        // anchored on it), the structural notes never render as rows, and no
        // row carries a stale relation to a folded op id.
        let trunk_row = window
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(trunk.id.to_string().as_str()))
            .expect("trunk row");
        assert!(trunk_row.parents.is_empty());
        assert!(trunk_row.parent_relations.is_empty());
        assert!(
            window.rows.iter().all(|r| r.kind != "note"),
            "structural notes must never render as rows"
        );
        for row in &window.rows {
            for rel in &row.parent_relations {
                assert!(
                    row.parents.contains(&rel.parent),
                    "relation.parent {} must be one of the row's parents {:?}",
                    rel.parent,
                    row.parents
                );
            }
        }
    }

    #[test]
    fn include_kind_filter_preserves_structural_anchor_and_target_rows() {
        // "Messages only" is an INCLUSIVE kind constraint: ordinary non-message
        // rows are excluded. Structural relation anchors and targets are
        // graph-topology-critical, so their rows must survive even
        // when their kind (tool) matches the exclusion — otherwise the
        // branch/reconnect geometry disappears. Here a parent thread's spawn
        // marker (a Tool op) and the subagent's first op (also a Tool op) are
        // both preserved, the SubagentOf edge still renders, and the relation
        // parent is one of the row's final parents.
        let spawn = op_envelope(
            1,
            1,
            OpKind::Tool(editchain_core::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Inline(b"Task".to_vec()),
                stage: editchain_core::ToolStage::Start,
                content: Payload::Empty,
            }),
        );
        let sub_first = op_envelope(
            2,
            1,
            OpKind::Tool(editchain_core::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Inline(b"Bash".to_vec()),
                stage: editchain_core::ToolStage::Start,
                content: Payload::Empty,
            }),
        );
        let ops = vec![
            spawn.clone(),
            sub_first.clone(),
            structural_note(
                OpId::new(NodeId(9), 0, 1),
                sub_first.id,
                vec![spawn.id],
                editchain_core::NoteRelationship::SubagentOf,
                10,
            ),
        ];
        let projection = HistoryProjection::from_ops(ops);
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::new(
            String::new(),
            String::new(),
            "^message$".to_string(),
            false,
            true,
            false,
        );
        let window = ws.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            hide_submodules: false,
            filter: &filter,
            include_layout: true,
        });

        // Both tool-kind rows are preserved because they are a structural
        // anchor/target pair; every other row kind is excluded.
        let sub_row = window
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(sub_first.id.to_string().as_str()))
            .expect("subagent first op row preserved");
        let spawn_row = window
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(spawn.id.to_string().as_str()))
            .expect("spawn marker row preserved");
        assert_eq!(sub_row.parents, vec![spawn.id.to_string()]);
        assert_eq!(
            sub_row.parent_relations,
            vec![ParentRelationDto {
                parent: spawn.id.to_string(),
                kind: ParentRelationKind::Subagent,
            }]
        );
        assert!(spawn_row.parent_relations.is_empty());

        // The layout for the SAME filtered view still draws the SubagentOf
        // edge between the two visible rows.
        let layout = ws.graph_layout(false, 0, 100, &filter);
        assert!(
            layout.edges.iter().any(|e| {
                e.child == sub_first.id.to_string() && e.parent == spawn.id.to_string()
            }),
            "filtered layout must draw the SubagentOf edge; got {:#?}",
            layout
                .edges
                .iter()
                .map(|e| (e.child.as_str(), e.parent.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn history_window_bundles_meta_subops() {
        // A real turn (import + message), then a META import. The META import
        // bundles into the turn's row as a sub-op; the service emits it as its
        // own expanded row immediately after the parent.
        let turn = import_op(1, 1, false);
        let msg = message_op(1, 2, turn.id);
        let meta = Op {
            parents: ParentSet::One(turn.id),
            ..import_op(1, 3, true)
        };

        // q6 Phase-1: bundling is driven by explicit ProjectionOptions, not a global
        // toggle — no mutex needed; each projection is independently configured.
        let opts = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection =
            HistoryProjection::from_ops_with(vec![turn.clone(), msg, meta.clone()], opts);
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::default();
        let window = ws.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            hide_submodules: false,
            filter: &filter,
            include_layout: true,
        });

        // Parent row + one expanded sub-op row.
        assert_eq!(window.rows.len(), 2);
        assert_eq!(window.total, 2);
        assert_eq!(window.chain_generation, 3);
        assert_eq!(window.sub_op_counts.as_deref(), Some(&[1][..]));
        // Parent carries the bundled sub-op summary (for the collapsed chevron).
        assert_eq!(window.rows[0].sub_ops.len(), 1);
        assert_eq!(window.rows[0].sub_ops[0].op_id, meta.id.to_string());
        assert_eq!(window.rows[0].sub_ops[0].kind, "last-prompt");
        assert!(!window.rows[0].is_subop);
        assert!(window.rows[0].group_end);
        assert_eq!(window.rows[0].parent_row, None);
        // The expanded sub-op row follows its parent and inherits its lane.
        assert!(window.rows[1].is_subop);
        assert!(!window.rows[1].group_end);
        assert_eq!(window.rows[1].parent_row, Some(0));
        assert_eq!(
            window.rows[1].op_id.as_deref(),
            Some(meta.id.to_string().as_str())
        );
        assert_eq!(window.rows[1].subop_kind.as_deref(), Some("msg"));

        // A page beginning inside an expanded block must still resolve the
        // owning top-level node. Global expansion metadata is sent only on the
        // offset-zero page and retained by the client for later windows.
        let deep = ws.history_window(HistoryWindowOptions {
            offset: 1,
            limit: 1,
            hide_submodules: false,
            filter: &filter,
            include_layout: true,
        });
        assert_eq!(deep.rows.len(), 1);
        assert!(deep.rows[0].is_subop);
        assert!(!deep.rows[0].group_end);
        assert_eq!(deep.rows[0].op_id, Some(meta.id.to_string()));
        assert!(deep.sub_op_counts.is_none());
    }

    #[test]
    fn meta_bundle_default_standalone_opt_in_bundles() {
        // META imports render standalone by default (no cross-session grouping).
        // Only when META bundling is re-enabled do they contract along their
        // exact stored parent into an expanded sub-op row.
        let turn = import_op(1, 1, false);
        let msg = message_op(1, 2, turn.id);
        let meta = Op {
            parents: ParentSet::One(turn.id),
            ..import_op(1, 3, true)
        };

        // Default (bundling off): META is a standalone top-level row.
        let projection_off =
            HistoryProjection::from_ops(vec![turn.clone(), msg.clone(), meta.clone()]);
        let mut ws_off = Workspace::from_projection(projection_off);
        let filter = ChainFilter::default();
        let window_off = ws_off.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            hide_submodules: false,
            filter: &filter,
            include_layout: true,
        });
        let meta_default = window_off
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(meta.id.to_string().as_str()));
        assert!(
            meta_default.is_some(),
            "META import must appear as a row by default"
        );
        assert!(
            !meta_default.unwrap().is_subop,
            "META import must be a standalone top-level row by default (not a sub-op)"
        );

        // Opt-in (bundling on, fresh projection so the per-filter node cache is
        // not reused): the same META op renders as an expanded sub-op row.
        let opts_on = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection_on =
            HistoryProjection::from_ops_with(vec![turn.clone(), msg, meta.clone()], opts_on);
        let mut ws_on = Workspace::from_projection(projection_on);
        let window_on = ws_on.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            hide_submodules: false,
            filter: &filter,
            include_layout: true,
        });
        let meta_on = window_on
            .rows
            .iter()
            .find(|r| r.op_id.as_deref() == Some(meta.id.to_string().as_str()));
        assert!(
            meta_on.is_some_and(|r| r.is_subop),
            "META import must be an expanded sub-op row when bundling is enabled"
        );
    }

    #[test]
    fn sub_op_rows_draw_pass_through_lanes() {
        // A linear chain where a middle turn carries a bundled META sub-op and
        // has both a child above and a parent below on its own lane. The sub-op
        // row must carry that lane as pass-through (above == below), so the
        // client draws it as a full-height straight line with no dot.
        //
        // Build newest-first by clock:
        //   child (seq high) -> turn+meta (middle) -> parent (low).
        let parent = message_op(5, 4, OpId::new(NodeId(5), 0, 3));
        let turn = import_op(5, 5, false);
        let turn_with_parent = Op {
            parents: ParentSet::One(parent.id),
            ..turn.clone()
        };
        let msg = message_op(5, 6, turn.id); // child of turn
        let meta = Op {
            parents: ParentSet::One(turn.id),
            ..import_op(5, 7, true)
        }; // bundled under turn

        let opts = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection = HistoryProjection::from_ops_with(
            vec![
                msg.clone(),
                turn_with_parent.clone(),
                meta.clone(),
                parent.clone(),
            ],
            opts,
        );
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::default();
        let window = ws.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            hide_submodules: false,
            filter: &filter,
            include_layout: true,
        });

        // Find the sub-op row (is_subop).
        let sub = window
            .rows
            .iter()
            .find(|r| r.is_subop)
            .expect("sub-op row present");
        // The sub-op row must have at least one pass-through lane (its own
        // parent's lane), and above == below so the client draws a full line.
        assert!(!sub.above.is_empty());
        assert_eq!(sub.above, sub.below);
    }

    #[test]
    fn sub_op_label_parses_record_type() {
        let op = import_op(1, 1, true);
        let (summary, kind) = sub_op_label(&op);
        assert_eq!(summary, "last-prompt");
        assert_eq!(kind, "last-prompt");
    }

    #[test]
    fn sub_op_label_uses_event_payload_type() {
        let op = op_envelope(
            1,
            1,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(
                    br#"{"type":"event_msg","payload":{"type":"task_complete"}}"#.to_vec(),
                ),
                raw_hash: None,
            }),
        );
        let (summary, kind) = sub_op_label(&op);
        assert_eq!(summary, "task_complete");
        assert_eq!(kind, "task_complete");
    }

    #[test]
    fn sub_op_label_preserves_folded_claude_tool_fragment() {
        let op = op_envelope(
            1,
            1,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(
                    br#"{"type":"assistant","message":{"id":"msg-1","content":[{"type":"tool_use","name":"Read"}]}}"#
                        .to_vec(),
                ),
                raw_hash: None,
            }),
        );
        let (summary, kind) = sub_op_label(&op);
        assert_eq!(summary, "tool: Read");
        assert_eq!(kind, "tool");
    }

    #[test]
    fn sub_op_label_renders_tool_result_preview() {
        // A tool-result sub-op (Tool, Finish) should render a content preview.
        let op = Op {
            id: OpId::new(NodeId(1), 0, 1),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(1),
            scope: ScopeRef::Session(SessionId(10)),
            tags: Tags::TOOL,
            kind: OpKind::Tool(editchain_core::op::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Empty,
                stage: editchain_core::op::ToolStage::Finish,
                content: Payload::Inline(b"1\tline one\n2\tline two".to_vec()),
            }),
        };
        let (summary, kind) = sub_op_label(&op);
        assert_eq!(summary, "line one");
        assert_eq!(kind, "tool_result");
    }

    /// Diagnostic (not run in CI): load a real chain and report how many
    /// independent chains the projection produces, comparing raw source chains
    /// (`(OpId.node, OpId.boot)` streams) vs the bundled projection's own
    /// connected-root count against `metadata` on/off.
    #[test]
    #[ignore = "manual diagnostics against a real chain"]
    #[expect(
        clippy::print_stderr,
        reason = "manual diagnostics deliberately print raw chain-level counts to stderr"
    )]
    fn diag_chain_counts() {
        let (ops, _stats, _locations) = read_chain_ops(&PathBuf::from(
            "/mnt/hot/ambientlight/repos/editchain/.editchain",
        ))
        .unwrap();
        eprintln!("\n=== chain diag: {} ops ===", ops.len());

        // Raw source chains = distinct (node, boot) streams among Import ops.
        let meta_imports = ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Import(_)))
            .filter(|o| o.tags.matches_any(Tags::META))
            .count();
        eprintln!("meta-tagged raw imports: {meta_imports}");
        // Show the import tag bitmask distribution so we can see what the old
        // importer actually stamped (META may be encoded differently or absent).
        let mut tag_hist: HashMap<u64, usize> = HashMap::new();
        for o in ops.iter().filter(|o| matches!(o.kind, OpKind::Import(_))) {
            *tag_hist.entry(o.tags.0).or_default() += 1;
        }
        let mut sorted_tags: Vec<(u64, usize)> = tag_hist.into_iter().collect();
        sorted_tags.sort_by_key(|(t, _)| *t);
        for (t, c) in sorted_tags.iter().take(12) {
            eprintln!("  import tags {t:#016b} x{c}");
        }

        let sources: std::collections::HashSet<(u64, u32)> = ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Import(_)))
            .map(|o| (o.id.node.0, o.id.boot))
            .collect();
        eprintln!(
            "raw source streams (node,boot) among import ops: {}",
            sources.len()
        );

        // Distinct raw import roots (SnapshotTopology: count nodes with no parent
        // present among import ops) — how many chains the importer actually made.
        let import_ids: std::collections::HashSet<OpId> = ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Import(_)))
            .map(|o| o.id)
            .collect();
        let import_roots = ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Import(_)))
            .filter(|o| {
                o.parents.iter().all(|p| !import_ids.contains(p)) // no import parent present => root
            })
            .count();
        eprintln!("raw import roots (no import parent): {import_roots}");

        // Projection counts.
        for (label, bundle) in [("meta OFF", false), ("meta ON", true)] {
            let proj = HistoryProjection::from_ops_with(
                ops.clone(),
                editchain_project::ProjectionOptions {
                    bundle_metadata: bundle,
                },
            );
            let rows = proj.nodes().len();
            let chains = proj.independent_chains();
            eprintln!("{label}: top_rows={rows} chains={chains}");
            if bundle {
                // Client-view root count over one shared meta-ON projection: how
                // many top-level rows have NO parent that resolves to a present
                // row, using the SAME lifted `parents` the service emits in
                // HistoryRow. This is what actually renders as a distinct chain.
                let nodes = proj.nodes();
                let present: std::collections::HashSet<String> = nodes
                    .iter()
                    .map(editchain_project::HistoryNode::node_key)
                    .collect();
                let mut client_roots = 0usize;
                for node in &nodes {
                    let lifted = proj.lifted_parent_keys(node);
                    if lifted.iter().all(|p| !present.contains(p)) {
                        client_roots += 1;
                    }
                }
                eprintln!("meta ON client-view roots (lifted parents): {client_roots}");
            }
        }
    }

    #[test]
    fn open_previews_blobs_and_hydrates_details_and_search_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_path = dir.path().join("workspace");
        fs::create_dir_all(&workspace_path).unwrap();
        let chain_dir = workspace_path.join(".editchain");

        // Payloads well past INLINE_LIMIT (4096) so the importer would spill
        // them into durable blobs.
        let msg_content = format!("needle-hydrated-message {}", "x".repeat(8192)).into_bytes();
        let tool_content = format!("needle-hydrated-tool {}", "y".repeat(8192)).into_bytes();
        let raw_content = format!("needle-hydrated-raw {}", "z".repeat(8192)).into_bytes();
        let msg_ref = store_blob(&chain_dir, &msg_content);
        let tool_ref = store_blob(&chain_dir, &tool_content);
        let raw_ref = store_blob(&chain_dir, &raw_content);

        let msg = op_envelope(
            1,
            1,
            OpKind::Message(MessageOp {
                content: Payload::Blob(msg_ref),
                content_type: Payload::Empty,
            }),
        );
        let tool = op_envelope(
            1,
            2,
            OpKind::Tool(editchain_core::op::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Empty,
                stage: editchain_core::op::ToolStage::Finish,
                content: Payload::Blob(tool_ref),
            }),
        );
        let raw = op_envelope(
            1,
            3,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Blob(raw_ref),
                raw_hash: None,
            }),
        );
        write_chain(&chain_dir, &[msg.clone(), tool.clone(), raw.clone()]);

        // Reopen the workspace over the durable chain.
        let ws = Workspace::open(workspace_path.to_str().unwrap(), ".editchain").unwrap();
        assert_eq!(ws.diagnostics.chain.records, 3);
        assert_eq!(ws.diagnostics.chain.accepted, 3);
        assert_eq!(ws.diagnostics.blobs.hydrated, 0);
        assert_eq!(ws.diagnostics.blobs.previewed, 3);
        assert_eq!(ws.diagnostics.blobs.deferred, 3);
        assert_eq!(ws.diagnostics.blobs.missing, 0);
        assert_eq!(ws.diagnostics.blobs.corrupt, 0);
        assert!(ws.diagnostics.warnings().is_empty());

        // NodeDetails hydrates the requested source operation on demand.
        let details = ws.node_details(Some(msg.id.to_string()), None).unwrap();
        assert!(details.body.contains("needle-hydrated-message"));
        let tool_details = ws.node_details(Some(tool.id.to_string()), None).unwrap();
        assert!(tool_details.body.contains("needle-hydrated-tool"));
        let raw_details = ws.node_details(Some(raw.id.to_string()), None).unwrap();
        assert!(raw_details.summary.contains("needle-hydrated-raw"));

        // Search hydrates each source operation while lazily building its index.
        let state = build_lexical_index(&ws).unwrap();
        let filters = SearchFilters {
            kinds: None,
            sources: None,
            sessions: None,
            actors: None,
            paths: None,
            after: None,
            before: None,
            include_raw: false,
            include_private: false,
        };
        let results = state
            .index
            .search_internal("needle-hydrated-message", &filters, 5)
            .unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.op_id == msg.id));
    }

    #[test]
    fn git_search_hits_use_index_identity_map_not_synthetic_ids() {
        // A real commit in a repository whose id exceeds 2^53, so the identity
        // must round-trip as an exact decimal string.
        let mut bytes = [0u8; 32];
        bytes[0] = 0xaa;
        let oid = GitOid::new(editchain_core::GitObjectFormat::Sha1, bytes);
        let commit = editchain_core::GitCommitEntity {
            repository: RepositoryId(OVER_2_53),
            object_format: editchain_core::GitObjectFormat::Sha1,
            oid,
            imported_record: None,
            availability: editchain_core::GitAvailability::Resolved,
            tree: oid,
            parents: Vec::new(),
            author: editchain_core::GitSignature {
                name: Payload::Inline(b"Alice".to_vec()),
                email: Payload::Inline(b"alice@example.com".to_vec()),
                when: 0,
            },
            committer: editchain_core::GitSignature {
                name: Payload::Inline(b"Alice".to_vec()),
                email: Payload::Inline(b"alice@example.com".to_vec()),
                when: 0,
            },
            authored_at: 0,
            committed_at: 0,
            message: Payload::Inline(b"needle-git-identity".to_vec()),
            imported_refs: Vec::new(),
            live_refs: Vec::new(),
            changed_paths: Vec::new(),
        };
        let mut projection = HistoryProjection::new();
        projection.merge_git_commits(vec![commit]);
        let ws = Workspace::from_projection(projection);

        let state = build_lexical_index(&ws).unwrap();
        let filters = SearchFilters {
            kinds: None,
            sources: None,
            sessions: None,
            actors: None,
            paths: None,
            after: None,
            before: None,
            include_raw: false,
            include_private: false,
        };
        let results = state
            .index
            .search_internal("needle-git-identity", &filters, 5)
            .unwrap();
        let hit = results
            .iter()
            .find(|r| r.metadata.source == Source::Git)
            .expect("git hit");

        // The synthetic indexed op id deterministically maps to the real
        // commit identity — never guessed at response time.
        let identity = state
            .git_identities
            .get(&hit.op_id)
            .expect("identity map entry for synthetic op id");
        assert_eq!(identity.oid, oid);
        assert_eq!(identity.repository_id.0, OVER_2_53);
        assert!(!identity.is_submodule);

        let dto = search_hit_from_chunk(hit, &state.git_identities);
        assert_eq!(dto.git_oid.as_deref(), Some(oid.to_hex().as_str()));
        assert_eq!(
            dto.repository.as_deref(),
            Some(OVER_2_53.to_string().as_str())
        );
        assert_eq!(dto.kind, "git");
        assert!(!dto.is_submodule);
        // The synthetic op id is present in the envelope but is NOT a
        // projection node: GetNodeDetails must not resolve it.
        assert_eq!(dto.op_id, hit.op_id.to_string());
        assert!(ws.node_details(Some(dto.op_id.clone()), None).is_none());
    }

    #[test]
    fn open_preserves_missing_and_corrupt_blob_refs_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_path = dir.path().join("workspace");
        fs::create_dir_all(&workspace_path).unwrap();
        let chain_dir = workspace_path.join(".editchain");

        // Missing: the reference is never stored.
        let missing_data = b"never-stored-content".to_vec();
        let missing_ref = BlobRef {
            id: ContentId::Hash256(hash_raw(&missing_data)),
            len: u32::try_from(missing_data.len()).unwrap(),
        };

        // Corrupt by content: the file exists but holds different bytes.
        let corrupt_data = b"corrupt-original-content".to_vec();
        let corrupt_hash = hash_raw(&corrupt_data);
        let corrupt_ref = BlobRef {
            id: ContentId::Hash256(corrupt_hash),
            len: u32::try_from(corrupt_data.len()).unwrap(),
        };
        let sink = FsBlobSink::new(chain_dir.join("blobs")).unwrap();
        fs::write(sink.path_for(&corrupt_hash), b"corrupted bytes").unwrap();

        // Corrupt by length: correct bytes but a lying declared length.
        let len_data = b"valid-length-content".to_vec();
        let len_ref = BlobRef {
            id: ContentId::Hash256(hash_raw(&len_data)),
            len: u32::try_from(len_data.len()).unwrap().saturating_add(1),
        };
        let _: BlobRef = store_blob(&chain_dir, &len_data);

        // Unresolvable: a local node ref cannot be addressed by this store.
        let local_ref = BlobRef {
            id: ContentId::Local {
                node: NodeId(1),
                seq: 7,
            },
            len: 3,
        };

        let missing_msg = op_envelope(
            1,
            1,
            OpKind::Message(MessageOp {
                content: Payload::Blob(missing_ref),
                content_type: Payload::Empty,
            }),
        );
        let corrupt_msg = op_envelope(
            1,
            2,
            OpKind::Message(MessageOp {
                content: Payload::Blob(corrupt_ref),
                content_type: Payload::Empty,
            }),
        );
        let len_msg = op_envelope(
            1,
            3,
            OpKind::Message(MessageOp {
                content: Payload::Blob(len_ref),
                content_type: Payload::Empty,
            }),
        );
        let local_msg = op_envelope(
            1,
            4,
            OpKind::Message(MessageOp {
                content: Payload::Blob(local_ref),
                content_type: Payload::Empty,
            }),
        );
        write_chain(
            &chain_dir,
            &[
                missing_msg.clone(),
                corrupt_msg.clone(),
                len_msg.clone(),
                local_msg.clone(),
            ],
        );

        // The open succeeds; every unhydrated payload stays a Blob ref.
        let mut server = Server::new();
        let request = Request {
            id: 1,
            body: RequestBody::Open(editchain_protocol::OpenRequest {
                workspace_path: workspace_path.to_string_lossy().to_string(),
                chain_dir: ".editchain".to_string(),
            }),
        };
        let response = server.handle(&request).unwrap();
        assert!(
            matches!(&response.body, ResponseBody::Ok(_)),
            "open unexpectedly failed"
        );
        let value = match response.body {
            ResponseBody::Ok(value) => value,
            ResponseBody::Error(_) => return,
        };
        let diagnostics = value.get("diagnostics").unwrap();
        assert_eq!(diagnostics["blobs"]["hydrated"], 0);
        assert_eq!(diagnostics["blobs"]["missing"], 1);
        assert_eq!(diagnostics["blobs"]["corrupt"], 2);
        assert_eq!(diagnostics["blobs"]["unresolved"], 1);
        assert!(!value["warnings"].as_array().unwrap().is_empty());

        let ws = server.workspace.as_ref().unwrap();
        for op in &ws.projection.ops {
            if let OpKind::Message(message) = &op.kind {
                assert!(matches!(message.content, Payload::Blob(_)));
            }
        }
        // Details must not claim hydrated content for preserved refs (missing
        // payloads surface as empty text, not fabricated content).
        let details = ws
            .node_details(Some(missing_msg.id.to_string()), None)
            .unwrap();
        assert_eq!(details.body, "");
    }

    #[test]
    fn open_canonicalizes_replays_and_quarantines_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_path = dir.path().join("workspace");
        fs::create_dir_all(&workspace_path).unwrap();
        let chain_dir = workspace_path.join(".editchain");

        let first = op_envelope(
            1,
            1,
            OpKind::Message(MessageOp {
                content: Payload::Inline(b"first-payload".to_vec()),
                content_type: Payload::Empty,
            }),
        );
        let replayed = first.clone();
        let conflicting = Op {
            kind: OpKind::Message(MessageOp {
                content: Payload::Inline(b"conflicting-payload".to_vec()),
                content_type: Payload::Empty,
            }),
            ..first.clone()
        };
        let second = op_envelope(
            1,
            2,
            OpKind::Message(MessageOp {
                content: Payload::Inline(b"second-payload".to_vec()),
                content_type: Payload::Empty,
            }),
        );
        write_chain(
            &chain_dir,
            &[first.clone(), replayed, conflicting.clone(), second.clone()],
        );

        let ws = Workspace::open(workspace_path.to_str().unwrap(), ".editchain").unwrap();
        assert_eq!(ws.diagnostics.chain.records, 4);
        assert_eq!(ws.diagnostics.chain.accepted, 2);
        assert_eq!(ws.diagnostics.chain.duplicates, 1);
        assert_eq!(ws.diagnostics.chain.quarantined, 1);
        assert_eq!(ws.projection.ops.len(), 2);
        assert_eq!(ws.diagnostics.warnings().len(), 2);

        // The quarantined conflict must not replace the accepted payload silently.
        let details = ws.node_details(Some(first.id.to_string()), None).unwrap();
        assert!(details.body.contains("first-payload"));
        assert!(!details.body.contains("conflicting-payload"));
    }

    #[test]
    fn hydrate_traverses_every_payload_bearing_field() {
        let dir = tempfile::tempdir().unwrap();
        let chain_dir = dir.path().join("chain");
        let mut blobs = FsBlobSink::new(chain_dir.join("blobs")).unwrap();
        let file_blob_ref = blobs.put(b"full-file-content").unwrap();
        let resolver = BlobResolver::open(&chain_dir).unwrap();
        let mut blob = |data: &[u8]| Payload::Blob(blobs.put(data).unwrap());
        let git_oid = || GitOid::new(editchain_core::GitObjectFormat::Sha1, [0u8; 32]);

        let mut ops = vec![
            op_envelope(
                1,
                1,
                OpKind::Actor(editchain_core::op::ActorOp {
                    label: blob(b"actor-label"),
                    role: blob(b"actor-role"),
                }),
            ),
            op_envelope(
                1,
                2,
                OpKind::Message(MessageOp {
                    content: blob(b"msg-content"),
                    content_type: blob(b"msg-type"),
                }),
            ),
            op_envelope(
                1,
                3,
                OpKind::Tool(editchain_core::op::ToolOp {
                    tool_call_id: blob(b"tool-call-id"),
                    tool_name: blob(b"tool-name"),
                    stage: editchain_core::op::ToolStage::Start,
                    content: blob(b"tool-content"),
                }),
            ),
            op_envelope(
                1,
                4,
                OpKind::Command(editchain_core::op::CommandOp {
                    command_id: blob(b"cmd-id"),
                    content: blob(b"cmd-content"),
                    stage: editchain_core::op::CommandStage::Start,
                }),
            ),
            op_envelope(
                1,
                5,
                OpKind::File(editchain_core::op::FileOp {
                    path: PathId(1),
                    stage: editchain_core::op::FileStage::Observed,
                    base: None,
                    after: None,
                    edit: editchain_core::op::FileEdit::ReplaceBytes {
                        range: editchain_core::op::ByteRange { start: 0, end: 4 },
                        bytes: blob(b"replace-bytes"),
                    },
                }),
            ),
            op_envelope(
                1,
                6,
                OpKind::File(editchain_core::op::FileOp {
                    path: PathId(2),
                    stage: editchain_core::op::FileStage::Observed,
                    base: None,
                    after: None,
                    edit: editchain_core::op::FileEdit::UnifiedDiff(blob(b"unified-diff")),
                }),
            ),
            op_envelope(
                1,
                7,
                OpKind::Reflection(editchain_core::ReflectionOp {
                    scope: ScopeRef::None,
                    covers: editchain_core::FrontierSet::new(),
                    window: editchain_core::WindowRef {
                        start_seq: 0,
                        end_seq: 0,
                    },
                    summary: blob(b"reflection-summary"),
                    anchors: blob(b"reflection-anchors"),
                }),
            ),
            op_envelope(
                1,
                8,
                OpKind::Import(ImportOp {
                    raw_ref: blob(b"raw-ref"),
                    raw_hash: None,
                }),
            ),
            op_envelope(
                1,
                9,
                OpKind::Note(editchain_core::op::NoteOp {
                    target_ids: Vec::new(),
                    relationship: editchain_core::op::NoteRelationship::Explains,
                    content: blob(b"note-content"),
                }),
            ),
            op_envelope(
                1,
                10,
                OpKind::Error(editchain_core::op::ErrorOp {
                    code: blob(b"err-code"),
                    message: blob(b"err-message"),
                }),
            ),
            op_envelope(
                1,
                11,
                OpKind::Unknown(editchain_core::op::UnknownOp {
                    kind_discriminant: 0xFF,
                    raw_bytes: blob(b"unknown-raw"),
                }),
            ),
            op_envelope(
                1,
                12,
                OpKind::GitCommit(Box::new(editchain_core::GitCommitEntity {
                    repository: RepositoryId(0),
                    object_format: editchain_core::GitObjectFormat::Sha1,
                    oid: git_oid(),
                    imported_record: None,
                    availability: editchain_core::GitAvailability::ImportedOnly,
                    tree: git_oid(),
                    parents: Vec::new(),
                    author: editchain_core::GitSignature {
                        name: blob(b"author-name"),
                        email: blob(b"author-email"),
                        when: 0,
                    },
                    committer: editchain_core::GitSignature {
                        name: blob(b"committer-name"),
                        email: blob(b"committer-email"),
                        when: 0,
                    },
                    authored_at: 0,
                    committed_at: 0,
                    message: blob(b"commit-message"),
                    imported_refs: vec![blob(b"imported-ref")],
                    live_refs: vec![blob(b"live-ref")],
                    changed_paths: Vec::new(),
                })),
            ),
            op_envelope(
                1,
                13,
                OpKind::GitLink(editchain_core::GitLink {
                    source: OpId::new(NodeId(1), 0, 0),
                    target_repo: RepositoryId(0),
                    target_oid: git_oid(),
                    kind: editchain_core::GitLinkKind::Custom(blob(b"custom-link")),
                }),
            ),
            op_envelope(
                1,
                14,
                OpKind::File(editchain_core::op::FileOp {
                    path: PathId(3),
                    stage: editchain_core::op::FileStage::Observed,
                    base: None,
                    after: None,
                    edit: editchain_core::op::FileEdit::Blob(file_blob_ref),
                }),
            ),
        ];

        let stats = hydrate_blob_payloads(&mut ops, &resolver);
        // 2 actor + 2 message + 3 tool + 2 command + 1 replace-bytes + 1 diff
        // + 2 reflection + 1 import + 1 note + 2 error + 1 unknown + 7 commit
        // (message/author/committer/refs) + 1 custom link.
        assert_eq!(stats.hydrated, 26);
        // The file edit blob has no inline representation: validated, preserved.
        assert_eq!(stats.verified_refs, 1);
        assert_eq!(stats.missing, 0);
        assert_eq!(stats.corrupt, 0);
        assert_eq!(stats.unresolved, 0);

        // Spot-check nested hydration: message content inline, and the blob
        // edit variant preserved (validated, never rewritten into a synthetic
        // ReplaceBytes range).
        let message_op = ops.iter().find(|op| op.id.seq == 2).unwrap();
        let message_hydrated = matches!(
            &message_op.kind,
            OpKind::Message(MessageOp { content, .. })
                if content == &Payload::Inline(b"msg-content".to_vec())
        );
        assert!(message_hydrated, "message content not hydrated inline");
        let blob_edit_op = ops.iter().find(|op| op.id.seq == 14).unwrap();
        let blob_edit_preserved = matches!(
            &blob_edit_op.kind,
            OpKind::File(file_op)
                if matches!(
                    &file_op.edit,
                    editchain_core::op::FileEdit::Blob(blob_ref) if *blob_ref == file_blob_ref
                )
        );
        assert!(
            blob_edit_preserved,
            "valid file blob edit must stay FileEdit::Blob"
        );
    }

    #[test]
    fn projection_previews_defer_full_blob_until_details() {
        let tmp = tempfile::tempdir().unwrap();
        let full_text = "large payload ".repeat(2_000);
        let blob_ref = store_blob(tmp.path(), full_text.as_bytes());
        let source = op_envelope(
            9,
            1,
            OpKind::Message(MessageOp {
                content: Payload::Blob(blob_ref),
                content_type: Payload::Empty,
            }),
        );
        let resolver = BlobResolver::open(tmp.path()).unwrap();
        let (projection_ops, stats) =
            projection_ops_with_previews(std::slice::from_ref(&source), &resolver);

        assert_eq!(stats.hydrated, 0);
        assert_eq!(stats.previewed, 1);
        assert_eq!(stats.deferred, 1);
        assert!(matches!(
            &source.kind,
            OpKind::Message(MessageOp {
                content: Payload::Blob(found),
                ..
            }) if found == &blob_ref
        ));
        let preview_len = if let OpKind::Message(MessageOp {
            content: Payload::Inline(bytes),
            ..
        }) = &projection_ops[0].kind
        {
            String::from_utf8_lossy(bytes).chars().count()
        } else {
            0
        };
        assert_ne!(preview_len, 0, "expected inline projection preview");
        assert!(preview_len <= DISPLAY_PREVIEW_CHAR_LIMIT.saturating_add(1));

        let projection = HistoryProjection::from_ops(projection_ops);
        let mut source_op_index = HashMap::new();
        let _: Option<usize> = source_op_index.insert(source.id, 0);
        let ws = Workspace {
            projection,
            source_ops: vec![source.clone()],
            source_op_index,
            session_metadata: HashMap::new(),
            agent_file_changes: HashMap::new(),
            git_file_changes: HashMap::new(),
            source_op_locations: Vec::new(),
            blob_resolver: Some(resolver),
            repositories: Vec::new(),
            diagnostics: OpenDiagnostics::default(),
            root_path: PathBuf::new(),
            chain_path: tmp.path().to_path_buf(),
            snapshot: None,
            projection_loaded: true,
            current_view: None,
        };
        let details = ws
            .node_details(Some(source.id.to_string()), None)
            .expect("details");
        assert_eq!(details.body, full_text);
    }

    #[test]
    fn compact_import_record_preserves_bounded_semantic_subset_and_prefix_fallback() {
        // Inline records keep the envelope discriminators plus the bounded
        // semantic subset the classifier and outcome logic read.
        let echo = compact_import_record(
            br#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_call] {\"tool\":\"Bash\"}"}}"#,
        );
        let echo: serde_json::Value = serde_json::from_slice(&echo).unwrap();
        assert_eq!(echo["type"], "event_msg");
        assert_eq!(echo["payload"]["type"], "agent_message");
        assert_eq!(
            echo["payload"]["message"],
            "[external_agent_tool_call] {\"tool\":\"Bash\"}"
        );

        let item = compact_import_record(
            br#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"call_9","exitCode":1,"status":"completed","errorMessage":"boom"}}}"#,
        );
        let item: serde_json::Value = serde_json::from_slice(&item).unwrap();
        assert_eq!(item["payload"]["item"]["exitCode"], 1);
        assert_eq!(item["payload"]["item"]["status"], "completed");
        assert_eq!(item["payload"]["item"]["errorMessage"], "boom");

        let response = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"[external_agent_tool_result] done"}]}}"#,
        );
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["payload"]["role"], "assistant");
        assert_eq!(
            response["payload"]["content"][0]["text"],
            "[external_agent_tool_result] done"
        );

        // Session provenance is the only `session_meta` payload copied into
        // the display projection. It survives both complete JSON and the
        // prefix-only path used for large blob-backed records.
        let session = compact_import_record(
            br#"{"type":"session_meta","payload":{"model_provider":"sglang_dsv4","agent_nickname":"Harvey","base_instructions":"large private field"}}"#,
        );
        let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
        assert_eq!(session["payload"]["model_provider"], "sglang_dsv4");
        assert_eq!(session["payload"]["agent_nickname"], "Harvey");
        assert!(session["payload"].get("base_instructions").is_none());
        let session_op = op_envelope(
            90,
            1,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(serde_json::to_vec(&session).unwrap()),
                raw_hash: None,
            }),
        );
        let metadata = session_metadata_index(std::slice::from_ref(&session_op));
        let metadata = metadata.get("session:10").expect("session metadata");
        assert_eq!(metadata.session_title, None);
        assert_eq!(metadata.model_provider.as_deref(), Some("sglang_dsv4"));
        assert_eq!(metadata.agent_nickname.as_deref(), Some("Harvey"));

        // Claude's explicit title and the portable Codex title record retain
        // only their bounded display fields. A duplicate Claude agent-name is
        // removed from the final metadata, while a distinct named subagent is
        // kept for `title · nickname` rendering.
        let custom_title = compact_import_record(
            br#"{"type":"custom-title","customTitle":"q0","private":"discard"}"#,
        );
        let custom_title: serde_json::Value = serde_json::from_slice(&custom_title).unwrap();
        assert_eq!(custom_title["customTitle"], "q0");
        assert!(custom_title.get("private").is_none());
        let codex_title = compact_import_record(
            br#"{"type":"session_title","provider":"codex","title":"r8","updated_at":"discard"}"#,
        );
        let codex_title: serde_json::Value = serde_json::from_slice(&codex_title).unwrap();
        assert_eq!(codex_title["title"], "r8");
        assert!(codex_title.get("updated_at").is_none());

        let metadata = session_metadata_index(&[
            op_envelope(
                91,
                1,
                OpKind::Import(ImportOp {
                    raw_ref: Payload::Inline(
                        br#"{"type":"ai-title","aiTitle":"generated"}"#.to_vec(),
                    ),
                    raw_hash: None,
                }),
            ),
            op_envelope(
                91,
                2,
                OpKind::Import(ImportOp {
                    raw_ref: Payload::Inline(serde_json::to_vec(&custom_title).unwrap()),
                    raw_hash: None,
                }),
            ),
            op_envelope(
                91,
                3,
                OpKind::Import(ImportOp {
                    raw_ref: Payload::Inline(br#"{"type":"agent-name","agentName":"q0"}"#.to_vec()),
                    raw_hash: None,
                }),
            ),
        ]);
        let metadata = metadata.get("session:10").expect("Claude title metadata");
        assert_eq!(metadata.session_title.as_deref(), Some("q0"));
        assert_eq!(metadata.agent_nickname, None);

        let metadata = session_metadata_index(&[
            op_envelope(
                92,
                1,
                OpKind::Import(ImportOp {
                    raw_ref: Payload::Inline(serde_json::to_vec(&codex_title).unwrap()),
                    raw_hash: None,
                }),
            ),
            op_envelope(
                92,
                2,
                OpKind::Import(ImportOp {
                    raw_ref: Payload::Inline(
                        br#"{"type":"session_meta","payload":{"agent_nickname":"Tesla"}}"#.to_vec(),
                    ),
                    raw_hash: None,
                }),
            ),
        ]);
        let metadata = metadata.get("session:10").expect("Codex title metadata");
        assert_eq!(metadata.session_title.as_deref(), Some("r8"));
        assert_eq!(metadata.agent_nickname.as_deref(), Some("Tesla"));

        let session_prefix = compact_import_record(
            br#"{"type":"session_meta","payload":{"model_provider":"sglang_dsv4","agent_nickname":"Harvey","base_instructions":"unterminated"#,
        );
        let session_prefix: serde_json::Value = serde_json::from_slice(&session_prefix).unwrap();
        assert_eq!(session_prefix["payload"]["model_provider"], "sglang_dsv4");
        assert_eq!(session_prefix["payload"]["agent_nickname"], "Harvey");

        // Legacy token-usage imports need their exact schema shape during
        // projection, but never their accounting values.
        let usage = compact_import_record(
            br#"{"type":"token_usage_record","payload":{"thread_id":"0195cda5-433d-7f9a-9d7b-a9f15b60c2e2","turn_id":"turn-1","session_id":"0195cda5-433d-7f9a-9d7b-a9f15b60c2e2","root_turn_id":"turn-1","response_id":"response-1","usage":{"total_tokens":13},"turn_token_usage":{"total_tokens":13},"thread_token_usage":{"total_tokens":13}}}"#,
        );
        let usage: serde_json::Value = serde_json::from_slice(&usage).unwrap();
        assert_eq!(usage["type"], "token_usage_record");
        assert_eq!(usage["payload"]["turn_id"], "turn-1");
        for field in ["usage", "turn_token_usage", "thread_token_usage"] {
            assert_eq!(usage["payload"][field], serde_json::json!({}));
        }

        // Large outputs are bounded, never copied into the projection.
        let huge = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call_output","output":"{}"}}}}"#,
            "y".repeat(200_000),
        );
        let compacted = compact_import_record(huge.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        let output = compacted["payload"]["output"].as_str().unwrap();
        assert!(output.chars().count() <= DISPLAY_PREVIEW_CHAR_LIMIT.saturating_add(1));

        // Truncated blob preview: the prefix fallback still recovers the
        // external-tool marker near the envelope start.
        let truncated = format!(
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"[external_agent_tool_result] {}"}}"#,
            "z".repeat(200_000),
        );
        let compacted = compact_import_record(truncated.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["type"], "event_msg");
        assert_eq!(compacted["payload"]["type"], "agent_message");
        assert!(
            compacted["payload"]["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("[external_agent_tool_result]"),
            "marker recovered from truncated blob preview"
        );
    }

    #[test]
    fn compact_import_record_preserves_exact_claude_response_identity() {
        let complete = compact_import_record(
            br#"{"parentUuid":"parent-1","type":"assistant","message":{"id":"msg-response-1","content":[{"type":"tool_use","id":"call-1"}]}}"#,
        );
        let complete: serde_json::Value = serde_json::from_slice(&complete).unwrap();
        assert_eq!(complete["type"], "assistant");
        assert_eq!(complete["message"]["id"], "msg-response-1");

        // Blob previews can end before the large content body closes. Identity
        // is near the envelope start and remains exact in that prefix path.
        let prefix = compact_import_record(
            br#"{"parentUuid":"parent-1","type":"assistant","message":{"id":"msg-response-1","content":[{"type":"tool_use","input":"unterminated"#,
        );
        let prefix: serde_json::Value = serde_json::from_slice(&prefix).unwrap();
        assert_eq!(prefix["message"]["id"], "msg-response-1");
    }

    #[test]
    fn compact_import_record_preserves_claude_interruption_evidence() {
        let complete = compact_import_record(
            br#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},"interruptedMessageId":"msg-cancelled"}"#,
        );
        let complete: serde_json::Value = serde_json::from_slice(&complete).unwrap();
        assert_eq!(complete["type"], "user");
        assert_eq!(complete["text"], "[Request interrupted by user]");
        assert_eq!(complete["interruptedMessageId"], "msg-cancelled");

        // An unclosed blob preview can still recover the exact typed marker
        // even when the trailing interruption id has not arrived yet.
        let prefix = compact_import_record(
            br#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"},{"type":"tool_result","content":"unterminated"#,
        );
        let prefix: serde_json::Value = serde_json::from_slice(&prefix).unwrap();
        assert_eq!(prefix["text"], "[Request interrupted by user for tool use]");
    }

    #[test]
    fn compact_import_record_preserves_canonical_codex_exec_outcome_header() {
        let raw = format!(
            r#"{{"type":"response_item","payload":{{"type":"custom_tool_call_output","output":[{{"type":"input_text","text":"Script failed\nWall time 0.0 seconds\nOutput:\n"}},{{"type":"input_text","text":"{}"}}]}}}}"#,
            "x".repeat(DISPLAY_PREVIEW_READ_LIMIT.saturating_mul(2)),
        );

        let compacted = compact_import_record(raw.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(
            compacted["payload"]["output"][0]["text"],
            "Script failed\nWall time 0.0 seconds\nOutput:"
        );

        // A truncated blob preview takes the prefix parser but must retain the
        // identical status evidence near the start of the envelope.
        let compacted = compact_import_record(&raw.as_bytes()[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(
            compacted["payload"]["output"][0]["text"],
            "Script failed\nWall time 0.0 seconds\nOutput:"
        );
    }

    #[test]
    fn compact_import_record_preserves_structured_tool_payload_carriers() {
        // Object/array tool-payload carriers (arguments/input/parameters)
        // keep a bounded structural/content signal so childless tool-like
        // rows are not compacted into empty transport. Large nested strings
        // stay bounded.
        let call = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"function_call","name":"WebSearch","arguments":{"query":"editchain docs"}}}"#,
        );
        let call: serde_json::Value = serde_json::from_slice(&call).unwrap();
        assert_eq!(call["payload"]["arguments"]["query"], "editchain docs");

        let carriers = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"function_call","input":{"path":"/tmp/x"},"parameters":{"depth":2}}}"#,
        );
        let carriers: serde_json::Value = serde_json::from_slice(&carriers).unwrap();
        assert_eq!(carriers["payload"]["input"]["path"], "/tmp/x");
        assert_eq!(carriers["payload"]["parameters"]["depth"], 2);

        // Nested strings inside a structured carrier are bounded like any
        // other display field.
        let huge = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call","arguments":{{"query":"{}"}}}}}}"#,
            "y".repeat(200_000),
        );
        let compacted = compact_import_record(huge.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        let query = compacted["payload"]["arguments"]["query"].as_str().unwrap();
        assert!(query.chars().count() <= DISPLAY_PREVIEW_CHAR_LIMIT.saturating_add(1));

        // Truncated blob preview: the prefix fallback still recovers an
        // object arguments carrier near the envelope start.
        let truncated = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call","arguments":{{"query":"docs"}},"output":"{}"}}"#,
            "z".repeat(200_000),
        );
        let compacted = compact_import_record(truncated.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["type"], "response_item");
        assert_eq!(compacted["payload"]["type"], "function_call");
        assert_eq!(compacted["payload"]["arguments"]["query"], "docs");
    }

    /// Total retained object keys plus array items in a compacted carrier.
    fn count_entries(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(map) => map
                .iter()
                .map(|(_, child)| count_entries(child).saturating_add(1))
                .sum(),
            serde_json::Value::Array(items) => items
                .iter()
                .map(|child| count_entries(child).saturating_add(1))
                .sum(),
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => 0,
        }
    }

    /// Maximum container nesting depth of a value (containers count 1, leaves
    /// count 0).
    fn depth_of(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(map) => map
                .values()
                .map(depth_of)
                .max()
                .map_or(1, |depth| depth.saturating_add(1)),
            serde_json::Value::Array(items) => items
                .iter()
                .map(depth_of)
                .max()
                .map_or(1, |depth| depth.saturating_add(1)),
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => 0,
        }
    }

    #[test]
    fn compact_structured_globally_bounds_retained_entries() {
        // Two wide sibling objects share one total budget: the retained
        // output never exceeds the total entry limit across the whole carrier
        // (the old per-depth cap would retain 64 entries at every level).
        let value = serde_json::json!({
            "first": (0..128u32)
                .map(|i: u32| (format!("a{i}"), serde_json::Value::from(i)))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
            "second": (0..128u32)
                .map(|i: u32| (format!("b{i}"), serde_json::Value::from(i)))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        });
        let compacted = compact_structured(&value);
        assert_eq!(
            count_entries(&compacted),
            STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT,
            "the shared budget must cap the whole carrier"
        );
        let compacted_object = compacted.as_object().unwrap();
        assert!(
            compacted_object.contains_key("first"),
            "the leading sibling keeps the budget"
        );
        assert!(
            !compacted_object.contains_key("second"),
            "the trailing sibling is dropped once the shared budget is spent"
        );
    }

    #[test]
    fn compact_structured_bounds_oversized_multibyte_keys() {
        // Up to STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT object keys are retained
        // verbatim by key.clone(); oversized multibyte keys would otherwise
        // keep unbounded raw payload despite the boundedness claim. Keys must
        // be cut to the display char limit while staying non-empty.
        let huge_key = "界".repeat(DISPLAY_PREVIEW_CHAR_LIMIT.saturating_mul(8));
        let mut map = serde_json::Map::new();
        for i in 0..STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT {
            // Distinct leading discriminators keep every truncated bounded key
            // unique, so the budget slots are retained rather than collapsed
            // into one colliding entry.
            drop(map.insert(format!("{i}{huge_key}"), serde_json::Value::from(i)));
        }
        let compacted = compact_structured(&serde_json::Value::Object(map));
        let serialized = serde_json::to_string(&compacted).unwrap();
        assert!(
            !serialized.is_empty(),
            "the compacted carrier must keep a non-empty signal"
        );
        assert_eq!(
            compacted.as_object().unwrap().len(),
            STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT,
            "every budget slot stays retained with a bounded key"
        );
        // Fixed ceiling: each retained key holds at most
        // DISPLAY_PREVIEW_CHAR_LIMIT chars, worst-case 6 JSON-escaped bytes
        // per char, plus quotes; values and punctuation add a tiny fixed
        // amount. Unbounded keys would blow far past this ceiling.
        let ceiling = STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT
            .saturating_mul(
                DISPLAY_PREVIEW_CHAR_LIMIT
                    .saturating_mul(6)
                    .saturating_add(8),
            )
            .saturating_add(256);
        assert!(
            serialized.len() < ceiling,
            "serialized compact output ({} bytes) must stay below the fixed \
             ceiling ({ceiling} bytes)",
            serialized.len()
        );
    }

    #[test]
    fn compact_structured_keeps_first_key_when_truncation_collides() {
        // Two distinct oversized keys sharing one 1024-char prefix truncate to
        // the same bounded key text; the retained object must deterministically
        // keep the first original key instead of silently overwriting it.
        let shared_prefix = "界".repeat(DISPLAY_PREVIEW_CHAR_LIMIT.saturating_mul(8));
        let value = serde_json::json!({
            // Lexicographically first original key: keeps value 1 on collision
            // with first-wins handling.
            format!("{shared_prefix}a"): 1,
            format!("{shared_prefix}b"): 2,
        });
        let compacted = compact_structured(&value);
        let compacted_object = compacted.as_object().unwrap();
        assert_eq!(
            compacted_object.len(),
            1,
            "truncation collision must not produce two identical retained keys"
        );
        let bounded_key = compact_text(&format!("{shared_prefix}a"));
        assert_eq!(
            compacted_object.get(&bounded_key),
            Some(&serde_json::Value::from(1)),
            "the lexicographically first original key wins deterministically"
        );
    }

    #[test]
    fn compact_structured_prunes_nesting_beyond_max_depth() {
        // Pathological nesting is pruned at STRUCTURED_CARRIER_MAX_DEPTH
        // rather than recursing without bound, through the direct compactor
        // and the full compact_import_record path.
        let mut deep = serde_json::Value::Bool(true);
        for _ in 0..(STRUCTURED_CARRIER_MAX_DEPTH.saturating_mul(2)) {
            deep = serde_json::Value::Array(vec![deep]);
        }
        assert!(
            depth_of(&deep) > STRUCTURED_CARRIER_MAX_DEPTH,
            "input must exceed the depth cap"
        );
        assert!(
            depth_of(&compact_structured(&deep)) <= STRUCTURED_CARRIER_MAX_DEPTH.saturating_add(1)
        );

        let mut inner = String::from("1");
        for _ in 0..(STRUCTURED_CARRIER_MAX_DEPTH.saturating_mul(2)) {
            inner = format!("[{inner}]");
        }
        let raw = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call","arguments":{inner}}}}}"#
        );
        let compacted = compact_import_record(raw.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert!(
            depth_of(&compacted["payload"]["arguments"])
                <= STRUCTURED_CARRIER_MAX_DEPTH.saturating_add(1),
            "deep arguments carrier must be pruned through the import path"
        );
    }

    #[test]
    fn compact_import_record_keeps_sentinel_for_unclosed_preview_carrier() {
        // A large blob-backed function call whose arguments object starts
        // inside the bounded preview read window but closes after the cutoff
        // must keep a tiny non-empty signal: it is a genuine tool payload, not
        // empty transport.
        let raw = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call","name":"Bash","arguments":{{"command":"{}","cwd":"/tmp"}}}}}}"#,
            "x".repeat(200_000),
        );
        let bytes = raw.as_bytes();
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["type"], "response_item");
        assert_eq!(compacted["payload"]["type"], "function_call");
        assert_eq!(
            compacted["payload"]["arguments"]["truncated"], true,
            "started-but-incomplete carrier keeps the sentinel"
        );
    }

    #[test]
    fn compact_import_record_keeps_complete_empty_carriers_silent_in_previews() {
        // A complete empty object/array carrier closes inside the preview and
        // must stay silent even when the surrounding record is truncated.
        let cases = [
            r#"{"type":"response_item","payload":{"type":"function_call","arguments":{}},"output":""#,
            r#"{"type":"response_item","payload":{"type":"function_call","parameters":[]},"output":""#,
        ];
        for prefix in cases {
            let full = format!("{prefix}{}\"}}}}", "z".repeat(200_000));
            let bytes = full.as_bytes();
            assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
            let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
            let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
            assert!(
                compacted["payload"].get("arguments").is_none(),
                "complete empty arguments carrier must stay silent: {prefix}"
            );
            assert!(
                compacted["payload"].get("parameters").is_none(),
                "complete empty parameters carrier must stay silent: {prefix}"
            );
        }
    }

    #[test]
    fn compact_import_record_preserves_scalar_tool_payload_carriers() {
        // Non-empty string and scalar bool/number carriers (arguments/input/
        // parameters) keep a bounded signal; null/empty strings stay silent.
        let call = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"function_call","name":"Bash","arguments":"ls -la"}}"#,
        );
        let call: serde_json::Value = serde_json::from_slice(&call).unwrap();
        assert_eq!(call["payload"]["arguments"], "ls -la");

        let carriers = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"function_call","name":"Read","input":"/tmp/x","parameters":true}}"#,
        );
        let carriers: serde_json::Value = serde_json::from_slice(&carriers).unwrap();
        assert_eq!(carriers["payload"]["input"], "/tmp/x");
        assert_eq!(carriers["payload"]["parameters"], true);

        let number = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"function_call","name":"Tool","parameters":7}}"#,
        );
        let number: serde_json::Value = serde_json::from_slice(&number).unwrap();
        assert_eq!(number["payload"]["parameters"], 7);

        let empty = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"function_call","arguments":"","parameters":null}}"#,
        );
        let empty: serde_json::Value = serde_json::from_slice(&empty).unwrap();
        assert!(empty["payload"].get("arguments").is_none());
        assert!(empty["payload"].get("parameters").is_none());
    }

    #[test]
    fn compact_import_record_prefix_fallback_recovers_scalar_carriers() {
        // Truncated blob preview: string/bool input and parameters carriers
        // near the envelope start survive the prefix fallback.
        let raw = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call","name":"Read","input":"/tmp/x","parameters":true,"output":"{}"}}}}"#,
            "z".repeat(200_000),
        );
        let bytes = raw.as_bytes();
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["payload"]["input"], "/tmp/x");
        assert_eq!(compacted["payload"]["parameters"], true);
    }

    #[test]
    fn compact_import_record_preserves_bounded_reasoning_summary() {
        // Reasoning response items keep the first non-empty summary text so
        // the compact service record can still yield a legible row label.
        let item = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Audit the tree layout"},{"type":"summary_text","text":"ignored"}],"content":[{"type":"reasoning","text":"ignored"}]}}"#,
        );
        let item: serde_json::Value = serde_json::from_slice(&item).unwrap();
        assert_eq!(item["payload"]["summary"][0]["type"], "summary_text");
        assert_eq!(
            item["payload"]["summary"][0]["text"],
            "Audit the tree layout"
        );

        // Empty/whitespace-only first summary blocks fall through to a later
        // text-bearing block.
        let skipped = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"  "},{"type":"summary_text","text":"second block"}]}}"#,
        );
        let skipped: serde_json::Value = serde_json::from_slice(&skipped).unwrap();
        assert_eq!(skipped["payload"]["summary"][0]["text"], "second block");

        // An empty summary array keeps no summary field at all.
        let empty = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
        );
        let empty: serde_json::Value = serde_json::from_slice(&empty).unwrap();
        assert!(empty["payload"].get("summary").is_none());

        // Large multibyte summary text stays bounded to the display limit.
        let huge = format!(
            r#"{{"type":"response_item","payload":{{"type":"reasoning","summary":[{{"type":"summary_text","text":"{}"}}]}}}}"#,
            "界".repeat(200_000),
        );
        let compacted = compact_import_record(huge.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        let text = compacted["payload"]["summary"][0]["text"].as_str().unwrap();
        assert!(text.chars().count() <= DISPLAY_PREVIEW_CHAR_LIMIT.saturating_add(1));
        assert!(text.ends_with('…'), "cut summary keeps the ellipsis marker");
    }

    #[test]
    fn compact_import_record_prefix_fallback_recovers_reasoning_summary() {
        // Truncated blob preview: the first summary text sits near the
        // envelope start and survives the prefix fallback even when a huge
        // trailing output never closes inside the read limit.
        let raw = format!(
            r#"{{"type":"response_item","payload":{{"type":"reasoning","summary":[{{"type":"summary_text","text":"Recovered from prefix"}}],"output":"{}"}}"#,
            "z".repeat(200_000),
        );
        let bytes = raw.as_bytes();
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["type"], "response_item");
        assert_eq!(compacted["payload"]["type"], "reasoning");
        assert_eq!(
            compacted["payload"]["summary"][0]["text"],
            "Recovered from prefix"
        );
    }

    #[test]
    fn json_string_field_preview_skips_escaped_quotes() {
        let raw = r#"{"message":"say \"hello\" now","tail":true}"#;
        let (value, cut_by_read_limit) =
            json_string_field_preview(raw, "message", 0).expect("message preview");
        assert_eq!(value, r#"say \"hello\" now"#);
        assert!(!cut_by_read_limit);
    }

    #[test]
    fn json_string_field_preview_accepts_even_backslash_run_before_close() {
        let mut raw = String::from(r#"{"message":"path"#);
        raw.push('\\');
        raw.push('\\');
        raw.push('"');
        raw.push('}');

        let (value, cut_by_read_limit) =
            json_string_field_preview(&raw, "message", 0).expect("message preview");
        let mut expected = String::from("path");
        expected.push('\\');
        expected.push('\\');
        assert_eq!(value, expected);
        assert!(!cut_by_read_limit);
    }

    #[test]
    fn compact_import_record_prefix_fallback_decodes_string_escapes() {
        // The message closes inside the preview, but a later output field is
        // cut so the record takes the prefix-recovery path. Its display text
        // must match serde's fully parsed path, not expose raw `\n` / `\"`.
        let raw = br#"{"type":"event_msg","payload":{"type":"agent_message","message":"line one\n\"quoted\"","output":"unfinished"#;
        let compacted = compact_import_record(raw);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["payload"]["message"], "line one\n\"quoted\"");
        assert!(compacted["payload"].get("echo_text_truncated").is_none());
    }

    #[test]
    fn compact_import_record_marks_preview_ending_at_escaped_quote_truncated() {
        let mut raw = String::from(
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"prefix "#,
        );
        raw.push('\\');
        raw.push('"');

        let (_, cut_by_read_limit) =
            json_string_field_preview(&raw, "message", 0).expect("message preview");
        assert!(cut_by_read_limit, "an escaped quote is not a closing quote");

        let compacted = compact_import_record(raw.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["payload"]["message"], "prefix \"");
        assert_eq!(compacted["payload"]["echo_text_truncated"], true);
    }

    #[test]
    fn compact_import_record_marks_truncated_echo_text() {
        // The classifier must be told explicitly when a service-compacted echo
        // message text was truncated, instead of guessing from an ellipsis:
        // two distinct long texts sharing a display-preview prefix would
        // otherwise compare equal after compaction and be conflated by exact
        // duplicate pairing.

        // Inline event_msg agent_message over the display budget: the message
        // keeps the bounded prefix with an ellipsis and the flag is set.
        let prefix = "shared-prefix-".repeat(200);
        let long_event = format!(
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{prefix}TAIL-A"}}}}"#,
        );
        let compacted = compact_import_record(long_event.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["payload"]["echo_text_truncated"], true);
        let message = compacted["payload"]["message"].as_str().unwrap();
        assert!(message.starts_with("shared-prefix-"));
        assert!(
            message.ends_with('…'),
            "cut message keeps the ellipsis marker"
        );

        // A short untruncated message never sets the flag.
        let short_event = compact_import_record(
            br#"{"type":"event_msg","payload":{"type":"agent_message","message":"exact narrative"}}"#,
        );
        let short_event: serde_json::Value = serde_json::from_slice(&short_event).unwrap();
        assert!(short_event["payload"].get("echo_text_truncated").is_none());
        assert_eq!(short_event["payload"]["message"], "exact narrative");

        // Inline response_item assistant message: a long first content text
        // sets the flag; a short one does not.
        let long_response = format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{prefix}TAIL-B"}}]}}}}"#,
        );
        let compacted = compact_import_record(long_response.as_bytes());
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["payload"]["echo_text_truncated"], true);
        let text = compacted["payload"]["content"][0]["text"].as_str().unwrap();
        assert!(text.ends_with('…'));

        let short_response = compact_import_record(
            br#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"exact narrative"}]}}"#,
        );
        let short_response: serde_json::Value = serde_json::from_slice(&short_response).unwrap();
        assert!(short_response["payload"]
            .get("echo_text_truncated")
            .is_none());

        // Blob-backed record over the read budget: the preview window ends
        // inside the huge message value, so the prefix fallback recovers only
        // the bounded prefix (with an ellipsis) and sets the flag.
        let blob_raw = format!(
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{}"}}}}"#,
            "k".repeat(200_000),
        );
        let bytes = blob_raw.as_bytes();
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(
            compacted["payload"]["echo_text_truncated"], true,
            "read-limit cut blob preview sets the flag"
        );
        assert!(compacted["payload"]["message"]
            .as_str()
            .unwrap()
            .ends_with('…'));

        // Read-limit cut WITHOUT an ellipsis: a record whose `message` value
        // starts late enough in the preview window that the 4096-byte boundary
        // lands inside the value after fewer than the character-budget chars
        // (here ~622), so `compact_text` appends no ellipsis. Only the flag
        // tells the classifier the text is known-truncated — the ellipsis
        // heuristic alone would miss it.
        let padded_event = format!(
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","pad":"{}","message":"{}"}}}}"#,
            "p".repeat(3400),
            "k".repeat(5000),
        );
        let bytes = padded_event.as_bytes();
        let message_needle = r#""message":""#;
        let message_value_start = bytes
            .windows(message_needle.len())
            .position(|window| window == message_needle.as_bytes())
            .expect("message field")
            .saturating_add(message_needle.len());
        assert_eq!(
            message_value_start, 3474,
            "layout drives the no-ellipsis cut"
        );
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        assert!(message_value_start < DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        let message = compacted["payload"]["message"].as_str().unwrap();
        assert_eq!(message.chars().count(), 622);
        assert!(
            !message.ends_with('…'),
            "short remainder inside the window gets no appended ellipsis"
        );
        assert_eq!(
            compacted["payload"]["echo_text_truncated"], true,
            "read-limit cut without ellipsis still sets the flag"
        );

        // Same no-ellipsis read-limit cut for a response_item's first content
        // text value (window ends ~675 chars into the value, no ellipsis).
        let padded_response = format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","pad":"{}","content":[{{"type":"output_text","text":"{}"}}]}}}}"#,
            "p".repeat(3300),
            "k".repeat(6000),
        );
        let bytes = padded_response.as_bytes();
        let text_needle = r#""text":""#;
        let text_value_start = bytes
            .windows(text_needle.len())
            .position(|window| window == text_needle.as_bytes())
            .expect("content text field")
            .saturating_add(text_needle.len());
        assert_eq!(text_value_start, 3421, "layout drives the no-ellipsis cut");
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        assert!(text_value_start < DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        let text = compacted["payload"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), 675);
        assert!(!text.ends_with('…'));
        assert_eq!(compacted["payload"]["echo_text_truncated"], true);
    }

    #[test]
    fn compact_import_record_prefix_fallback_recovers_payload_exit_code() {
        // A truncated blob preview may cut the record before its
        // `payload.item` block entirely; payload-level exitCode/status/
        // errorMessage outcome evidence near the envelope start must still
        // survive, exactly as the full-parse path preserves it.
        let failure = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call_output","exitCode":1,"errorMessage":"boom","status":"failed","output":"{}"}}}}"#,
            "z".repeat(200_000),
        );
        let bytes = failure.as_bytes();
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["payload"]["type"], "function_call_output");
        assert_eq!(compacted["payload"]["exitCode"], 1);
        assert_eq!(compacted["payload"]["errorMessage"], "boom");
        assert_eq!(compacted["payload"]["status"], "failed");

        // Success evidence: exitCode 0 is recovered the same way.
        let success = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call_output","exitCode":0,"output":"{}"}}}}"#,
            "y".repeat(200_000),
        );
        let bytes = success.as_bytes();
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["payload"]["exitCode"], 0);

        // The nested item-level copy is still recovered (unchanged behavior),
        // including when the payload-level field precedes it.
        let nested = format!(
            r#"{{"type":"event_msg","payload":{{"type":"item_completed","exitCode":2,"status":"completed","item":{{"type":"CommandExecution","id":"call_x","exitCode":2,"status":"completed"}},"output":"{}"}}}}"#,
            "w".repeat(200_000),
        );
        let bytes = nested.as_bytes();
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert_eq!(compacted["payload"]["exitCode"], 2);
        assert_eq!(compacted["payload"]["item"]["exitCode"], 2);
        assert_eq!(compacted["payload"]["item"]["status"], "completed");
    }

    #[test]
    fn row_first_window_precedes_global_layout() {
        let first = message_op(1, 1, OpId::new(NodeId(0), 0, 0));
        let second = message_op(1, 2, first.id);
        let projection = HistoryProjection::from_ops(vec![first, second]);
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::new(
            String::new(),
            String::new(),
            String::new(),
            false,
            false,
            false,
        );

        let provisional = ws.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 10,
            hide_submodules: false,
            filter: &filter,
            include_layout: false,
        });
        assert!(!provisional.layout_ready);
        assert!(!provisional.rows.is_empty());
        assert!(provisional.rows.iter().all(|row| {
            row.lane == 0
                && row.above.is_empty()
                && row.below.is_empty()
                && row.transitions.is_empty()
        }));
        assert!(ws
            .current_view
            .as_ref()
            .is_some_and(|(_, snapshot)| snapshot.context.is_none()));

        let laid_out = ws.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 10,
            hide_submodules: false,
            filter: &filter,
            include_layout: true,
        });
        assert!(laid_out.layout_ready);
        assert_eq!(laid_out.rows.len(), provisional.rows.len());
        assert_eq!(laid_out.rows[0].node_key, provisional.rows[0].node_key);
        assert!(ws
            .current_view
            .as_ref()
            .is_some_and(|(_, snapshot)| snapshot.context.is_some()));
    }

    #[test]
    fn view_cache_stays_bounded_across_filter_changes() {
        let ops = vec![
            message_op(1, 1, OpId::new(NodeId(0), 0, 0)),
            message_op(1, 2, OpId::new(NodeId(0), 0, 0)),
        ];
        let projection = HistoryProjection::from_ops(ops);
        let mut ws = Workspace::from_projection(projection);
        let filters: Vec<ChainFilter> = (0..8)
            .map(|i| {
                ChainFilter::new(
                    format!("pattern-{i}"),
                    String::new(),
                    String::new(),
                    false,
                    false,
                    false,
                )
            })
            .collect();
        for filter in &filters {
            drop(ws.history_window(HistoryWindowOptions {
                offset: 0,
                limit: 10,
                hide_submodules: false,
                filter,
                include_layout: true,
            }));
            drop(ws.graph_layout(false, 0, 10, filter));
        }
        // The cache keeps a single active slot: after eight distinct filters
        // only the last snapshot is retained, never an accumulated O(V) set.
        let cached_key = ws.current_view.as_ref().map(|(key, _)| key.clone());
        assert_eq!(cached_key, Some((false, filters.last().unwrap().key())));
        // Same-key reuse between GetWindow and GetLayout keeps the snapshot.
        drop(ws.history_window(HistoryWindowOptions {
            offset: 0,
            limit: 10,
            hide_submodules: false,
            filter: filters.last().unwrap(),
            include_layout: true,
        }));
        assert_eq!(
            ws.current_view.as_ref().map(|(key, _)| key.clone()),
            Some((false, filters.last().unwrap().key()))
        );
    }

    #[test]
    fn default_and_fixed_filters_hide_undated_and_trace_rows() {
        // The fixed pregenerated viewer filter hides trace rows so Activity
        // mode is served from the render snapshot. A `None` DTO maps to that
        // fixed filter explicitly; `ChainFilter::default()` stays the raw
        // baseline (hide_trace off), and raw mode sends an explicit
        // `hide_trace: false` to materialize the live projection.
        let default_filter = chain_filter_from_dto(None);
        assert!(default_filter.key().hide_trace);
        assert_eq!(
            default_filter.key(),
            fixed_view_filter().key(),
            "None DTO must map to the fixed viewer filter"
        );
        assert!(fixed_view_filter().key().hide_trace);
        assert!(
            !ChainFilter::default().key().hide_trace,
            "ChainFilter::default is the raw baseline"
        );
        assert!(
            ChainFilter::default().key().hide_undated,
            "existing ChainFilter::default behavior preserved"
        );
        assert!(
            default_filter.key().hide_undated,
            "the fixed Activity view must hide timestamp-zero rows"
        );

        let raw = chain_filter_from_dto(Some(&ChainFilterDto {
            summary_pattern: String::new(),
            kind_pattern: String::new(),
            include_kind_pattern: String::new(),
            hide_undated: false,
            hide_trace: false,
            splice: true,
        }));
        assert!(!raw.key().hide_trace);
        assert_ne!(
            raw.key(),
            fixed_view_filter().key(),
            "raw mode must not be served from the fixed-view snapshot"
        );
    }

    /// Build a scored chunk for a search hit.
    fn scored_chunk(op_id: OpId, score: f64, source: Source, text: &str) -> ScoredChunk {
        let chunk_id = editchain_query::search::ChunkId {
            op_id,
            chunk_ordinal: 0,
        };
        ScoredChunk {
            chunk_id,
            op_id,
            score,
            text: text.to_string(),
            metadata: editchain_query::search::ChunkMetadata {
                op_id,
                chunk_id,
                source,
                session_id: Some(SessionId(10)),
                actor_id: ActorId(1),
                kind_tags: 0,
                timestamp_ms: 1_000,
                generation: 0,
            },
        }
    }

    #[test]
    fn find_in_history_resolves_top_level_and_folded_children_and_dedupes() {
        // One visible turn row (raw import) with a normalized message child
        // folded into it and a META sub-op bundled under it. Chunks matching
        // the row's own op, the folded child, or the sub-op must all resolve to
        // the single visible top-level row, keeping the best BM25 score.
        let turn = import_op(1, 1, false);
        let msg = message_op(1, 2, turn.id);
        let meta = Op {
            parents: ParentSet::One(turn.id),
            ..import_op(1, 3, true)
        };
        let opts = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection =
            HistoryProjection::from_ops_with(vec![turn.clone(), msg.clone(), meta.clone()], opts);
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::default();

        let chunks = vec![
            scored_chunk(turn.id, 1.0, Source::EditChain, "needle in turn"),
            scored_chunk(msg.id, 3.5, Source::EditChain, "needle in folded message"),
            scored_chunk(meta.id, 0.5, Source::EditChain, "needle in meta sub-op"),
        ];
        let matches = ws.find_in_history(&chunks, &BTreeMap::new(), false, &filter);

        assert_eq!(matches.len(), 1, "all three chunks dedupe into one row");
        assert_eq!(matches[0].node_key, turn.id.to_string());
        assert_eq!(matches[0].row, 0);
        assert_eq!(matches[0].op_id, msg.id.to_string());
        assert!((matches[0].score - 3.5).abs() < f64::EPSILON);
        assert!(matches[0].git_oid.is_none());
        assert!(matches[0].repository.is_none());
        // The O(V) op→row map is built once and cached on the view snapshot.
        assert!(ws
            .current_view
            .as_ref()
            .is_some_and(|(_, snapshot)| snapshot.op_rows.is_some()));
        let again = ws.find_in_history(&chunks, &BTreeMap::new(), false, &filter);
        assert_eq!(again.len(), 1);
        assert!((again[0].score - 3.5).abs() < f64::EPSILON);
    }

    #[test]
    fn find_in_history_maps_bundle_members_and_subops_to_containing_parent() {
        // An Activity-view execute-run bundle folds two member rows into one
        // top-level row; a hit inside any member (or a member's own bundled
        // metadata sub-op) must resolve to the containing bundle row and its
        // absolute parent-row offset — never auto-expand anything.
        let anchor = op_envelope(
            1,
            1,
            OpKind::Tool(editchain_core::op::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Inline(b"Bash".to_vec()),
                stage: editchain_core::op::ToolStage::Start,
                content: Payload::Empty,
            }),
        );
        let member_a = op_envelope(
            1,
            2,
            OpKind::Tool(editchain_core::op::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Inline(b"Bash".to_vec()),
                stage: editchain_core::op::ToolStage::Start,
                content: Payload::Empty,
            }),
        );
        let member_b = op_envelope(
            1,
            3,
            OpKind::Tool(editchain_core::op::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Inline(b"Bash".to_vec()),
                stage: editchain_core::op::ToolStage::Start,
                content: Payload::Empty,
            }),
        );
        let meta_of_b = import_op(1, 4, true);
        let bundle = editchain_project::HistoryNode::ExecuteBundle {
            anchor: std::sync::Arc::new(anchor.clone()),
            source_time: editchain_project::EffectiveTime::Observed(1_000),
            parent_override: None,
            member_nodes: Vec::new(),
            members: vec![
                std::sync::Arc::new(member_a.clone()),
                std::sync::Arc::new(member_b.clone()),
                std::sync::Arc::new(meta_of_b.clone()),
            ],
            summary: "2 tool steps".to_string(),
            kind: "tool".to_string(),
            author: "agent".to_string(),
            meta: editchain_project::meta::NodeMeta::default(),
        };
        let filter = ChainFilter::default();
        let projection = HistoryProjection::from_ops(vec![anchor.clone()]);
        let mut ws = Workspace::from_projection(projection);
        let expansion = node_expansion(&bundle, &HashMap::new(), &HashMap::new());
        // Hand-build the cached snapshot so the mapping test exercises the exact
        // Activity-view shape (bundle row + two expanded member slots).
        ws.current_view = Some((
            (false, filter.key()),
            ViewSnapshot {
                nodes: vec![bundle],
                annotations: Vec::new(),
                context: None,
                sub_op_counts: vec![3],
                expansions: vec![expansion],
                expansion_spans: vec![ExpansionSpanDto {
                    row: 0,
                    descendant_count: 3,
                }],
                starts: vec![0, 4],
                expanded_total: 4,
                max_lane: 0,
                op_rows: None,
                git_rows: None,
            },
        ));

        let chunks = vec![
            scored_chunk(member_a.id, 2.0, Source::EditChain, "needle member a"),
            scored_chunk(member_b.id, 1.0, Source::EditChain, "needle member b"),
            scored_chunk(meta_of_b.id, 0.5, Source::EditChain, "needle meta of b"),
        ];
        let matches = ws.find_in_history(&chunks, &BTreeMap::new(), false, &filter);

        assert_eq!(matches.len(), 1, "members dedupe into the bundle row");
        assert_eq!(matches[0].node_key, anchor.id.to_string());
        assert_eq!(matches[0].row, 0, "bundle parent row offset");
        assert_eq!(matches[0].op_id, member_a.id.to_string());
        assert!((matches[0].score - 2.0).abs() < f64::EPSILON);
        assert_eq!(matches[0].summary, "2 tool steps");
    }

    #[test]
    fn find_in_history_excludes_hits_with_no_row_in_the_active_view() {
        // turn1 is dated and visible; turn2 lives in its own undated session,
        // so `hide_undated` removes it from the view (sessions with no dated
        // rows keep `Unknown` time — no BundleAnchor display time is assigned).
        // A hit inside turn2's folded child must be dropped even though the op
        // exists in the projection.
        let turn1 = import_op(1, 1, false);
        let msg1 = message_op(1, 2, turn1.id);
        let mut turn2 = import_op(1, 3, false);
        turn2.scope = ScopeRef::Session(SessionId(20));
        turn2 = {
            let mut op = turn2;
            op.clock = Clock::None;
            op
        };
        let mut msg2 = message_op(1, 4, turn2.id);
        if let OpKind::Message(m) = &mut msg2.kind {
            m.content = Payload::Inline(b"needle-hidden".to_vec());
        }
        msg2.scope = ScopeRef::Session(SessionId(20));
        let opts = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let projection = HistoryProjection::from_ops_with(
            vec![turn1.clone(), msg1.clone(), turn2.clone(), msg2.clone()],
            opts,
        );
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::new(
            String::new(),
            String::new(),
            String::new(),
            true,
            false,
            false,
        );

        let chunks = vec![
            scored_chunk(msg1.id, 1.0, Source::EditChain, "needle visible"),
            scored_chunk(msg2.id, 2.0, Source::EditChain, "needle-hidden"),
        ];
        let matches = ws.find_in_history(&chunks, &BTreeMap::new(), false, &filter);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].node_key, turn1.id.to_string());
        assert_eq!(matches[0].row, 0);
        assert!(matches[0].text.contains("needle visible"));
    }

    #[test]
    fn find_in_history_git_hits_resolve_by_real_identity_and_respect_submodules() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xbb;
        let oid = GitOid::new(editchain_core::GitObjectFormat::Sha1, bytes);
        let commit = editchain_core::GitCommitEntity {
            repository: RepositoryId(2),
            object_format: editchain_core::GitObjectFormat::Sha1,
            oid,
            imported_record: None,
            availability: editchain_core::GitAvailability::Resolved,
            tree: oid,
            parents: Vec::new(),
            author: editchain_core::GitSignature {
                name: Payload::Inline(b"Alice".to_vec()),
                email: Payload::Inline(b"alice@example.com".to_vec()),
                when: 0,
            },
            committer: editchain_core::GitSignature {
                name: Payload::Inline(b"Alice".to_vec()),
                email: Payload::Inline(b"alice@example.com".to_vec()),
                when: 0,
            },
            authored_at: 0,
            committed_at: 0,
            message: Payload::Inline(b"needle-git".to_vec()),
            imported_refs: Vec::new(),
            live_refs: Vec::new(),
            changed_paths: Vec::new(),
        };
        let mut projection = HistoryProjection::new();
        projection.merge_git_commits(vec![commit.clone()]);
        let mut ws = Workspace::from_projection(projection);
        // Mark the commit's repository as a nested/submodule repo: the main
        // workspace repo at /ws/.git (id 1) contains /ws/nested/.git (id 2).
        ws.repositories = vec![
            editchain_git::RepositoryDiscovery {
                id: RepositoryId(1),
                path: PathBuf::from("/ws/.git"),
                is_worktree: false,
            },
            editchain_git::RepositoryDiscovery {
                id: RepositoryId(2),
                path: PathBuf::from("/ws/nested/.git"),
                is_worktree: false,
            },
        ];
        let synthetic = OpId::new(NodeId(0), 0, 0);
        let mut identities = BTreeMap::new();
        let _: Option<GitHitIdentity> = identities.insert(
            synthetic,
            GitHitIdentity {
                repository_id: commit.repository,
                oid: commit.oid,
                is_submodule: true,
            },
        );
        let chunks = vec![scored_chunk(synthetic, 1.0, Source::Git, "needle-git")];

        // With submodules visible, the commit row resolves by real identity.
        // (The raw baseline hides nothing undated here; the commit is dated.)
        let raw = ChainFilter::new(
            String::new(),
            String::new(),
            String::new(),
            false,
            false,
            false,
        );
        let visible = ws.find_in_history(&chunks, &identities, false, &raw);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].node_key, oid.to_hex());
        assert_eq!(visible[0].row, 0);
        assert_eq!(visible[0].git_oid.as_deref(), Some(oid.to_hex().as_str()));
        assert_eq!(visible[0].repository.as_deref(), Some("2"));
        assert_eq!(visible[0].kind, "git");
        assert!(visible[0].is_submodule);

        // With `hide_submodules` (as the fixed viewer sends), the same hit has
        // no row in the active view and is dropped — never resolved to a
        // synthetic op id or phantom offset.
        let hidden = ws.find_in_history(&chunks, &identities, true, &raw);
        assert!(hidden.is_empty());
    }
}
