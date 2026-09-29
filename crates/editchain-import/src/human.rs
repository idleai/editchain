//! Exact capture of `editchain-human-history` JSONL archives.
//!
//! The engine retains complete archive lines, including envelope fields and
//! whitespace, independently of editor-specific validation and work derivation.
//! Unknown or malformed records remain raw evidence and are counted as malformed.
//! Known records use the recorder's full session/sequence identity, so overlapping
//! archives and relocated files do not duplicate observations. Reusing that
//! identity with different bytes retains a conflict, including after a rewrite.
//!
//! Archive operations have their own namespace. [`native_event_id`] identifies
//! the live recorder operation; an `OccurrenceOf` note relates the two. Import
//! never invents the live recorder's cross-window causal parents. Editor capture,
//! validation and human-work materialization remain with the editor adapter.

use std::io;
use std::path::{Path, PathBuf};

use editchain_core::human::HumanIdentity;
use editchain_core::{
    ActorId, Clock, ImportOp, NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind, ParentSet,
    Payload, ScopeRef, Tags,
};
use serde::{Deserialize, Serialize};

use crate::native::{NativeIdentity, NativeMapping};
use crate::sink::{emit_op, payload_for, EmissionKind};
use crate::source_read::{LineWithHash, SourceReadPlan, SourceReadState};
use crate::{
    derive_external_entity_id, derive_node_id, derive_session_id, resolve_source_cursor, BlobSink,
    CursorStore, ImportError, ImportOptions, ImportReport, OpSink, SourcePosition, SourceStream,
};

/// Caller-selected archives and optional filter for the recorded capture root.
#[derive(Debug, Clone)]
pub struct HumanImportRequest {
    /// A JSONL archive or a directory containing JSONL archives recursively.
    pub source: PathBuf,
    /// Exact recorded `workspace_path` to include; `None` includes every root.
    /// This is capture provenance, not a product workspace or current directory.
    pub recorded_root: Option<String>,
}

/// Native recorder header with opaque editor payload and exact bytes kept separately.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HumanEvent {
    /// Recorder schema; this adapter recognizes version one.
    pub schema: u32,
    /// Full recorder incarnation.
    pub session: String,
    /// One-based event identity within the incarnation.
    pub sequence: u64,
    /// Recorded wall time; never used for identity or deduplication.
    pub time_ms: u64,
    /// Optional local person attribution, separate from the recorder incarnation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<HumanIdentity>,
    /// Uninterpreted editor observation, including unknown additive fields.
    pub event: serde_json::Value,
}

/// A recognized archive envelope. Raw bytes, rather than reserialization of
/// this view, are authoritative for fields unknown to this library version.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HumanArchiveRecord {
    /// Envelope discriminator, `editchain-human-history`.
    pub format: String,
    /// Envelope schema, one.
    pub schema: u32,
    /// Capture root recorded by the producer.
    pub workspace_path: String,
    /// Native recorder identity and opaque observation.
    pub event: HumanEvent,
}

impl HumanArchiveRecord {
    /// Recognize a complete supported envelope without interpreting editor content.
    #[must_use]
    pub fn parse(raw: &[u8]) -> Option<Self> {
        let record: Self = serde_json::from_slice(raw).ok()?;
        (record.format == "editchain-human-history"
            && record.schema == 1
            && record.event.schema == 1
            && record.event.session.len() == 36
            && record
                .event
                .session
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
            && (1..=9_007_199_254_740_991).contains(&record.event.sequence))
        .then_some(record)
    }

    /// Full native identity, independent of archive filename and generation.
    #[must_use]
    pub fn native_identity(&self) -> NativeIdentity {
        NativeIdentity::Human {
            session: self.event.session.clone(),
            sequence: self.event.sequence,
        }
    }

    fn archive_id(&self) -> Result<editchain_core::SourceId, ImportError> {
        Ok(crate::ids::derive_external_entity_source(
            "human:archive-record:v1",
            &serde_json::to_string(&self.native_identity())?,
        ))
    }
}

/// Derive the existing live editor operation identity without changing its contract.
///
/// This is the recorder session/sequence mapping used before archive imports
/// existed. It is distinct from archive evidence IDs and persistent person IDs.
///
/// # Errors
/// Returns an encoding error if the hash cannot supply the fixed identity bytes.
pub fn native_event_source(session: &str, sequence: u64) -> io::Result<editchain_core::SourceId> {
    let hash = blake3::derive_key("editchain.vscode.editor.session.v1", session.as_bytes());
    let mut node = [0_u8; 8];
    node.copy_from_slice(
        hash.get(..8)
            .ok_or_else(|| io::Error::other("invalid hash"))?,
    );
    let mut boot = [0_u8; 4];
    boot.copy_from_slice(
        hash.get(8..12)
            .ok_or_else(|| io::Error::other("invalid hash"))?,
    );
    Ok(editchain_core::SourceId::new(
        NodeId(u64::from_le_bytes(node)),
        u32::from_le_bytes(boot),
        sequence,
    ))
}

/// Recover a native mapping from retained raw archive bytes.
#[must_use]
pub fn human_mapping(source: editchain_core::SourceId, raw: &[u8]) -> Option<NativeMapping> {
    Some(NativeMapping {
        identity: HumanArchiveRecord::parse(raw)?.native_identity(),
        source,
        raw_hash: crate::hash_raw(raw),
        outputs: Vec::new(),
        incarnation: None,
    })
}

/// Capture complete human archive records using the shared resumable cursor contract.
///
/// Prefer [`crate::capture_import`] for a private checkpoint overlay and durable
/// acceptance. This lower-level function stages cursor updates in `cursors`;
/// callers must persist operations and blobs before committing those updates.
/// Partial final lines stay pending. Filtering uses the exact recorded root;
/// files are captured before parsing, so later source mutations cannot change
/// admitted bytes. A directory relocation preserves provider-relative cursors.
///
/// # Errors
/// Returns discovery, capture, cancellation, identity, or sink errors.
pub fn import_human(
    request: &HumanImportRequest,
    options: &ImportOptions,
    ops: &mut dyn OpSink,
    blobs: &mut dyn BlobSink,
    cursors: &mut dyn CursorStore,
) -> Result<ImportReport, ImportError> {
    options.cancellation.check(&request.source)?;
    let files = archives(&request.source, &options.cancellation)?;
    import_human_files((request, &files), options, ops, blobs, cursors)
}

pub(crate) fn import_human_files(
    selection: (&HumanImportRequest, &[PathBuf]),
    options: &ImportOptions,
    ops: &mut dyn OpSink,
    blobs: &mut dyn BlobSink,
    cursors: &mut dyn CursorStore,
) -> Result<ImportReport, ImportError> {
    let (request, files) = selection;
    let root = if request.source.is_file() {
        request.source.parent().unwrap_or_else(|| Path::new(""))
    } else {
        &request.source
    };
    let mut report = ImportReport {
        files_discovered: files.len(),
        ..ImportReport::default()
    };
    for path in files {
        options.cancellation.check(path)?;
        // A caller selecting another recorded root must not inherit a cursor
        // that previously skipped those records. Raw identities remain shared.
        let provider = request.recorded_root.as_ref().map_or_else(
            || "human".to_owned(),
            |root| format!("human:root:{}", blake3::hash(root.as_bytes())),
        );
        let resolved = resolve_source_cursor(cursors, &provider, root, path, "")?;
        let plan = SourceReadPlan::capture_controlled(
            path,
            resolved.cursor.as_ref(),
            cursors.get_generation(&resolved.state_key)?,
            cursors.get_reservation(&resolved.canonical_key)?.as_ref(),
            &options.source_control(),
        )?;
        if plan.state() == SourceReadState::Unchanged {
            continue;
        }
        report.files_processed = report.files_processed.saturating_add(1);
        let stream = SourceStream::new(resolved.source_node, plan.generation());
        for (index, line) in plan.lines().iter().enumerate() {
            options.cancellation.check(path)?;
            let record = HumanArchiveRecord::parse(&line.data);
            if request.recorded_root.as_ref().is_some_and(|root| {
                record
                    .as_ref()
                    .is_none_or(|record| &record.workspace_path != root)
            }) {
                continue;
            }
            let ordinal = plan
                .start_seq()
                .checked_add(u64::try_from(index).map_err(io::Error::other)?)
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or_else(|| ImportError::CursorStore("human record ordinal exhausted".into()))?;
            let fallback = stream.source_position(SourcePosition::raw(ordinal))?;
            let raw = raw_op(record.as_ref(), line, fallback, blobs)?;
            emit_op(&raw, ops, &mut report, EmissionKind::Raw)?;
            if let Some(record) = record {
                // Identity evidence is always retained, even for raw-only capture.
                let mapping = occurrence(&record, &raw, line.hash)?;
                emit_op(&mapping, ops, &mut report, EmissionKind::Derived)?;
            } else {
                report.malformed = report.malformed.saturating_add(1);
            }
        }
        let mut checkpoint = plan.checkpoint().clone();
        checkpoint.source_node = Some(resolved.source_node);
        options.cancellation.check(path)?;
        cursors.set_generation(&resolved.canonical_key, plan.generation())?;
        cursors.set_cursor(&resolved.canonical_key, &checkpoint)?;
    }
    Ok(report)
}

fn raw_op(
    record: Option<&HumanArchiveRecord>,
    line: &LineWithHash,
    fallback: editchain_core::SourceId,
    blobs: &mut dyn BlobSink,
) -> Result<Op, ImportError> {
    let mut raw = Op {
        source: Some(fallback),
        id: fallback.id(),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::IMPORT | Tags::HUMAN,
        kind: OpKind::Import(ImportOp {
            raw_ref: payload_for(&line.data, blobs)?,
            raw_hash: Some(line.hash),
        }),
    };
    if let Some(record) = record {
        let source = record.archive_id()?;
        raw.id = source.id();
        raw.source = Some(source);
        raw.clock = Clock::UnixMs(record.event.time_ms);
        let native = native_event_source(&record.event.session, record.event.sequence)?;
        raw.actor = ActorId(native.node.0);
        if let Some(identity) = &record.event.identity {
            raw.actor = ActorId(derive_node_id(&format!("human:unsigned:{}", identity.guid)).0);
            raw.scope = ScopeRef::Session(derive_session_id(&format!(
                "vscode.human:{}:{}",
                identity.guid, identity.stream
            )));
        }
    } else {
        raw.tags |= Tags::ERROR;
    }
    Ok(raw)
}

fn occurrence(
    record: &HumanArchiveRecord,
    raw: &Op,
    raw_hash: [u8; 32],
) -> Result<Op, ImportError> {
    let mapping = NativeMapping {
        identity: record.native_identity(),
        source: crate::ids::provenance(raw)?,
        raw_hash,
        outputs: Vec::new(),
        incarnation: None,
    };
    let content = serde_json::to_string(&mapping)?;
    Ok(Op {
        source: Some(crate::ids::derive_external_entity_source(
            "human:archive-mapping:v1",
            &content,
        )),
        id: derive_external_entity_id("human:archive-mapping:v1", &content),
        parents: ParentSet::One(raw.id),
        actor: raw.actor,
        clock: raw.clock,
        scope: raw.scope,
        tags: Tags::META | Tags::IMPORT | Tags::HUMAN,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![native_event_id(
                &record.event.session,
                record.event.sequence,
            )?],
            relationship: NoteRelationship::OccurrenceOf,
            content: Payload::Inline(content.into_bytes()),
        }),
    })
}

pub(crate) fn archives(
    source: &Path,
    cancellation: &crate::cancellation::ImportCancellation,
) -> Result<Vec<PathBuf>, ImportError> {
    let metadata = std::fs::metadata(source)?;
    if metadata.is_file() {
        return Ok(vec![source.to_path_buf()]);
    }
    let mut pending = vec![source.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        cancellation.check(&directory)?;
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "jsonl") {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Canonical identity of a native editor event, preserving its producer contract.
/// # Errors
/// Returns an error if the producer descriptor cannot be encoded.
pub fn native_event_id(session: &str, sequence: u64) -> io::Result<OpId> {
    native_event_source(session, sequence).map(editchain_core::SourceId::id)
}
