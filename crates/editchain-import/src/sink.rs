use editchain_core::{Op, BlobRef, ContentId};
use editchain_core::payload;

use crate::error::ImportError;
use crate::ids::hash_raw;

/// A sink for accepting encoded operations.
pub trait OpSink {
    /// Accept a single encoded operation (postcard bytes).
    fn accept_op(&mut self, op: &Op) -> Result<bool, ImportError>;
}

/// A sink for accepting large blob payloads.
pub trait BlobSink {
    /// Store a blob and return a content identifier.
    fn store_blob(&mut self, data: &[u8]) -> Result<(), ImportError>;

    /// Store a blob and return a `BlobRef` referencing it.
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
pub fn payload_for(bytes: &[u8], blobs: &mut dyn BlobSink) -> Result<payload::Payload, ImportError> {
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
    fn get_cursor(&self, path: &str) -> Result<Option<CursorValue>, ImportError>;
    /// Write the cursor for a source file.
    fn set_cursor(&mut self, path: &str, cursor: &CursorValue) -> Result<(), ImportError>;
}

/// A cursor value representing how far we've read in a source file.
#[derive(Debug, Clone)]
pub struct CursorValue {
    /// File size at last read (for generation detection).
    pub file_size: u64,
    /// Byte offset we've read up to.
    pub byte_offset: u64,
    /// Number of ops emitted from this file.
    pub ops_emitted: u64,
    /// Blake3 hash of all content up to byte_offset (for integrity).
    pub content_hash: [u8; 32],
}

/// A memory-backed op sink for testing.
#[derive(Debug, Default)]
pub struct MemoryOpSink {
    pub ops: Vec<Op>,
}

impl MemoryOpSink {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }
}

impl OpSink for MemoryOpSink {
    fn accept_op(&mut self, op: &Op) -> Result<bool, ImportError> {
        self.ops.push(op.clone());
        Ok(true)
    }
}

/// A memory-backed blob sink for testing.
#[derive(Debug, Default)]
pub struct MemoryBlobSink {
    pub blobs: Vec<Vec<u8>>,
}

impl MemoryBlobSink {
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

/// A memory-backed blob sink that returns content-addressed BlobRefs.
/// Stores blobs keyed by their BLAKE3 hash for deduplication.
#[derive(Debug, Default)]
pub struct ContentAddressedBlobSink {
    blobs: std::collections::HashMap<[u8; 32], Vec<u8>>,
}

impl ContentAddressedBlobSink {
    pub fn new() -> Self {
        Self { blobs: std::collections::HashMap::new() }
    }

    pub fn get(&self, hash: &[u8; 32]) -> Option<&[u8]> {
        self.blobs.get(hash).map(|v| v.as_slice())
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}

impl BlobSink for ContentAddressedBlobSink {
    fn store_blob(&mut self, data: &[u8]) -> Result<(), ImportError> {
        let hash = hash_raw(data);
        self.blobs.entry(hash).or_insert_with(|| data.to_vec());
        Ok(())
    }
}

/// A memory-backed cursor store for testing.
#[derive(Debug, Default)]
pub struct MemoryCursorStore {
    cursors: std::collections::HashMap<String, CursorValue>,
}

impl MemoryCursorStore {
    pub fn new() -> Self {
        Self { cursors: std::collections::HashMap::new() }
    }
}

impl CursorStore for MemoryCursorStore {
    fn get_cursor(&self, path: &str) -> Result<Option<CursorValue>, ImportError> {
        Ok(self.cursors.get(path).cloned())
    }

    fn set_cursor(&mut self, path: &str, cursor: &CursorValue) -> Result<(), ImportError> {
        self.cursors.insert(path.to_string(), cursor.clone());
        Ok(())
    }
}