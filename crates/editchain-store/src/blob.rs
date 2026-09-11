//! Shared content-addressed blob storage and verified read access.

use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use editchain_core::{BlobRef, ContentId};

use crate::durable::atomic_write;

/// A filesystem-backed, content-addressed blob sink.
///
/// Blobs are stored under a directory as one file per unique BLAKE3 hash
/// (`<dir>/<hex-hash>`), deduplicated by content: storing identical bytes
/// twice writes only one file. Writes are atomic (temp file + rename) so a
/// crash never leaves a truncated blob readable under its final name, and the
/// directory survives process restarts, giving durable storage for payloads
/// that callers choose to retain outside an operation envelope.
#[derive(Debug, Clone)]
pub struct BlobStore {
    /// Directory holding the blob files.
    dir: PathBuf,
}

impl BlobStore {
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
        self.dir.join(blake3::Hash::from(*hash).to_hex().as_str())
    }

    /// Read stored bytes by their BLAKE3 address, without verifying their contents.
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

    /// Resolve a blob reference, validating declared length and hash.
    ///
    /// Non-`Found` outcomes leave the caller's [`BlobRef`] untouched so legacy
    /// chains with missing or corrupt blobs still open.
    #[must_use]
    pub fn resolve(&self, blob: &BlobRef) -> BlobResolution {
        let Some(hash) = addressable_hash(blob.id) else {
            return BlobResolution::Unresolvable;
        };
        match self.get(&hash) {
            Ok(Some(bytes)) if blob_matches(&bytes, blob, hash) => BlobResolution::Found(bytes),
            Ok(Some(_)) | Err(_) => BlobResolution::Corrupt,
            Ok(None) => BlobResolution::Missing,
        }
    }

    /// Resolve a full content-addressed payload when only its `ContentId` is
    /// stored (as with `FileOp.base` / `FileOp.after`).
    #[must_use]
    pub fn resolve_content(&self, id: ContentId) -> Option<Vec<u8>> {
        let hash = addressable_hash(id)?;
        let bytes = self.get(&hash).ok().flatten()?;
        (hash_raw(&bytes) == hash).then_some(bytes)
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

impl BlobStore {
    /// Retain complete bytes under their BLAKE3 content address.
    ///
    /// Existing bytes must match exactly before a write can be reused. A
    /// mismatch leaves the existing evidence untouched. New files are synced
    /// and atomically published before this method succeeds.
    ///
    /// # Errors
    ///
    /// Returns an IO error for inconsistent existing bytes or a failed read,
    /// publication, or directory sync.
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let hash = hash_raw(data);
        let path = self.path_for(&hash);
        match File::open(&path) {
            Ok(mut file) => {
                if !same_blob_bytes(&mut file, data)? {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "existing blob {} does not match its content address",
                            path.display()
                        ),
                    ));
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("opening blob {}: {error}", path.display()),
                ))
            }
        }
        atomic_write(&path, data)
            .map_err(|e| io::Error::new(e.kind(), format!("storing blob {}: {e}", path.display())))
    }
}

fn same_blob_bytes(file: &mut File, expected: &[u8]) -> io::Result<bool> {
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
/// Blob files use the shared durable
/// `<chain>/blobs/<lowercase blake3 hex>` format. Full resolution validates
/// declared length and BLAKE3; bounded row previews validate file length and
/// defer full hashing until content is explicitly requested.
#[derive(Debug, Clone)]
pub struct BlobReader {
    /// The durable store directory; `None` when the chain has no `blobs/`
    /// directory.
    store: Option<BlobStore>,
}

impl BlobReader {
    /// Open the blob store for a chain directory without creating it.
    ///
    /// # Errors
    ///
    /// Returns an IO error if `chain_dir/blobs` exists but cannot be read.
    pub fn open(chain_dir: &Path) -> io::Result<Self> {
        BlobStore::open_read_only(chain_dir.join("blobs")).map(|store| Self { store })
    }

    fn path_for(&self, hash: &[u8; 32]) -> Option<PathBuf> {
        self.store.as_ref().map(|store| store.path_for(hash))
    }

    /// Resolve a blob reference, validating declared length and BLAKE3 hash.
    #[must_use]
    pub fn resolve(&self, blob: &BlobRef) -> BlobResolution {
        match &self.store {
            Some(store) => store.resolve(blob),
            None if addressable_hash(blob.id).is_some() => BlobResolution::Missing,
            None => BlobResolution::Unresolvable,
        }
    }

    /// Resolve complete content by its full BLAKE3 identity.
    #[must_use]
    pub fn resolve_content(&self, id: ContentId) -> Option<Vec<u8>> {
        self.store.as_ref()?.resolve_content(id)
    }

    /// Read at most `limit` bytes for a display preview without hydrating or
    /// hashing the complete payload.
    ///
    /// File length is checked against the reference up front. Full BLAKE3
    /// validation remains deferred to [`Self::resolve`] when details/search
    /// actually request the complete payload.
    #[must_use]
    pub fn preview(&self, blob: &BlobRef, limit: usize) -> BlobPreviewResolution {
        let Some(hash) = addressable_hash(blob.id) else {
            return BlobPreviewResolution::Unresolvable;
        };
        let Some(path) = self.path_for(&hash) else {
            return BlobPreviewResolution::Missing;
        };
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
        let mut bytes = Vec::new();
        let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
        if file.take(limit_u64).read_to_end(&mut bytes).is_err() {
            return BlobPreviewResolution::Corrupt;
        }
        BlobPreviewResolution::Found(bytes)
    }
}

/// Outcome of a bounded, length-checked preview read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobPreviewResolution {
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
/// Blob files are keyed by the full 256-bit BLAKE3 hash, so truncated
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

#[must_use]
fn hash_raw(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}
