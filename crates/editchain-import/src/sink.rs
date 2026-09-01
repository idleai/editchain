use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use editchain_core::payload;
use editchain_core::{BlobRef, ContentId, Op};

use crate::error::ImportError;
use crate::ids::hash_raw;

/// A sink for accepting encoded operations.
pub trait OpSink {
    /// Accept a single encoded operation (postcard bytes).
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the operation cannot be stored.
    fn accept_op(&mut self, op: &Op) -> Result<bool, ImportError>;

    /// Downcast this sink to a concrete type for post-import mutation.
    ///
    /// Used by the import orchestrator to run subagent linking over the ops a
    /// [`MemoryOpSink`] has collected. Returns `None` for sinks that do not
    /// expose their op vec (linking is then skipped).
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
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
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "data.len() fits in u32 for practical blob sizes"
    )]
    fn put(&mut self, data: &[u8]) -> Result<BlobRef, ImportError> {
        let hash = hash_raw(data);
        let id = ContentId::Hash256(hash);
        self.store_blob(data)?;
        Ok(BlobRef {
            id,
            len: data.len() as u32,
        })
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

/// A store for persisting per-file read cursors.
pub trait CursorStore {
    /// Read the cursor for a source file.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the cursor cannot be read.
    fn get_cursor(&self, path: &str) -> Result<Option<CursorValue>, ImportError>;
    /// Write the cursor for a source file.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the cursor cannot be written.
    fn set_cursor(&mut self, path: &str, cursor: &CursorValue) -> Result<(), ImportError>;

    /// Read the persisted boot generation for a source file.
    ///
    /// The generation counter is bumped whenever an import detects that a
    /// source file was rewritten (truncated) since the last read; it selects
    /// the deterministic boot epoch for the file's op ids. Stores that do not
    /// track generations return `0` (the original generation), which keeps
    /// the Claude importer's boot behavior unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] if the generation cannot be read.
    fn get_generation(&self, _path: &str) -> Result<u32, ImportError> {
        Ok(0)
    }
    /// Persist the boot generation for a source file.
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

/// A cursor value representing how far we've read in a source file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CursorValue {
    /// File size at last read (for generation detection).
    pub file_size: u64,
    /// Byte offset we've read up to.
    pub byte_offset: u64,
    /// Number of ops emitted from this file.
    pub ops_emitted: u64,
    /// Blake3 hash of all content up to `byte_offset` (for integrity).
    pub content_hash: [u8; 32],
    /// Importer-owned normalized projection version applied to this source.
    /// Older cursor JSON omits this field and therefore upgrades from zero.
    #[serde(default)]
    pub normalization_version: u32,
}

/// A memory-backed op sink for testing.
#[derive(Debug, Default)]
pub struct MemoryOpSink {
    /// Stored operations.
    pub ops: Vec<Op>,
}

impl MemoryOpSink {
    /// Create a new empty memory op sink.
    #[must_use]
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }
}

impl OpSink for MemoryOpSink {
    fn accept_op(&mut self, op: &Op) -> Result<bool, ImportError> {
        self.ops.push(op.clone());
        Ok(true)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
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
}

impl MemoryCursorStore {
    /// Create a new empty memory cursor store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cursors: std::collections::HashMap::new(),
            generations: std::collections::HashMap::new(),
        }
    }
}

impl CursorStore for MemoryCursorStore {
    fn get_cursor(&self, path: &str) -> Result<Option<CursorValue>, ImportError> {
        Ok(self.cursors.get(path).cloned())
    }

    fn set_cursor(&mut self, path: &str, cursor: &CursorValue) -> Result<(), ImportError> {
        let _: Option<CursorValue> = self.cursors.insert(path.to_string(), cursor.clone());
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
        if path.exists() {
            // Content-addressed dedup: identical bytes are stored once.
            return Ok(());
        }
        atomic_write(&path, data)
            .map_err(|e| ImportError::BlobSink(format!("storing blob {}: {e}", path.display())))
    }
}

/// A filesystem-backed cursor store persisting one JSON file per source path.
///
/// Source paths are keyed by their BLAKE3 hash so filenames stay bounded and
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
/// space.
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
}

impl FsCursorStore {
    /// Open (creating if needed) a cursor directory.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the directory cannot be created or the persisted
    /// generation map cannot be read.
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let generations = read_generations(&dir.join("generations.json"))?;
        Ok(Self {
            dir,
            staged: std::collections::HashMap::new(),
            generations,
            staged_generations: std::collections::HashMap::new(),
        })
    }

    /// Directory containing the cursor files.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path the cursor for `source_path` is stored at.
    #[must_use]
    pub fn cursor_path(&self, source_path: &str) -> PathBuf {
        let key = hex_encode(&hash_raw(source_path.as_bytes()));
        self.dir.join(format!("{key}.json"))
    }

    /// Whether any cursor mutations are staged and not yet committed.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.staged.is_empty() || !self.staged_generations.is_empty()
    }
}

impl CursorStore for FsCursorStore {
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
        let _: Option<CursorValue> = self.staged.insert(path.to_string(), cursor.clone());
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
        // Buffer in memory; committed (before the staged cursors) by
        // `commit()`.
        let _: Option<u32> = self.staged_generations.insert(path.to_string(), generation);
        Ok(())
    }

    fn commit(&mut self) -> Result<(), ImportError> {
        // Durability ordering: make any generation bump durable BEFORE the
        // cursor files that depend on it. A failure in the between-writes
        // window must never leave a durable cursor whose generation is not
        // yet recorded — reopening would then continue the wrong boot stream.
        // Writing cursors first (the old order) could strand exactly that state.
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
            // Only remove after the durable write succeeded, so a retry after
            // a partial failure still commits the remaining entries.
            let _: Option<CursorValue> = self.staged.remove(&path);
        }
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

    #[test]
    fn legacy_cursor_json_defaults_normalization_version_to_zero() {
        let cursor: CursorValue = serde_json::from_str(
            r#"{"file_size":42,"byte_offset":40,"ops_emitted":7,"content_hash":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#,
        )
        .unwrap();
        assert_eq!(cursor.normalization_version, 0);
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
            file_size: 42,
            byte_offset: 40,
            ops_emitted: 7,
            content_hash: [7u8; 32],
            normalization_version: 0,
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
            file_size: 42,
            byte_offset: 40,
            ops_emitted: 7,
            content_hash: [7u8; 32],
            normalization_version: 0,
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
