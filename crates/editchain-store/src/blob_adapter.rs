//! Content-addressed storage contracts independent of filesystem paths.

use std::io;

use editchain_core::{BlobRef, ContentId};

use crate::{BlobReader, BlobResolution, BlobStore};

/// Verified reads over caller-selected content storage.
///
/// Missing content is an explicit result, independent of operation admission.
/// Implementations must observe later arrivals on subsequent reads, validate
/// full addresses, and preserve the caller's reference. IO errors are distinct
/// from missing or corrupt bytes.
pub trait BlobSource {
    /// Resolve exact bytes by content address without decoding their contents.
    ///
    /// # Errors
    /// Returns backend access errors; absence is [`BlobResolution::Missing`].
    fn read_content(&self, id: ContentId) -> io::Result<BlobResolution>;

    /// Resolve content and validate its recorded length as well as its address.
    ///
    /// # Errors
    /// Returns backend access errors.
    fn read_blob(&self, reference: &BlobRef) -> io::Result<BlobResolution> {
        match self.read_content(reference.id)? {
            BlobResolution::Found(bytes)
                if u32::try_from(bytes.len()).ok() != Some(reference.len) =>
            {
                Ok(BlobResolution::Corrupt)
            }
            resolution @ (BlobResolution::Found(_)
            | BlobResolution::Missing
            | BlobResolution::Corrupt
            | BlobResolution::Unresolvable) => Ok(resolution),
        }
    }
}

/// Durable publication of immutable, content-addressed bytes.
pub trait BlobStorage: BlobSource {
    /// Persist exact bytes, returning their full content address and length.
    ///
    /// Success acknowledges durable contents and publication, including on
    /// duplicates after an uncertain commit. Existing conflicting bytes must
    /// never be overwritten. Records may reference blobs before they arrive.
    ///
    /// # Errors
    /// Returns size, existing-content mismatch or persistence errors. A failed
    /// publication may have committed; callers can retry the same exact bytes.
    fn put(&mut self, bytes: &[u8]) -> io::Result<BlobRef>;

    /// Durably publish an ordered, caller-bounded batch of exact payloads.
    ///
    /// Success covers every returned reference. Failure may have published a
    /// subset; retry the same bytes. The default retains per-payload fencing.
    ///
    /// # Errors
    /// Returns validation or persistence errors with an unknown commit outcome.
    fn put_batch(&mut self, payloads: &[&[u8]]) -> io::Result<Vec<BlobRef>> {
        payloads.iter().map(|bytes| self.put(bytes)).collect()
    }
}

impl BlobSource for BlobStore {
    fn read_content(&self, id: ContentId) -> io::Result<BlobResolution> {
        let ContentId::Hash256(hash) = id else {
            return Ok(BlobResolution::Unresolvable);
        };
        match self.get(&hash)? {
            Some(bytes) if blake3::hash(&bytes).as_bytes() == &hash => {
                Ok(BlobResolution::Found(bytes))
            }
            Some(_) => Ok(BlobResolution::Corrupt),
            None => Ok(BlobResolution::Missing),
        }
    }
}

impl BlobStorage for BlobStore {
    fn put(&mut self, bytes: &[u8]) -> io::Result<BlobRef> {
        let len = u32::try_from(bytes.len())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        self.write(bytes)?;
        Ok(BlobRef {
            id: ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
            len,
        })
    }

    fn put_batch(&mut self, payloads: &[&[u8]]) -> io::Result<Vec<BlobRef>> {
        let references = payloads
            .iter()
            .map(|bytes| {
                Ok(BlobRef {
                    id: ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
                    len: u32::try_from(bytes.len())
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        self.write_batch(payloads)?;
        Ok(references)
    }
}

impl BlobSource for BlobReader {
    fn read_content(&self, id: ContentId) -> io::Result<BlobResolution> {
        self.store().read_content(id)
    }
}
