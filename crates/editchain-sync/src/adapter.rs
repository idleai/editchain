//! Replication over portable append-log and blob adapters.

use std::collections::BTreeSet;
use std::io;

use editchain_core::{BlobRef, ContentId, Payload};
use editchain_store::format::decode_op;
use editchain_store::{AppendLog, BlobResolution, BlobStorage};

use crate::{content, invalid, RecordKey, ReplicationStorage, Snapshot, MAX_OBJECT_BYTES};

/// Caller-owned export authorization for one peer and chain.
///
/// Record receipt grants no export rights. Authorize blobs independently: a
/// peer-provided record containing a guessed hash must not reveal private local
/// content. Authentication, peer discovery and incoming write grants belong to
/// the caller. Implementations must freeze decisions for a session and make
/// [`Self::check`] fail on revocation or a policy revision.
pub trait ExportPolicy {
    /// Opaque chain/sharing namespace; both endpoints must agree on it.
    fn namespace(&self) -> &str;

    /// Verify this policy's authorization and revision are still current.
    ///
    /// # Errors
    /// Returns revocation, revision-change or authorization-backend errors.
    fn check(&self) -> io::Result<()>;

    /// Whether these exact operation bytes may be offered to this peer.
    ///
    /// # Errors
    /// Returns authorization-backend errors.
    fn share_record(&self, key: RecordKey, encoded: &[u8]) -> io::Result<bool>;

    /// Whether this content may be sent in response to a record's reference.
    /// The engine also verifies that the record actually references the hash.
    ///
    /// # Errors
    /// Returns authorization-backend errors.
    fn share_blob(&self, key: RecordKey, hash: [u8; 32]) -> io::Result<bool>;
}

/// An immutable caller-selected export scope, independent of workspace state.
#[derive(Debug, Clone)]
pub struct ExportScope {
    namespace: String,
    records: Option<BTreeSet<RecordKey>>,
    blobs: Option<BTreeSet<[u8; 32]>>,
}

impl ExportScope {
    /// Explicitly share all retained operations and their referenced blobs,
    /// including future receipts. Use only for a wholly shareable store.
    ///
    /// # Errors
    /// Rejects an empty or oversized negotiation namespace.
    pub fn all(namespace: impl Into<String>) -> io::Result<Self> {
        let namespace = namespace.into();
        if namespace.is_empty() || namespace.len() > 1024 {
            return Err(invalid("invalid replication namespace"));
        }
        Ok(Self {
            namespace,
            records: None,
            blobs: None,
        })
    }

    /// Share only exact record variants and referenced content selected here.
    /// New receipts cannot expand either set. Missing parents remain missing.
    ///
    /// # Errors
    /// Rejects an empty or oversized negotiation namespace.
    pub fn selected(
        namespace: impl Into<String>,
        records: BTreeSet<RecordKey>,
        blobs: BTreeSet<[u8; 32]>,
    ) -> io::Result<Self> {
        let mut scope = Self::all(namespace)?;
        scope.records = Some(records);
        scope.blobs = Some(blobs);
        Ok(scope)
    }
}

impl ExportPolicy for ExportScope {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn check(&self) -> io::Result<()> {
        Ok(())
    }

    fn share_record(&self, key: RecordKey, _encoded: &[u8]) -> io::Result<bool> {
        Ok(self.records.as_ref().is_none_or(|keys| keys.contains(&key)))
    }

    fn share_blob(&self, _key: RecordKey, hash: [u8; 32]) -> io::Result<bool> {
        Ok(self
            .blobs
            .as_ref()
            .is_none_or(|hashes| hashes.contains(&hash)))
    }
}

/// Exact replication using caller-supplied durable storage and export policy.
///
/// The append adapter's exclusive transaction covers this value's lifetime.
/// For hosts that need short transactions alongside capture, implement
/// [`ReplicationStorage`] directly, as [`crate::Replica`] does. No directories,
/// identities, membership ledgers or background tasks are created here.
#[derive(Debug)]
pub struct StoreReplica<L, B, P> {
    log: L,
    blobs: B,
    policy: P,
}

impl<L: AppendLog, B: BlobStorage, P: ExportPolicy> StoreReplica<L, B, P> {
    /// Bind existing storage to the caller's already approved policy.
    pub const fn new(log: L, blobs: B, policy: P) -> Self {
        Self { log, blobs, policy }
    }

    /// Return adapters and policy, for local writes or a later connection.
    pub fn into_parts(self) -> (L, B, P) {
        (self.log, self.blobs, self.policy)
    }

    fn records(&self, exporting: bool) -> io::Result<Snapshot> {
        self.ensure_scope()?;
        self.log.sync()?;
        let mut snapshot = Snapshot::default();
        let _stats = self.log.visit_records(&mut |_flags, bytes| {
            let key = RecordKey::from_encoded(bytes)?;
            if !exporting || self.policy.share_record(key, bytes)? {
                let _key = snapshot.insert_encoded(bytes.to_vec())?;
            }
            Ok(())
        })?;
        Ok(snapshot)
    }

    fn references(
        &self,
        snapshot: &Snapshot,
        key: RecordKey,
        exporting: bool,
    ) -> io::Result<content::References> {
        self.ensure_scope()?;
        let encoded = snapshot
            .record(key)
            .ok_or_else(|| invalid("record outside replication snapshot"))?;
        if exporting && !self.policy.share_record(key, encoded)? {
            return Err(invalid("record outside export scope"));
        }
        let op = decode_op(encoded).map_err(io::Error::other)?;
        let mut references = content::references(&op);
        if let Some(Payload::Blob(reference)) = content::structured_payload(&op) {
            if let ContentId::Hash256(hash) = reference.id {
                if !exporting || self.policy.share_blob(key, hash)? {
                    if let Some(bytes) = self.content(&references, hash)? {
                        content::nested(&mut references, &bytes);
                    }
                }
            }
        }
        Ok(references)
    }

    fn content(
        &self,
        references: &content::References,
        hash: [u8; 32],
    ) -> io::Result<Option<Vec<u8>>> {
        if !references.contains_key(&hash) {
            return Err(invalid("blob is not referenced by this record"));
        }
        match self.blobs.read_content(ContentId::Hash256(hash))? {
            BlobResolution::Found(bytes) => {
                validate_content(references, hash, &bytes)?;
                Ok(Some(bytes))
            }
            BlobResolution::Missing => Ok(None),
            BlobResolution::Corrupt | BlobResolution::Unresolvable => {
                Err(invalid("invalid stored replication blob"))
            }
        }
    }

    fn publish_blob(&mut self, hash: [u8; 32], bytes: &[u8]) -> io::Result<()> {
        let expected = BlobRef {
            id: ContentId::Hash256(hash),
            len: u32::try_from(bytes.len()).map_err(io::Error::other)?,
        };
        if self.blobs.put(bytes)? != expected {
            return Err(invalid(
                "blob adapter returned a different content identity",
            ));
        }
        Ok(())
    }
}

impl<L: AppendLog, B: BlobStorage, P: ExportPolicy> ReplicationStorage for StoreReplica<L, B, P> {
    fn namespace(&self) -> &str {
        self.policy.namespace()
    }

    fn ensure_scope(&self) -> io::Result<()> {
        self.policy.check()
    }

    fn snapshot(&self) -> io::Result<Snapshot> {
        self.records(true)
    }

    fn receiving_snapshot(&self) -> io::Result<Snapshot> {
        self.records(false)
    }

    fn ingest_records(
        &mut self,
        records: &[(RecordKey, Vec<u8>)],
        snapshot: &mut Snapshot,
    ) -> io::Result<()> {
        crate::storage::validate_records(records)?;
        // Re-read after every uncertain write. Never let an optimistic cache
        // skip publication or suppress a conflicting representation.
        let mut known = self.receiving_snapshot()?;
        for (key, bytes) in records {
            if let Some(previous) = known.record(*key) {
                if previous != bytes {
                    return Err(invalid("record digest collision"));
                }
            } else {
                self.log.append_record(0, bytes)?;
                let _key = known.insert_encoded(bytes.clone())?;
            }
        }
        for (_, bytes) in records {
            let _key = snapshot.insert_encoded(bytes.clone())?;
        }
        Ok(())
    }

    fn blob_hashes(&self, snapshot: &Snapshot, key: RecordKey) -> io::Result<BTreeSet<[u8; 32]>> {
        Ok(self.references(snapshot, key, false)?.into_keys().collect())
    }

    fn read_blob(
        &self,
        snapshot: &Snapshot,
        key: RecordKey,
        hash: [u8; 32],
    ) -> io::Result<Option<Vec<u8>>> {
        let references = self.references(snapshot, key, true)?;
        if !self.policy.share_blob(key, hash)? {
            return Ok(None);
        }
        self.content(&references, hash)
    }

    fn ingest_blob(&mut self, key: RecordKey, hash: [u8; 32], bytes: &[u8]) -> io::Result<()> {
        let snapshot = self.receiving_snapshot()?;
        let references = self.references(&snapshot, key, false)?;
        validate_content(&references, hash, bytes)?;
        self.publish_blob(hash, bytes)
    }

    fn confirm_blob(
        &mut self,
        snapshot: &Snapshot,
        key: RecordKey,
        hash: [u8; 32],
    ) -> io::Result<bool> {
        let references = self.references(snapshot, key, false)?;
        let Some(bytes) = self.content(&references, hash)? else {
            return Ok(false);
        };
        self.publish_blob(hash, &bytes)?;
        Ok(true)
    }
}

fn validate_content(
    references: &content::References,
    hash: [u8; 32],
    bytes: &[u8],
) -> io::Result<()> {
    if bytes.len() > MAX_OBJECT_BYTES
        || blake3::hash(bytes).as_bytes() != &hash
        || !content::matches_len(
            references,
            hash,
            u64::try_from(bytes.len()).map_err(io::Error::other)?,
        )
    {
        return Err(invalid("replication blob hash, size or reference mismatch"));
    }
    Ok(())
}
