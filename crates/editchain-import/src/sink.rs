use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use editchain_codec::frame::{encode_op, encoded_op_len};
use editchain_core::payload;
use editchain_core::{Admission, BlobRef, ContentId, NodeId, NoteRelationship, Op, OpKind, OpSet};

use crate::error::ImportError;
use crate::ids::hash_raw;

/// A sink for retaining typed operation variants and reporting their admission.
pub trait OpSink {
    /// Retain a typed operation; admission is relative to this sink's evidence.
    /// This acknowledgment does not imply durability.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the operation cannot be stored.
    fn accept_op(&mut self, op: &Op) -> Result<Admission, ImportError>;
}

#[derive(Clone, Copy)]
pub(crate) enum EmissionKind {
    Raw,
    Derived,
}

pub(crate) fn emit_op(
    op: &Op,
    sink: &mut dyn OpSink,
    report: &mut crate::model::ImportReport,
    kind: EmissionKind,
) -> Result<(), ImportError> {
    match sink.accept_op(op)? {
        Admission::Duplicate => {
            report.duplicates = report.duplicates.saturating_add(1);
            return Ok(());
        }
        Admission::Conflict => report.op_conflicts = report.op_conflicts.saturating_add(1),
        Admission::Accepted => {}
    }
    if matches!(kind, EmissionKind::Raw) {
        report.raw_ops = report.raw_ops.saturating_add(1);
    } else if matches!(&op.kind, OpKind::Note(note) if note.relationship == NoteRelationship::ProviderEvidence)
    {
        report.evidence_ops = report.evidence_ops.saturating_add(1);
    } else {
        report.normalized_ops = report.normalized_ops.saturating_add(1);
    }
    Ok(())
}

/// A sink for accepting large blob payloads.
pub trait BlobSink {
    /// Store a blob and return a content identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the blob cannot be stored.
    fn store_blob(&mut self, data: &[u8]) -> Result<(), ImportError>;

    /// Store a blob and return a `BlobRef` referencing it.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the blob cannot be stored.
    fn put(&mut self, data: &[u8]) -> Result<BlobRef, ImportError> {
        let len = u32::try_from(data.len()).map_err(|error| {
            ImportError::BlobSink(format!(
                "blob length exceeds the 32-bit reference format: {error}"
            ))
        })?;
        let hash = hash_raw(data);
        let id = ContentId::Hash256(hash);
        self.store_blob(data)?;
        Ok(BlobRef { id, len })
    }
}

/// Inline payload threshold — payloads above this size are spilled to blobs.
pub const INLINE_LIMIT: usize = 4096;

/// Choose between inline and blob storage based on payload size.
///
/// # Errors
///
/// Returns [`ImportError`] if the blob sink fails to store the payload.
pub fn payload_for(
    bytes: &[u8],
    blobs: &mut dyn BlobSink,
) -> Result<payload::Payload, ImportError> {
    if bytes.len() <= INLINE_LIMIT {
        Ok(payload::Payload::Inline(bytes.to_vec()))
    } else {
        let blob_ref = blobs.put(bytes)?;
        Ok(payload::Payload::Blob(blob_ref))
    }
}

/// A store for persisting per-source read cursors.
pub trait CursorStore {
    /// Read a proposed source prefix whose operation IDs were reserved before
    /// append. It constrains generation reuse without advancing accepted bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if reservation state cannot be read.
    fn get_reservation(&self, key: &str) -> Result<Option<CursorValue>, ImportError>;

    /// Durably reserve the generation and source prefix before appending any
    /// operations that use them. Accepted cursor reads remain unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if the reservation cannot be stored.
    fn reserve_checkpoint(&mut self, key: &str, cursor: &CursorValue) -> Result<(), ImportError>;

    /// Read the cursor for a source key.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the cursor cannot be read.
    fn get_cursor(&self, key: &str) -> Result<Option<CursorValue>, ImportError>;
    /// Write the cursor for a source key.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the cursor cannot be written.
    fn set_cursor(&mut self, key: &str, cursor: &CursorValue) -> Result<(), ImportError>;

    /// Read the persisted boot generation for a source key.
    ///
    /// The generation counter is bumped whenever an import detects that a
    /// source's accepted byte prefix changed since the last read; it selects
    /// the deterministic boot epoch for the source's op ids. Stores that do not
    /// track generations return `0` (the original generation), which keeps
    /// the Claude importer's boot behavior unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the generation cannot be read.
    fn get_generation(&self, _path: &str) -> Result<u32, ImportError> {
        Ok(0)
    }
    /// Persist the boot generation for a source key.
    ///
    /// Filesystem stores stage the value in memory like [`Self::set_cursor`];
    /// nothing reaches disk until [`Self::commit`] runs.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the generation cannot be written.
    fn set_generation(&mut self, _path: &str, _generation: u32) -> Result<(), ImportError> {
        Ok(())
    }

    /// Persist any staged cursor mutations to durable storage.
    ///
    /// The default implementation is a no-op for in-memory stores. Filesystem
    /// stores buffer [`Self::set_cursor`] calls in memory and only write them
    /// here, so a caller can make cursors durable strictly after the
    /// operations they cover have been durably appended. Stores that track
    /// generations persist a staged generation bump before any cursor file
    /// that depends on it, so a crash or write error between the two never
    /// leaves a durable cursor ahead of its generation.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if a staged cursor cannot be written.
    fn commit(&mut self) -> Result<(), ImportError> {
        Ok(())
    }
}

/// Accepted coverage of a named semantic derivation over a source prefix.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MaterializationCheckpoint {
    /// Named provider derivation contract, separate from metadata migrations.
    pub contract: String,
    /// Last physical record covered by this derivation checkpoint.
    pub through: u64,
    /// Whether private reasoning has been captured through the accepted prefix.
    pub includes_thinking: bool,
}

/// A cursor value representing how far we've read in a source file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CursorValue {
    /// Generation of the accepted bytes in this cursor. Older cursors omit it
    /// and use the legacy generation map until their first successful capture.
    /// Keeping it beside the accepted hash makes rewrite replay stable after a
    /// crash between persisting a proposed generation and its new cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_generation: Option<u32>,
    /// File size at last read (for generation detection).
    pub file_size: u64,
    /// Byte offset we've read up to.
    pub byte_offset: u64,
    /// Number of ops emitted from this file.
    pub ops_emitted: u64,
    /// Blake3 hash of all content up to `byte_offset` (for integrity).
    pub content_hash: [u8; 32],
    /// Hash contract for `content_hash`: zero is the legacy rolling scheme;
    /// version one is direct BLAKE3 over exactly `0..byte_offset`.
    #[serde(default)]
    pub content_hash_version: u32,
    /// Stable node that owns this source's operation IDs.
    ///
    /// Legacy cursors omit it; migration derives their original path-based node
    /// once and then carries it across sessions-root relocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node: Option<NodeId>,
    /// Importer-owned normalized projection version applied to this source.
    /// Older cursor JSON omits this field and therefore upgrades from zero.
    #[serde(default)]
    pub normalization_version: u32,
    /// Accepted semantic derivation, when normalization has been requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<MaterializationCheckpoint>,
    /// Content hash of the provider-owned session-title record last captured
    /// for this source. Codex titles live beside rollouts rather than inside
    /// them, so this lets an unchanged rollout reproject when only its title
    /// changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title_hash: Option<[u8; 32]>,
}

/// Bounds on distinct operations retained by one capture sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchLimits {
    /// Maximum distinct operation variants, including conflicting variants.
    pub operations: usize,
    /// Maximum combined Postcard bytes of the retained variants.
    pub encoded_bytes: u64,
}

impl Default for BatchLimits {
    fn default() -> Self {
        Self {
            operations: 1_000_000,
            encoded_bytes: 256 * 1024 * 1024,
        }
    }
}

/// A bounded memory sink retaining every distinct variant, including conflicts.
/// Canonical evidence remains immutable even if the public inspection vector
/// is consumed or changed by a compatibility caller.
#[derive(Debug, Default)]
pub struct MemoryOpSink {
    /// Distinct retained operations in emission order.
    pub ops: Vec<Op>,
    evidence: OpSet,
    retained_variants: usize,
    encoded_bytes: u64,
    limits: BatchLimits,
}

impl MemoryOpSink {
    /// Create a new empty memory op sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a capture sink with explicit aggregate resource limits.
    #[must_use]
    pub fn with_limits(limits: BatchLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    /// Consume the capture and release its encoded admission index.
    #[must_use]
    pub fn into_operations(self) -> Vec<Op> {
        self.ops
    }
}

impl OpSink for MemoryOpSink {
    fn accept_op(&mut self, op: &Op) -> Result<Admission, ImportError> {
        let length = encoded_op_len(op).map_err(|error| ImportError::OpSink(error.to_string()))?;
        let bytes = u64::try_from(length).map_err(io::Error::other)?;
        if bytes > u64::from(editchain_codec::scan::MAX_RECORD_BYTES) {
            return Err(ImportError::OpSink(
                "encoded operation exceeds the 64 MiB record limit".into(),
            ));
        }
        let encoded = encode_op(op).map_err(|error| ImportError::OpSink(error.to_string()))?;
        let admission = self.evidence.classify(op.id, &encoded);
        if admission == Admission::Duplicate {
            return Ok(admission);
        }
        let total = self
            .encoded_bytes
            .checked_add(bytes)
            .ok_or_else(|| ImportError::OpSink("capture byte count exhausted".into()))?;
        if self.retained_variants >= self.limits.operations || total > self.limits.encoded_bytes {
            return Err(ImportError::OpSink(format!(
                "capture exceeds batch limit ({} operation variants, {} encoded bytes)",
                self.limits.operations, self.limits.encoded_bytes
            )));
        }
        let retained = self.evidence.insert(op.id, encoded);
        self.ops.push(op.clone());
        self.retained_variants = self.retained_variants.saturating_add(1);
        self.encoded_bytes = total;
        Ok(retained)
    }
}

/// A memory-backed blob sink for testing.
#[derive(Debug, Default)]
pub struct MemoryBlobSink {
    /// Stored blob payloads.
    pub blobs: Vec<Vec<u8>>,
}

impl MemoryBlobSink {
    /// Create a new empty memory blob sink.
    #[must_use]
    pub fn new() -> Self {
        Self { blobs: Vec::new() }
    }
}

impl BlobSink for MemoryBlobSink {
    fn store_blob(&mut self, data: &[u8]) -> Result<(), ImportError> {
        self.blobs.push(data.to_vec());
        Ok(())
    }
}

/// A memory-backed blob sink that returns content-addressed `BlobRef`s.
/// Stores blobs keyed by their BLAKE3 hash for deduplication.
#[derive(Debug, Default)]
pub struct ContentAddressedBlobSink {
    blobs: std::collections::HashMap<[u8; 32], Vec<u8>>,
}

impl ContentAddressedBlobSink {
    /// Create a new empty content-addressed blob sink.
    #[must_use]
    pub fn new() -> Self {
        Self {
            blobs: std::collections::HashMap::new(),
        }
    }

    /// Retrieve a blob by its BLAKE3 hash.
    #[must_use]
    pub fn get(&self, hash: &[u8; 32]) -> Option<&[u8]> {
        self.blobs.get(hash).map(Vec::as_slice)
    }

    /// Number of stored blobs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    /// Returns true if no blobs are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}

impl BlobSink for ContentAddressedBlobSink {
    fn store_blob(&mut self, data: &[u8]) -> Result<(), ImportError> {
        let hash = hash_raw(data);
        let _: &mut Vec<u8> = self.blobs.entry(hash).or_insert_with(|| data.to_vec());
        Ok(())
    }
}

/// A memory-backed cursor store for testing.
#[derive(Debug, Default)]
pub struct MemoryCursorStore {
    cursors: std::collections::HashMap<String, CursorValue>,
    generations: std::collections::HashMap<String, u32>,
    reservations: std::collections::HashMap<String, CursorValue>,
}

impl MemoryCursorStore {
    /// Create a new empty memory cursor store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cursors: std::collections::HashMap::new(),
            generations: std::collections::HashMap::new(),
            reservations: std::collections::HashMap::new(),
        }
    }
}

impl CursorStore for MemoryCursorStore {
    fn get_reservation(&self, key: &str) -> Result<Option<CursorValue>, ImportError> {
        Ok(self.reservations.get(key).cloned())
    }

    fn reserve_checkpoint(&mut self, key: &str, cursor: &CursorValue) -> Result<(), ImportError> {
        drop(self.reservations.insert(key.to_string(), cursor.clone()));
        Ok(())
    }

    fn get_cursor(&self, path: &str) -> Result<Option<CursorValue>, ImportError> {
        Ok(self.cursors.get(path).cloned())
    }

    fn set_cursor(&mut self, path: &str, cursor: &CursorValue) -> Result<(), ImportError> {
        drop(self.cursors.insert(path.to_string(), cursor.clone()));
        if self.reservations.get(path) == Some(cursor) {
            drop(self.reservations.remove(path));
        }
        Ok(())
    }

    fn get_generation(&self, path: &str) -> Result<u32, ImportError> {
        Ok(self.generations.get(path).copied().unwrap_or(0))
    }

    fn set_generation(&mut self, path: &str, generation: u32) -> Result<(), ImportError> {
        let _: Option<u32> = self.generations.insert(path.to_string(), generation);
        Ok(())
    }
}

/// A filesystem-backed, content-addressed blob sink.
///
/// Blobs are stored under a directory as one file per unique BLAKE3 hash
/// (`<dir>/<hex-hash>`), deduplicated by content: storing identical bytes
/// twice writes only one file. Writes are atomic (temp file + rename) so a
/// crash never leaves a truncated blob readable under its final name, and the
/// directory survives process restarts, giving durable storage for payloads
/// that spill past [`INLINE_LIMIT`].
#[derive(Debug, Clone)]
pub struct FsBlobSink {
    /// Directory holding the blob files.
    dir: PathBuf,
}

impl FsBlobSink {
    /// Open (creating if needed) a blob directory.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the directory cannot be created.
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Open an existing blob directory for reading without creating it.
    ///
    /// Returns `Ok(None)` when no blob directory exists yet (the common case
    /// for chains that predate durable blobs), so read paths never mutate
    /// storage.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the path exists but cannot be read.
    pub fn open_read_only(dir: impl Into<PathBuf>) -> io::Result<Option<Self>> {
        let dir = dir.into();
        match fs::metadata(&dir) {
            Ok(meta) if meta.is_dir() => Ok(Some(Self { dir })),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("blob path is not a directory: {}", dir.display()),
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Directory containing the blob files.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path a blob with the given hash is stored at.
    #[must_use]
    pub fn path_for(&self, hash: &[u8; 32]) -> PathBuf {
        self.dir.join(hex_encode(hash))
    }

    /// Read a blob back by its BLAKE3 hash.
    ///
    /// Returns `Ok(None)` when no blob with that hash has been stored.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the blob file exists but cannot be read.
    pub fn get(&self, hash: &[u8; 32]) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path_for(hash)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Number of distinct blobs stored.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the blob directory cannot be read.
    pub fn len(&self) -> io::Result<usize> {
        fs::read_dir(&self.dir)?.try_fold(0usize, |count, entry| {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                Ok(count.saturating_add(1))
            } else {
                Ok(count)
            }
        })
    }

    /// Whether no blobs are stored.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the blob directory cannot be read.
    pub fn is_empty(&self) -> io::Result<bool> {
        self.len().map(|len| len == 0)
    }
}

impl BlobSink for FsBlobSink {
    fn store_blob(&mut self, data: &[u8]) -> Result<(), ImportError> {
        let hash = hash_raw(data);
        let path = self.path_for(&hash);
        match fs::File::open(&path) {
            Ok(mut file) => {
                if !same_blob_bytes(&mut file, data)? {
                    return Err(ImportError::BlobSink(format!(
                        "existing blob {} does not match its content address",
                        path.display()
                    )));
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ImportError::BlobSink(format!(
                    "opening blob {}: {error}",
                    path.display()
                )))
            }
        }
        atomic_write(&path, data)
            .map_err(|e| ImportError::BlobSink(format!("storing blob {}: {e}", path.display())))
    }
}

fn same_blob_bytes(file: &mut fs::File, expected: &[u8]) -> io::Result<bool> {
    if file.metadata()?.len() != u64::try_from(expected.len()).map_err(io::Error::other)? {
        return Ok(false);
    }
    let mut buffer = [0; 8192];
    for chunk in expected.chunks(buffer.len()) {
        let target = buffer
            .get_mut(..chunk.len())
            .ok_or_else(|| io::Error::other("blob comparison range"))?;
        file.read_exact(target)?;
        if target != chunk {
            return Ok(false);
        }
    }
    Ok(file.read(&mut buffer)? == 0)
}

/// A filesystem-backed cursor store persisting one JSON file per source key.
///
/// A source-prefix reservation is made before operation append. If that append
/// is interrupted and the source changes again, the next capture uses another
/// generation instead of colliding with already written physical record IDs.
/// Reservations constrain ID reuse without advancing accepted cursors.
///
/// Source keys are keyed by their BLAKE3 hash so filenames stay bounded and
/// free of path separators (`<dir>/<hex-hash>.json`). [`Self::set_cursor`]
/// mutations are staged in memory; only [`CursorStore::commit`] writes them to
/// disk (atomic temp file + rename). This lets the import command advance
/// cursors only after the operations they cover are durably appended. The
/// directory survives process restarts, so repeated imports of unchanged
/// source files skip already-imported content.
///
/// The per-source boot generation counter (see [`CursorStore::get_generation`])
/// is persisted in a single `generations.json` map in the same directory, with
/// the same stage-then-commit discipline as cursors. [`CursorStore::commit`]
/// persists a generation bump before any cursor file that depends on it, so a
/// crash or write error between the two never leaves a durable cursor whose
/// generation is not yet durable (a reopened store would otherwise continue
/// the wrong boot stream). It is retained even when an individual cursor file
/// is deleted, so a reset re-import of a rewritten source reuses its current
/// generation's op ids instead of falling back into the original boot-0 id
/// space. Each current cursor also retains its accepted generation beside the
/// prefix hash. The journal binds a completed rewrite to its exact accepted
/// source bytes even when another rewrite arrives before recovery. Callers must
/// serialize access with the chain writer lock throughout capture and commit.
#[derive(Debug, Clone)]
pub struct FsCursorStore {
    /// Directory holding the cursor files.
    dir: PathBuf,
    /// Cursor mutations staged since the last commit; not yet durable.
    staged: std::collections::HashMap<String, CursorValue>,
    /// Durable per-source generation counters (`generations.json`).
    generations: std::collections::HashMap<String, u32>,
    /// Generation bumps staged since the last commit; not yet durable.
    staged_generations: std::collections::HashMap<String, u32>,
    /// A durable intent still needs materialization or journal cleanup.
    journal_pending: bool,
}

impl FsCursorStore {
    /// Open (creating if needed) a cursor directory.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the directory cannot be created or the persisted
    /// generation map cannot be read, or interrupted checkpoint recovery fails.
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let generations = read_generations(&dir.join("generations.json"))?;
        let mut store = Self {
            dir,
            staged: std::collections::HashMap::new(),
            generations,
            staged_generations: std::collections::HashMap::new(),
            journal_pending: false,
        };
        match fs::read(store.journal_path()) {
            Ok(bytes) => {
                let journal: CheckpointJournal =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if journal.version != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unsupported checkpoint journal version",
                    ));
                }
                store.staged = journal.cursors;
                store.staged_generations = journal.generations;
                store.journal_pending = true;
                store.finish_commit().map_err(io::Error::other)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(store)
    }

    /// Directory containing the cursor files.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path the cursor for `source_key` is stored at.
    #[must_use]
    pub fn cursor_path(&self, source_path: &str) -> PathBuf {
        let key = hex_encode(&hash_raw(source_path.as_bytes()));
        self.dir.join(format!("{key}.json"))
    }

    /// Whether any cursor mutations are staged and not yet committed.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.journal_pending || !self.staged.is_empty() || !self.staged_generations.is_empty()
    }
}

impl CursorStore for FsCursorStore {
    fn get_reservation(&self, key: &str) -> Result<Option<CursorValue>, ImportError> {
        let path = self.cursor_path(key).with_extension("reservation.json");
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                ImportError::CursorStore(format!("decoding source reservation: {error}"))
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ImportError::CursorStore(format!(
                "reading source reservation: {error}"
            ))),
        }
    }

    fn reserve_checkpoint(&mut self, key: &str, cursor: &CursorValue) -> Result<(), ImportError> {
        let json = serde_json::to_vec(cursor).map_err(|error| {
            ImportError::CursorStore(format!("encoding source reservation: {error}"))
        })?;
        atomic_write(
            &self.cursor_path(key).with_extension("reservation.json"),
            &json,
        )
        .map_err(|error| ImportError::CursorStore(format!("writing source reservation: {error}")))
    }

    fn get_cursor(&self, path: &str) -> Result<Option<CursorValue>, ImportError> {
        // Read-your-writes: a staged mutation shadows the durable value.
        if let Some(cursor) = self.staged.get(path) {
            return Ok(Some(cursor.clone()));
        }
        let file = self.cursor_path(path);
        match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| ImportError::CursorStore(format!("decoding {}: {e}", file.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ImportError::CursorStore(format!(
                "reading {}: {e}",
                file.display()
            ))),
        }
    }

    fn set_cursor(&mut self, path: &str, cursor: &CursorValue) -> Result<(), ImportError> {
        // Buffer in memory; nothing reaches disk until `commit()` runs after
        // the operations this cursor covers have been durably appended.
        drop(self.staged.insert(path.to_string(), cursor.clone()));
        Ok(())
    }

    fn get_generation(&self, path: &str) -> Result<u32, ImportError> {
        // Read-your-writes: a staged bump shadows the durable value.
        if let Some(generation) = self.staged_generations.get(path) {
            return Ok(*generation);
        }
        Ok(self.generations.get(path).copied().unwrap_or(0))
    }

    fn set_generation(&mut self, path: &str, generation: u32) -> Result<(), ImportError> {
        if self.get_generation(path)? == generation {
            return Ok(());
        }
        // Buffer in memory; committed (before the staged cursors) by
        // `commit()`.
        let _: Option<u32> = self.staged_generations.insert(path.to_string(), generation);
        Ok(())
    }

    fn commit(&mut self) -> Result<(), ImportError> {
        if !self.has_pending() {
            return Ok(());
        }
        // This intent is written only after operations are durable. Recovery
        // can finish exactly this paired checkpoint before reading a source
        // that may have changed again since the failed commit.
        let journal = CheckpointJournal {
            version: 1,
            cursors: self.staged.clone(),
            generations: self.staged_generations.clone(),
        };
        let json = serde_json::to_vec(&journal).map_err(|error| {
            ImportError::CursorStore(format!("encoding checkpoint journal: {error}"))
        })?;
        atomic_write(&self.journal_path(), &json).map_err(|error| {
            ImportError::CursorStore(format!("writing checkpoint journal: {error}"))
        })?;
        self.journal_pending = true;
        self.finish_commit()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointJournal {
    version: u32,
    cursors: std::collections::HashMap<String, CursorValue>,
    generations: std::collections::HashMap<String, u32>,
}

impl FsCursorStore {
    fn journal_path(&self) -> PathBuf {
        self.dir.join("checkpoint.pending.json")
    }

    fn finish_commit(&mut self) -> Result<(), ImportError> {
        if !self.staged_generations.is_empty() {
            for (path, generation) in &self.staged_generations {
                let _: Option<u32> = self.generations.insert(path.clone(), *generation);
            }
            let file = self.dir.join("generations.json");
            let json = serde_json::to_vec(&self.generations)
                .map_err(|e| ImportError::CursorStore(format!("encoding generations: {e}")))?;
            atomic_write(&file, &json).map_err(|e| {
                ImportError::CursorStore(format!("writing {}: {e}", file.display()))
            })?;
            // The bump is durable; drop the staged copy so a retry does not
            // rewrite it. In-memory reads stay coherent through `generations`.
            self.staged_generations.clear();
        }
        let pending = self.staged.clone();
        for (path, cursor) in pending {
            let file = self.cursor_path(&path);
            let json = serde_json::to_vec(&cursor)
                .map_err(|e| ImportError::CursorStore(format!("encoding cursor: {e}")))?;
            atomic_write(&file, &json).map_err(|e| {
                ImportError::CursorStore(format!("writing {}: {e}", file.display()))
            })?;
            if self.get_reservation(&path)?.as_ref() == Some(&cursor) {
                let reservation = self.cursor_path(&path).with_extension("reservation.json");
                fs::remove_file(&reservation).map_err(|error| {
                    ImportError::CursorStore(format!("removing source reservation: {error}"))
                })?;
                sync_parent_dir(&reservation).map_err(|error| {
                    ImportError::CursorStore(format!("syncing source reservation removal: {error}"))
                })?;
            }
            // Only remove after the durable write succeeded, so a retry after
            // a partial failure still commits the remaining entries.
            drop(self.staged.remove(&path));
        }
        let journal = self.journal_path();
        match fs::remove_file(&journal) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ImportError::CursorStore(format!(
                    "removing checkpoint journal: {error}"
                )))
            }
        }
        sync_parent_dir(&journal).map_err(|error| {
            ImportError::CursorStore(format!("syncing checkpoint journal removal: {error}"))
        })?;
        self.journal_pending = false;
        Ok(())
    }
}

/// Read the durable per-source generation map, treating a missing file as an
/// empty map (the common case for chains that predate generation tracking).
///
/// # Errors
///
/// Returns an IO error if the file exists but cannot be read or decoded.
fn read_generations(path: &Path) -> io::Result<std::collections::HashMap<String, u32>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("decoding {}: {e}", path.display()),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(std::collections::HashMap::new()),
        Err(e) => Err(e),
    }
}

/// Hex-encode bytes (lowercase) for use in storage filenames.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "capacity doubles a bounded byte length; nibble indices are masked to 0..15"
)]
#[must_use]
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Atomically write `data` to `path` via a same-directory temp file + rename.
///
/// The rename makes the final name appear only with complete contents; a crash
/// mid-write leaves at worst a stale temp file. After the rename the parent
/// directory is synced, so the new directory entry is durable before this
/// returns — otherwise a crash could lose the rename even though the file
/// bytes themselves were synced.
///
/// # Errors
///
/// Returns an IO error if the temp file cannot be written or renamed, or the
/// parent directory cannot be synced.
fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_parent_dir(path)
}

/// Fsync `path`'s parent directory so a rename/create inside it survives a
/// crash (directory entries are metadata and are not covered by the file's own
/// `sync_all`).
///
/// On Unix the directory is opened read-only and fsynced. On Windows opening a
/// directory requires `FILE_FLAG_BACKUP_SEMANTICS`. On other platforms
/// directory fsync is not available portably and the call degrades to a no-op
/// (best-effort durability).
///
/// # Errors
///
/// Returns an IO error if the parent directory cannot be opened or synced.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot sync parent of {}", path.display()),
        )
    })?;
    fs::File::open(parent)?.sync_all()
}

/// Windows variant of [`sync_parent_dir`]: directories open with
/// `FILE_FLAG_BACKUP_SEMANTICS` (0x02000000) and can then be fsynced.
#[cfg(windows)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot sync parent of {}", path.display()),
        )
    })?;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x0200_0000)
        .open(parent)?
        .sync_all()
}

/// Fallback for platforms without directory fsync: best-effort no-op.
#[cfg(not(any(unix, windows)))]
fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile as _;

    fn captured_record(bytes: &[u8]) -> Op {
        Op {
            id: editchain_core::OpId::new(NodeId(1), 0, 1),
            parents: editchain_core::ParentSet::None,
            actor: editchain_core::ActorId(0),
            clock: editchain_core::Clock::None,
            scope: editchain_core::ScopeRef::None,
            tags: editchain_core::Tags::IMPORT,
            kind: OpKind::Import(editchain_core::ImportOp {
                raw_ref: payload::Payload::Inline(bytes.to_vec()),
                raw_hash: Some(hash_raw(bytes)),
            }),
        }
    }

    #[test]
    fn bounded_capture_reports_duplicates_and_retains_conflicts_without_extra_capacity() {
        let first = captured_record(b"first");
        let second = captured_record(b"second");
        let bytes =
            u64::try_from(encode_op(&first).unwrap().len() + encode_op(&second).unwrap().len())
                .unwrap();
        let mut sink = MemoryOpSink::with_limits(BatchLimits {
            operations: 2,
            encoded_bytes: bytes,
        });
        let mut report = crate::model::ImportReport::default();
        emit_op(&first, &mut sink, &mut report, EmissionKind::Raw).unwrap();
        emit_op(&first, &mut sink, &mut report, EmissionKind::Raw).unwrap();
        emit_op(&second, &mut sink, &mut report, EmissionKind::Raw).unwrap();
        emit_op(&first, &mut sink, &mut report, EmissionKind::Raw).unwrap();
        assert_eq!(report.raw_ops, 2);
        assert_eq!(report.duplicates, 2);
        assert_eq!(report.op_conflicts, 1);
        assert_eq!(sink.ops, [first, second.clone()]);
        assert!(
            !sink.evidence.contains(&second.id),
            "both conflicting variants remain inert"
        );
        let mut extra = second.clone();
        extra.id.seq = 2;
        assert!(emit_op(&extra, &mut sink, &mut report, EmissionKind::Raw).is_err());
        assert_eq!(report.raw_ops, 2, "failed admission does not change counts");
        assert_eq!(
            sink.accept_op(&second).unwrap(),
            Admission::Duplicate,
            "exact replay still works at the bound"
        );
        assert_eq!(
            sink.evidence
                .classify(extra.id, &encode_op(&extra).unwrap()),
            Admission::Accepted,
            "a limit failure cannot poison later admission"
        );
    }

    #[test]
    fn encoded_byte_limit_rejects_before_retaining_or_quarantining_an_id() {
        let larger = captured_record(b"payload that cannot fit");
        let smaller = captured_record(b"x");
        let encoded = encode_op(&larger).unwrap();
        assert_eq!(encoded_op_len(&larger).unwrap(), encoded.len());
        let mut sink = MemoryOpSink::with_limits(BatchLimits {
            operations: 10,
            encoded_bytes: u64::try_from(encoded.len() - 1).unwrap(),
        });
        assert!(sink.accept_op(&larger).is_err());
        assert!(sink.ops.is_empty());
        assert!(sink.evidence.is_empty());
        assert_eq!(sink.accept_op(&smaller).unwrap(), Admission::Accepted);
    }

    #[test]
    fn existing_blob_bytes_must_match_before_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = FsBlobSink::new(dir.path()).unwrap();
        let data = vec![b'a'; 8193];
        let reference = sink.put(&data).unwrap();
        assert_eq!(reference.len, 8193);
        assert_eq!(reference.id, ContentId::Hash256(hash_raw(&data)));
        assert_eq!(sink.put(&data).unwrap(), reference);
        let path = sink.path_for(&hash_raw(&data));
        let mut same_length = data.clone();
        *same_length.last_mut().unwrap() = b'b';
        let mut truncated = data.clone();
        let _: Option<u8> = truncated.pop();
        let mut extended = data.clone();
        extended.push(b'b');
        for invalid in [same_length, truncated, extended] {
            fs::write(&path, &invalid).unwrap();
            assert!(matches!(sink.put(&data), Err(ImportError::BlobSink(_))));
            assert_eq!(
                fs::read(&path).unwrap(),
                invalid,
                "verification preserves the inconsistent evidence"
            );
        }
        fs::remove_file(&path).unwrap();
        assert_eq!(sink.put(&data).unwrap(), reference);
        assert_eq!(fs::read(&path).unwrap(), data);
    }

    #[test]
    fn legacy_cursor_json_defaults_normalization_version_to_zero() {
        let cursor: CursorValue = serde_json::from_str(
            r#"{"file_size":42,"byte_offset":40,"ops_emitted":7,"content_hash":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#,
        )
        .unwrap();
        assert_eq!(cursor.normalization_version, 0);
        assert_eq!(cursor.content_hash_version, 0);
        assert_eq!(cursor.source_node, None);
        assert_eq!(cursor.session_title_hash, None);
    }

    #[test]
    fn fs_blob_sink_roundtrips_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut blobs = FsBlobSink::new(dir.path().join("chain/blobs")).unwrap();
        assert!(blobs.is_empty().unwrap());

        let data = vec![b'z'; 8192];
        blobs.store_blob(&data).unwrap();
        blobs.store_blob(&data).unwrap(); // dedup: second store is a no-op.

        assert_eq!(blobs.len().unwrap(), 1);
        let hash = hash_raw(&data);
        assert!(blobs.path_for(&hash).is_file());
        assert_eq!(blobs.get(&hash).unwrap().unwrap(), data);
        // Unknown hashes read back as absent, not error.
        assert!(blobs.get(&[0u8; 32]).unwrap().is_none());
    }

    #[test]
    fn fs_blob_sink_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = dir.path().join("chain/blobs");
        let data = vec![b'a'; 5000];
        let hash = hash_raw(&data);

        {
            let mut blobs = FsBlobSink::new(&blob_dir).unwrap();
            blobs.store_blob(&data).unwrap();
        }

        // A fresh sink over the same directory sees the blob (process restart).
        let reopened = FsBlobSink::new(&blob_dir).unwrap();
        assert_eq!(reopened.len().unwrap(), 1);
        assert_eq!(reopened.get(&hash).unwrap().unwrap(), data);
    }

    #[test]
    fn fs_cursor_store_commit_persists_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let cursor_dir = dir.path().join("chain/cursors");
        let cursor = CursorValue {
            accepted_generation: None,
            file_size: 42,
            byte_offset: 40,
            ops_emitted: 7,
            content_hash: [7u8; 32],
            content_hash_version: 1,
            source_node: Some(NodeId(9)),
            normalization_version: 0,
            materialization: None,
            session_title_hash: None,
        };

        {
            let mut store = FsCursorStore::new(&cursor_dir).unwrap();
            assert!(store
                .get_cursor("/workspace/rollout-1.jsonl")
                .unwrap()
                .is_none());
            store
                .set_cursor("/workspace/rollout-1.jsonl", &cursor)
                .unwrap();
            // Staged writes are visible to the same instance before commit.
            assert_eq!(
                store
                    .get_cursor("/workspace/rollout-1.jsonl")
                    .unwrap()
                    .unwrap(),
                cursor
            );
            assert!(store.has_pending());
            store.commit().unwrap();
            assert!(!store.has_pending());
        }

        // A fresh store over the same directory restores the cursor.
        let reopened = FsCursorStore::new(&cursor_dir).unwrap();
        let restored = reopened
            .get_cursor("/workspace/rollout-1.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(restored, cursor);
        // Unrelated source paths stay absent.
        assert!(reopened
            .get_cursor("/other/rollout.jsonl")
            .unwrap()
            .is_none());
        // Cursor keys are hashed, so path separators never leak into filenames.
        assert_eq!(
            reopened.cursor_path("/workspace/rollout-1.jsonl"),
            cursor_dir.join(format!(
                "{}.json",
                hex_encode(&hash_raw(b"/workspace/rollout-1.jsonl"))
            ))
        );
    }

    #[test]
    fn fs_cursor_store_uncommitted_writes_disappear_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let cursor_dir = dir.path().join("chain/cursors");
        let cursor = CursorValue {
            accepted_generation: None,
            file_size: 42,
            byte_offset: 40,
            ops_emitted: 7,
            content_hash: [7u8; 32],
            content_hash_version: 1,
            source_node: Some(NodeId(9)),
            normalization_version: 0,
            materialization: None,
            session_title_hash: None,
        };

        {
            let mut store = FsCursorStore::new(&cursor_dir).unwrap();
            store
                .set_cursor("/workspace/rollout-1.jsonl", &cursor)
                .unwrap();
            // Staged only: a fresh instance over the same directory must not
            // observe the mutation before commit.
            let fresh = FsCursorStore::new(&cursor_dir).unwrap();
            assert!(fresh
                .get_cursor("/workspace/rollout-1.jsonl")
                .unwrap()
                .is_none());
            // Dropped without commit: the staged cursor never reaches disk.
        }

        let reopened = FsCursorStore::new(&cursor_dir).unwrap();
        assert!(reopened
            .get_cursor("/workspace/rollout-1.jsonl")
            .unwrap()
            .is_none());
    }

    #[test]
    fn memory_cursor_store_tracks_generations() {
        let mut store = MemoryCursorStore::new();
        assert_eq!(
            store.get_generation("/workspace/rollout-1.jsonl").unwrap(),
            0
        );
        store
            .set_generation("/workspace/rollout-1.jsonl", 2)
            .unwrap();
        assert_eq!(
            store.get_generation("/workspace/rollout-1.jsonl").unwrap(),
            2
        );
        // Unrelated paths stay at generation 0.
        assert_eq!(store.get_generation("/other.jsonl").unwrap(), 0);
    }

    #[test]
    fn fs_cursor_store_generations_persist_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let cursor_dir = dir.path().join("chain/cursors");

        {
            let mut store = FsCursorStore::new(&cursor_dir).unwrap();
            assert_eq!(
                store.get_generation("/workspace/rollout-1.jsonl").unwrap(),
                0
            );
            store
                .set_generation("/workspace/rollout-1.jsonl", 3)
                .unwrap();
            // Staged writes are visible to the same instance before commit.
            assert_eq!(
                store.get_generation("/workspace/rollout-1.jsonl").unwrap(),
                3
            );
            assert!(store.has_pending());
            store.commit().unwrap();
            assert!(!store.has_pending());
        }

        // A fresh store over the same directory restores the generation.
        let reopened = FsCursorStore::new(&cursor_dir).unwrap();
        assert_eq!(
            reopened
                .get_generation("/workspace/rollout-1.jsonl")
                .unwrap(),
            3
        );
        assert_eq!(reopened.get_generation("/other.jsonl").unwrap(), 0);
    }

    #[test]
    fn fs_cursor_store_uncommitted_generations_disappear_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let cursor_dir = dir.path().join("chain/cursors");

        {
            let mut store = FsCursorStore::new(&cursor_dir).unwrap();
            store
                .set_generation("/workspace/rollout-1.jsonl", 1)
                .unwrap();
            // Staged only: a fresh instance over the same directory must not
            // observe the mutation before commit.
            let fresh = FsCursorStore::new(&cursor_dir).unwrap();
            assert_eq!(
                fresh.get_generation("/workspace/rollout-1.jsonl").unwrap(),
                0
            );
            // Dropped without commit: the staged bump never reaches disk.
        }

        let reopened = FsCursorStore::new(&cursor_dir).unwrap();
        assert_eq!(
            reopened
                .get_generation("/workspace/rollout-1.jsonl")
                .unwrap(),
            0
        );
    }

    #[test]
    fn atomic_write_syncs_parent_directory_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        atomic_write(&path, b"payload").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"payload");
        // The temp file is renamed away; nothing stale remains behind.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty());
        // The directory sync path itself is exercised and succeeds.
        sync_parent_dir(&path).unwrap();
    }
}
