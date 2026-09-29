//! Bounded blob capture followed by an explicit durability handoff.

use editchain_core::{BlobRef, ContentId};
use editchain_store::{BlobResolution, BlobStorage};

use super::BlobSink;
use crate::ImportError;

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_PAYLOADS: usize = 64;

/// Capture payloads in bounded cohorts before making their references durable.
///
/// Call [`Self::flush`] successfully before persisting operations or accepting
/// source checkpoints. Individual `BlobSink` acknowledgements mean retained
/// capture bytes, as with `MemoryBlobSink`; they do not imply durability.
/// Dropping this sink discards any remaining unflushed capture bytes.
#[derive(Debug)]
pub struct BufferedBlobSink<B> {
    store: B,
    pending: Vec<Vec<u8>>,
    bytes: usize,
}

impl<B: BlobStorage> BufferedBlobSink<B> {
    /// Wrap a durable blob adapter with a 64-payload, 4 MiB capture buffer.
    #[must_use]
    pub const fn new(store: B) -> Self {
        Self {
            store,
            pending: Vec::new(),
            bytes: 0,
        }
    }

    /// Establish durability for every previously accepted payload.
    ///
    /// # Errors
    /// Returns backend errors without discarding pending bytes. Retrying this
    /// call is safe even if the backend published part of the prior attempt.
    pub fn flush(&mut self) -> Result<(), ImportError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let payloads: Vec<_> = self.pending.iter().map(Vec::as_slice).collect();
        let references = self.store.put_batch(&payloads)?;
        if references.len() != payloads.len()
            || references.iter().zip(&payloads).any(|(reference, bytes)| {
                reference.id != ContentId::Hash256(*blake3::hash(bytes).as_bytes())
                    || usize::try_from(reference.len).ok() != Some(bytes.len())
            })
        {
            return Err(ImportError::BlobSink(
                "blob batch returned different content references".into(),
            ));
        }
        self.pending.clear();
        self.bytes = 0;
        Ok(())
    }
}

impl<B: BlobStorage> BlobSink for BufferedBlobSink<B> {
    fn read_blob(&self, reference: &BlobRef) -> Result<Option<Vec<u8>>, ImportError> {
        if let Some(bytes) = self
            .pending
            .iter()
            .find(|bytes| ContentId::Hash256(*blake3::hash(bytes).as_bytes()) == reference.id)
        {
            if u32::try_from(bytes.len()).ok() != Some(reference.len) {
                return Err(ImportError::BlobSink("source blob length mismatch".into()));
            }
            return Ok(Some(bytes.clone()));
        }
        match self.store.read_blob(reference)? {
            BlobResolution::Found(bytes) => Ok(Some(bytes)),
            BlobResolution::Missing | BlobResolution::Unresolvable => Ok(None),
            BlobResolution::Corrupt => Err(ImportError::BlobSink(
                "source blob does not match its reference".into(),
            )),
        }
    }

    fn store_blob(&mut self, data: &[u8]) -> Result<(), ImportError> {
        if self.bytes.saturating_add(data.len()) > MAX_BYTES {
            self.flush()?;
        }
        if data.len() > MAX_BYTES {
            let _reference = self.store.put(data)?;
            return Ok(());
        }
        self.pending.push(data.to_vec());
        self.bytes = self.bytes.saturating_add(data.len());
        if self.pending.len() >= MAX_PAYLOADS {
            self.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, io};

    use editchain_store::BlobSource;

    use super::*;

    #[derive(Debug, Default)]
    struct Blobs {
        values: BTreeMap<[u8; 32], Vec<u8>>,
        batches: usize,
        fail: bool,
    }

    impl BlobSource for Blobs {
        fn read_content(&self, id: ContentId) -> io::Result<BlobResolution> {
            let ContentId::Hash256(hash) = id else {
                return Ok(BlobResolution::Unresolvable);
            };
            Ok(self
                .values
                .get(&hash)
                .map_or(BlobResolution::Missing, |bytes| {
                    BlobResolution::Found(bytes.clone())
                }))
        }
    }

    impl BlobStorage for Blobs {
        fn put(&mut self, bytes: &[u8]) -> io::Result<BlobRef> {
            let hash = *blake3::hash(bytes).as_bytes();
            drop(self.values.insert(hash, bytes.to_vec()));
            Ok(BlobRef {
                id: ContentId::Hash256(hash),
                len: u32::try_from(bytes.len()).map_err(io::Error::other)?,
            })
        }

        fn put_batch(&mut self, payloads: &[&[u8]]) -> io::Result<Vec<BlobRef>> {
            self.batches = self.batches.saturating_add(1);
            let mut references = Vec::new();
            for bytes in payloads {
                references.push(self.put(bytes)?);
                if self.fail {
                    self.fail = false;
                    return Err(io::Error::other("uncertain partial blob batch"));
                }
            }
            Ok(references)
        }
    }

    #[test]
    fn capture_is_bounded_and_flush_is_required_before_durable_handoff() {
        let mut sink = BufferedBlobSink::new(Blobs::default());
        for n in 0..63 {
            sink.store_blob(format!("blob {n}").as_bytes()).unwrap();
        }
        assert!(sink.store.values.is_empty());
        sink.store_blob(b"blob 63").unwrap();
        assert_eq!(sink.store.batches, 1);
        assert_eq!(sink.store.values.len(), 64);
        assert!(sink.pending.is_empty());
        sink.store_blob(&vec![1; 3 * 1024 * 1024]).unwrap();
        sink.store_blob(&vec![2; 2 * 1024 * 1024]).unwrap();
        assert_eq!(
            sink.store.batches, 2,
            "byte limit flushes before accepting the next payload"
        );
        assert_eq!(sink.pending.len(), 1);
        sink.flush().unwrap();
        assert_eq!(sink.store.batches, 3);
        assert!(sink.pending.is_empty());
        sink.store_blob(&vec![3; MAX_BYTES.saturating_add(1)])
            .unwrap();
        assert!(
            sink.pending.is_empty(),
            "oversized single payloads bypass the capture buffer"
        );
        assert_eq!(sink.store.values.len(), 67);
    }

    #[test]
    fn uncertain_blob_flush_retains_pending_capture_bytes_for_exact_retry() {
        let mut sink = BufferedBlobSink::new(Blobs {
            fail: true,
            ..Blobs::default()
        });
        for value in [b"first".as_slice(), b"second", b"third"] {
            sink.store_blob(value).unwrap();
        }
        assert!(sink.flush().is_err());
        assert_eq!(sink.pending.len(), 3);
        assert_eq!(sink.store.values.len(), 1);
        sink.flush().unwrap();
        assert!(sink.pending.is_empty());
        assert_eq!(sink.store.values.len(), 3);
        assert_eq!(sink.store.batches, 2);
    }

    #[test]
    fn conversion_reads_pending_flushed_and_oversized_blobs() {
        let mut sink = BufferedBlobSink::new(Blobs::default());
        let reference = sink.put(b"pending source").unwrap();
        assert_eq!(
            sink.read_blob(&reference).unwrap(),
            Some(b"pending source".to_vec())
        );
        assert!(
            sink.store.values.is_empty(),
            "read does not force durability"
        );
        let mut wrong_length = reference;
        wrong_length.len = 0;
        assert!(sink.read_blob(&wrong_length).is_err());
        sink.flush().unwrap();
        assert_eq!(
            sink.read_blob(&reference).unwrap(),
            Some(b"pending source".to_vec())
        );
        assert!(sink.read_blob(&wrong_length).is_err());
        let oversized = vec![42; MAX_BYTES.saturating_add(1)];
        let reference = sink.put(&oversized).unwrap();
        assert!(sink.pending.is_empty());
        assert_eq!(sink.read_blob(&reference).unwrap(), Some(oversized));
        let absent = BlobRef {
            id: ContentId::Hash256([0; 32]),
            len: 1,
        };
        assert_eq!(sink.read_blob(&absent).unwrap(), None);
    }
}
