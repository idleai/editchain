//! Native Rust stdio service for the `EditChain` VS Code extension.
//!
//! Reads framed requests from stdin, dispatches them against the chain reader,
//! git resolver, and unified search, and writes framed responses to stdout.

#[cfg(test)]
use tempfile as _;

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_import as _;
use editchain_query as _;
use serde as _;

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::PathBuf;

use editchain_codec::frame::decode_op;
use editchain_core::{
    ActorId, BlobRef, Clock, ContentId, GitOid, NodeId, Op, OpId, OpKind, OpSet, ParentSet,
    Payload, RepositoryId, ScopeRef, SessionId, Tags,
};
use editchain_git::{discover_repositories, resolve_commit, walk_history, RepositoryHandle};
use editchain_import::{hash_raw, FsBlobSink};
use editchain_index::LexicalIndex;
use editchain_node::segment::SegmentStore;
use editchain_project::filter::ChainFilter;
use editchain_project::HistoryProjection;
use editchain_protocol::{
    ChainFilterDto, GraphLayout as ProtocolGraphLayout, HistoryRow, HistoryWindow, LayoutEdge,
    LayoutPoint, LayoutRow, NodeDetails, ParentRelationDto, ParentRelationKind, RepositoryInfo,
    Request, RequestBody, ResolvedObject, Response, ResponseBody, SearchFiltersDto, SearchHit,
    SearchResponse,
};
use editchain_query::search::{SearchFilters, Source};

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
    /// Read-only durable blob store used by on-demand details/search hydration.
    blob_resolver: Option<BlobResolver>,
    /// Discovered git repositories.
    pub repositories: Vec<editchain_git::RepositoryDiscovery>,
    /// Diagnostics for this open: chain canonicalization and bounded blob
    /// preview/deferred-hydration outcomes.
    pub diagnostics: OpenDiagnostics,
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
    /// Graph geometry over `nodes`, built only after the first row window has
    /// painted. `None` is a valid provisional row-only snapshot.
    context: Option<editchain_project::layout::LayoutContext>,
    /// Number of expandable children attached to each top-level row.
    sub_op_counts: Vec<usize>,
    /// Expanded absolute slot where each top-level row starts, plus a sentinel.
    starts: Vec<usize>,
    /// Total number of fully expanded slots.
    expanded_total: usize,
    /// Maximum graph lane in this view.
    max_lane: usize,
}

/// Canonicalization outcome for the records decoded from a chain's segments.
///
/// Records are admitted through [`OpSet`], which ignores exact replays of an
/// accepted op and quarantines same-id records with conflicting bytes, so a
/// crash-replayed import page never double-counts or silently mutates an op.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
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

/// Blob access outcome for payloads decoded at open or explicitly hydrated.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
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
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
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
    pub fn open(chain_dir: &std::path::Path) -> io::Result<Self> {
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
        let Ok(metadata) = std::fs::metadata(&path) else {
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

/// Convert raw JSONL to the minimal fields used by row/sub-op labeling.
fn compact_import_record(bytes: &[u8]) -> Vec<u8> {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        let record_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !record_type.is_empty() {
            let compact = match record_type {
                "event_msg" => serde_json::json!({
                    "type": record_type,
                    "payload": {
                        "type": value
                            .get("payload")
                            .and_then(|payload| payload.get("type"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                    }
                }),
                "attachment" => serde_json::json!({
                    "type": record_type,
                    "attachment": value.get("attachment").cloned().unwrap_or_default()
                }),
                "user" => serde_json::json!({
                    "type": record_type,
                    "text": first_nested_json_text(&value).unwrap_or_default()
                }),
                _ => serde_json::json!({ "type": record_type }),
            };
            return serde_json::to_vec(&compact).unwrap_or_default();
        }
    }

    // Large blob JSON is intentionally read only as a prefix, so a complete
    // serde parse can end at EOF. Discriminators are near the envelope start;
    // recover those simple string fields without reading the full record.
    let raw = String::from_utf8_lossy(bytes);
    if let Some(record_type) = json_string_field(&raw, "type", 0) {
        let compact = if record_type == "event_msg" {
            let payload_start = raw.find("\"payload\"").unwrap_or(0);
            let event_type = json_string_field(&raw, "type", payload_start).unwrap_or("");
            serde_json::json!({
                "type": record_type,
                "payload": { "type": event_type }
            })
        } else {
            serde_json::json!({ "type": record_type })
        };
        return serde_json::to_vec(&compact).unwrap_or_default();
    }

    compact_text_bytes(bytes)
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

/// Extract one simple JSON string field from a prefix.
fn json_string_field<'a>(raw: &'a str, field: &str, start: usize) -> Option<&'a str> {
    let tail = raw.get(start..)?;
    let needle = format!("\"{field}\"");
    let field_offset = tail.find(&needle)?.saturating_add(needle.len());
    let after_field = tail.get(field_offset..)?;
    let colon = after_field.find(':')?;
    let value = after_field.get(colon.saturating_add(1)..)?.trim_start();
    let quoted = value.strip_prefix('"')?;
    let end = quoted.find('"')?;
    quoted.get(..end)
}

/// Bound one inline byte vector as UTF-8 display text.
fn compact_inline_bytes(bytes: &mut Vec<u8>) {
    *bytes = compact_text_bytes(bytes);
}

/// Bound arbitrary bytes as lossy UTF-8 display text.
fn compact_text_bytes(bytes: &[u8]) -> Vec<u8> {
    compact_text(&String::from_utf8_lossy(bytes)).into_bytes()
}

/// Bound display text by Unicode scalar count, appending an ellipsis on cut.
fn compact_text(text: &str) -> String {
    let mut chars = text.chars();
    let mut compact: String = chars.by_ref().take(DISPLAY_PREVIEW_CHAR_LIMIT).collect();
    if chars.next().is_some() {
        compact.push('…');
    }
    compact
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

impl Workspace {
    /// Create a workspace from an existing projection (used in tests).
    #[must_use]
    pub fn from_projection(projection: HistoryProjection) -> Self {
        let source_ops = projection.ops.clone();
        let source_op_index = source_ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.id, index))
            .collect();
        Self {
            projection,
            source_ops,
            source_op_index,
            blob_resolver: None,
            repositories: Vec::new(),
            diagnostics: OpenDiagnostics::default(),
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
        let (source_ops, chain_stats) = read_chain_ops(&chain_path)?;
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
        let options = editchain_project::ProjectionOptions {
            bundle_metadata: true,
        };
        let mut projection = HistoryProjection::from_ops_with(projection_ops, options);
        let repositories = discover_repositories(&PathBuf::from(workspace_path))?;
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
        // Stitch sessions and git history into a single edit chain.
        projection.link_history();
        let source_op_index = source_ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.id, index))
            .collect();
        Ok(Self {
            projection,
            source_ops,
            source_op_index,
            blob_resolver: Some(resolver),
            repositories,
            diagnostics,
            current_view: None,
        })
    }

    /// Get a window of history rows (newest-first).
    #[must_use]
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::needless_borrow,
        reason = "expanded-slot prefix sums are bounded by node count; indexing is bounds-checked by partition_point; node is a &HistoryNode reference"
    )]
    pub fn history_window(&mut self, options: HistoryWindowOptions<'_>) -> HistoryWindow {
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
                layout_ready: include_layout,
            };
        };
        let filtered = &snapshot.nodes;
        let ctx = snapshot.context.as_ref();

        // The service emits a FIXED fully-expanded flat list: every combined op
        // always occupies its stable 1+N absolute slots (parent + one per bundled
        // sub-op), so fetch/cache indices never move regardless of reveal state.
        // Collapse/expand is purely a client rendering decision; scroll offsets are
        // derived from how many slots are currently visible via prefix sums over
        // per-node sub-op counts.
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
            let block_start = starts[abs_idx];
            // Per-row graph geometry from the layout context (absolute row index
            // into the full sorted list).
            let (lane, above, below, transitions) = ctx.map_or_else(
                || (0, Vec::new(), Vec::new(), Vec::new()),
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
                    )
                },
            );
            let parent_row = block_start;
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
                    group: node.group(),
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
                    sub_ops: sub_op_summaries(node.sub_ops()),
                    is_subop: false,
                    parent_row: None,
                    subop_kind: None,
                });
            }
            // Emit each bundled sub-op as its own row immediately after its parent.
            let summaries = sub_op_summaries(node.sub_ops());
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
            for (i, sub) in summaries.iter().enumerate() {
                let slot = block_start + 1 + i;
                if slot < offset_usize || slot >= end_usize {
                    continue;
                }
                rows.push(HistoryRow {
                    op_id: Some(sub.op_id.clone()),
                    git_oid: None,
                    repository: None,
                    summary: sub.summary.clone(),
                    timestamp_ms: sub.timestamp_ms,
                    group: node.group(),
                    // Sub-op rows are not graph nodes; key them under their parent so
                    // group-start detection and click routing stay unambiguous.
                    node_key: format!("{}::sub:{i}", node.node_key()),
                    parents: Vec::new(),
                    parent_relations: Vec::new(),
                    is_submodule: false,
                    is_system: true,
                    author: String::new(),
                    commit_id: String::new(),
                    kind: sub.kind.clone(),
                    // No dot of its own — draw every pass-through lane as a
                    // full-height straight line (both halves meet at midY).
                    lane,
                    above: region_lanes.clone(),
                    below: region_lanes.clone(),
                    transitions: Vec::new(),
                    sub_ops: Vec::new(),
                    is_subop: true,
                    parent_row: Some(parent_row),
                    subop_kind: Some(subop_semantic_class(&sub.kind)),
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
        let sub_op_counts: Vec<usize> = nodes.iter().map(|node| node.sub_ops().len()).collect();
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
        self.current_view = Some((
            key,
            ViewSnapshot {
                nodes,
                context: None,
                sub_op_counts,
                starts,
                expanded_total,
                max_lane: 0,
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
            let index = self.source_op_index.get(&op_id).copied()?;
            let mut op = self.source_ops.get(index)?.clone();
            if let Some(resolver) = &self.blob_resolver {
                let mut stats = BlobHydrationStats::default();
                hydrate_kind(&mut op.kind, resolver, &mut stats);
            }
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

/// Convert an optional protocol filter DTO into a [`ChainFilter`].
///
/// A `None` DTO yields the default filter (hide undated, splice on), matching
/// the webview's default behavior. An empty DTO yields an empty filter that
/// hides nothing.
#[must_use]
fn chain_filter_from_dto(dto: Option<&ChainFilterDto>) -> ChainFilter {
    match dto {
        Some(d) => ChainFilter::new(
            d.summary_pattern.clone(),
            d.kind_pattern.clone(),
            d.include_kind_pattern.clone(),
            d.hide_undated,
            d.splice,
        ),
        None => ChainFilter::default(),
    }
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
    chunk: &editchain_query::search::ScoredChunk,
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
        editchain_project::HistoryNode::GitCommit(_) => false,
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
        editchain_project::HistoryNode::CollapsedImport { author, .. } => author.clone(),
        editchain_project::HistoryNode::GitCommit(commit) => payload_text(&commit.author.name),
    }
}

/// Build the bundled sub-op summaries for a row from its attached metadata ops.
///
/// Each bundled sub-op is a raw `Import` op tagged `META`. Its summary is the
/// record type (derived from the raw JSONL's `type` field when parseable, else
/// the raw reference text), so the viewer can label each revealed sub-row.
#[must_use]
fn sub_op_summaries(sub_ops: &[std::sync::Arc<Op>]) -> Vec<editchain_protocol::SubOpSummary> {
    sub_ops
        .iter()
        .map(|op| {
            let (summary, kind) = sub_op_label(op);
            editchain_protocol::SubOpSummary {
                op_id: op.id.to_string(),
                summary,
                kind,
                timestamp_ms: op.clock.as_u64(),
            }
        })
        .collect()
}

/// Map the projection's provider-neutral relation kind to the protocol enum.
///
/// The projection derives kinds from `SubagentOf` / `ReconnectsTo` / `ForkOf`
/// structural notes; the protocol enum has exactly those three variants plus a
/// forward-compatible `Unknown` (never produced by this service today).
#[must_use]
fn protocol_relation_kind(kind: editchain_project::RelationKind) -> ParentRelationKind {
    match kind {
        editchain_project::RelationKind::Subagent => ParentRelationKind::Subagent,
        editchain_project::RelationKind::Reconnect => ParentRelationKind::Reconnect,
        editchain_project::RelationKind::Fork => ParentRelationKind::Fork,
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
            let label = if record_type == "event_msg" {
                value
                    .get("payload")
                    .and_then(|payload| payload.get("type"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|event_type| !event_type.is_empty())
                    .unwrap_or(record_type)
            } else {
                record_type
            };
            return (label.to_string(), label.to_string());
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
type ChainReadResult = Result<(Vec<Op>, ChainReadStats), Box<dyn std::error::Error>>;

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
fn read_chain_ops(chain_dir: &PathBuf) -> ChainReadResult {
    if chain_dir.as_os_str().is_empty() {
        return Ok((Vec::new(), ChainReadStats::default()));
    }
    let store = SegmentStore::open(chain_dir)?;
    let pages = store.read_all()?;
    let mut opset = OpSet::new();
    let mut accepted: Vec<Op> = Vec::new();
    let mut stats = ChainReadStats::default();
    for page in pages {
        for record in page.records {
            let Ok(op) = decode_op(&record.data) else {
                continue;
            };
            stats.records = stats.records.saturating_add(1);
            match opset.insert(op.id, record.data) {
                Ok(true) => {
                    stats.accepted = stats.accepted.saturating_add(1);
                    accepted.push(op);
                }
                Ok(false) => stats.duplicates = stats.duplicates.saturating_add(1),
                Err(_) => stats.quarantined = stats.quarantined.saturating_add(1),
            }
        }
    }
    // Match the OpSet's canonical `OpId` key order without a second decode
    // pass — decoding 100k+ records twice is the dominant Open cost in debug.
    accepted.sort_by_key(|op| op.id);
    Ok((accepted, stats))
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
                    "nodes": self.workspace.as_ref().map_or(0, |w| w.projection.len()),
                    "chain_generation": self.workspace.as_ref().map_or(0, |w| w.projection.ops.len()),
                    // Canonicalization + lazy blob access outcomes for this open.
                    // New keys: backward-compatible; older clients ignore them.
                    "diagnostics": serde_json::to_value(diagnostics)?,
                    "warnings": warnings,
                }))
            }
            RequestBody::GetWindow(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let window = ws.history_window(HistoryWindowOptions {
                    offset: req.offset,
                    limit: req.limit,
                    hide_submodules: req.hide_submodules,
                    filter: &filter,
                    include_layout: req.include_layout,
                });
                ResponseBody::Ok(serde_json::to_value(window)?)
            }
            RequestBody::GetLayout(req) => {
                let ws = self.workspace.as_mut().ok_or("no workspace open")?;
                let filter = chain_filter_from_dto(req.filter.as_ref());
                let layout = ws.graph_layout(req.hide_submodules, req.offset, req.limit, &filter);
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
                    let ws = self.workspace.as_ref().ok_or("no workspace open")?;
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
    use editchain_codec::page::Page;
    use editchain_core::{ImportOp, MessageOp, PathId};
    use editchain_import::BlobSink as _;

    /// 2^53 + 1 — the first integer JavaScript's IEEE-754 doubles round.
    const OVER_2_53: u64 = 9_007_199_254_740_993;

    /// Write ops into a chain directory as a single segment page.
    fn write_chain(chain_dir: &std::path::Path, ops: &[Op]) {
        let mut store = SegmentStore::open(chain_dir).unwrap();
        let mut page = Page::new(0);
        for op in ops {
            page.add_record(0, encode_op(op).unwrap());
        }
        store.append_page(&page).unwrap();
    }

    /// Store a blob in a chain's durable blob store, returning its reference.
    fn store_blob(chain_dir: &std::path::Path, data: &[u8]) -> BlobRef {
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
        Op {
            id: OpId::new(NodeId(node), 0, seq),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(10)),
            tags,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(
                    format!(r#"{{"type":"last-prompt","seq":{seq}}}"#).into_bytes(),
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

    /// Build a structural relationship note (the shape `emit_codex_relationship_notes`
    /// produces): causal parent `parent`, targets `targets`, META-tagged so the
    /// projection folds it out of rendered rows and reads it as a virtual edge.
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
                content: Payload::Empty,
            }),
        }
    }

    #[test]
    fn history_window_exposes_structural_relationship_kinds() {
        // A parent thread (node 1) spawns a subagent thread (node 2) and
        // reconnects into it; a third thread (node 3) forks off the parent.
        // The structural notes drive untyped virtual edges in the projection;
        // the service must tag those edges with their provider-neutral kinds.
        let trunk = import_op(1, 1, false);
        let spawn_marker = message_op(1, 3, trunk.id);
        let sub_first = import_op(2, 1, false);
        let sub_last = message_op(2, 5, sub_first.id);
        let completion = message_op(1, 7, spawn_marker.id);
        let branch_first = import_op(3, 1, false);

        let ops = vec![
            trunk.clone(),
            spawn_marker.clone(),
            sub_first.clone(),
            sub_last.clone(),
            completion.clone(),
            branch_first.clone(),
            structural_note(
                OpId::new(NodeId(1), 0, 0xFFFC),
                sub_first.id,
                vec![spawn_marker.id],
                editchain_core::NoteRelationship::SubagentOf,
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

        // SubagentOf: the subagent thread's first op carries a "subagent"
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
        let meta = import_op(1, 3, true);

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
        assert_eq!(window.rows[0].parent_row, None);
        // The expanded sub-op row follows its parent and inherits its lane.
        assert!(window.rows[1].is_subop);
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
        assert_eq!(deep.rows[0].op_id, Some(meta.id.to_string()));
        assert!(deep.sub_op_counts.is_none());
    }

    #[test]
    fn meta_bundle_default_standalone_opt_in_bundles() {
        // META imports render standalone by default (no cross-session grouping).
        // Only when META bundling is re-enabled do they collapse into the nearest
        // preceding real node as an expanded sub-op row.
        let turn = import_op(1, 1, false);
        let msg = message_op(1, 2, turn.id);
        let meta = import_op(1, 3, true);

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
        let meta = import_op(5, 7, true); // bundled under turn

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
        let (ops, _stats) = read_chain_ops(&PathBuf::from(
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
        std::fs::create_dir_all(&workspace_path).unwrap();
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
        std::fs::create_dir_all(&workspace_path).unwrap();
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
        std::fs::write(sink.path_for(&corrupt_hash), b"corrupted bytes").unwrap();

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
        std::fs::create_dir_all(&workspace_path).unwrap();
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
            blob_resolver: Some(resolver),
            repositories: Vec::new(),
            diagnostics: OpenDiagnostics::default(),
            current_view: None,
        };
        let details = ws
            .node_details(Some(source.id.to_string()), None)
            .expect("details");
        assert_eq!(details.body, full_text);
    }

    #[test]
    fn row_first_window_precedes_global_layout() {
        let first = message_op(1, 1, OpId::new(NodeId(0), 0, 0));
        let second = message_op(1, 2, first.id);
        let projection = HistoryProjection::from_ops(vec![first, second]);
        let mut ws = Workspace::from_projection(projection);
        let filter = ChainFilter::new(String::new(), String::new(), String::new(), false, false);

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
}
