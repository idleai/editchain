//! Durable streaming writes under one exclusive storage transaction.

use std::{io, path::PathBuf};

use editchain_store::{BlobStorage as _, BlobStore, LogStore, SegmentStore};

use crate::{encode_op, invalid_input, validate_length, Admission, BlobRef, Op};

/// An exclusively owned writer for a caller-selected chain.
///
/// Admission history is read once and reused between successful writes. The
/// storage writer lock prevents competing writers from invalidating that state.
/// Errors discard uncertain admission state before the next attempt; dropping
/// this handle releases the lock. Every successful call is already durable.
#[derive(Debug)]
pub struct ChainWriter {
    root: PathBuf,
    log: LogStore<SegmentStore>,
    blobs: Option<BlobStore>,
}

impl ChainWriter {
    pub(crate) fn open(root: PathBuf) -> io::Result<Self> {
        let log = LogStore::new(SegmentStore::open(&root)?);
        Ok(Self {
            root,
            log,
            blobs: None,
        })
    }

    /// Persist one authored operation, retaining duplicate/conflict semantics.
    ///
    /// # Errors
    /// Returns encoding, framing, or persistence errors with a retryable outcome.
    pub fn append(&mut self, operation: &Op) -> io::Result<Admission> {
        validate_length(
            editchain_store::format::encoded_op_len(operation).map_err(invalid_input)?,
        )?;
        self.append_encoded(&encode_op(operation).map_err(invalid_input)?)
    }

    /// Persist exact operation bytes without re-encoding retained evidence.
    ///
    /// # Errors
    /// Returns validation, framing or persistence errors. Retrying the same
    /// bytes re-establishes durability after an uncertain result.
    pub fn append_encoded(&mut self, encoded: &[u8]) -> io::Result<Admission> {
        self.log.append_encoded(encoded)
    }

    /// Persist a batch, sharing durability boundaries where storage supports it.
    ///
    /// Results correspond to input order; later conflicts can quarantine earlier
    /// acceptances. No successful result is returned before the entire batch is
    /// durable. Callers choose bounded batches appropriate for their workload.
    ///
    /// # Errors
    /// Returns validation, framing or persistence errors; retry the exact batch.
    pub fn append_encoded_batch(&mut self, records: &[&[u8]]) -> io::Result<Vec<Admission>> {
        self.log.append_encoded_batch(records)
    }

    /// Persist exact content while retaining this writer's exclusive ownership.
    ///
    /// # Errors
    /// Returns size, content-identity or durable publication errors.
    pub fn store_blob(&mut self, bytes: &[u8]) -> io::Result<BlobRef> {
        self.blob_store()?.put(bytes)
    }

    /// Durably publish a caller-bounded batch of exact content in input order.
    ///
    /// # Errors
    /// Returns validation or persistence errors; a subset may have committed.
    pub fn store_blobs(&mut self, payloads: &[&[u8]]) -> io::Result<Vec<BlobRef>> {
        self.blob_store()?.put_batch(payloads)
    }

    fn blob_store(&mut self) -> io::Result<&mut BlobStore> {
        if self.blobs.is_none() {
            self.blobs = Some(BlobStore::new(self.root.join("blobs"))?);
        }
        self.blobs
            .as_mut()
            .ok_or_else(|| io::Error::other("missing writer blob store"))
    }
}
