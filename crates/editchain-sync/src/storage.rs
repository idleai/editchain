//! Storage boundary shared by native multiplayer and caller-owned adapters.

use std::collections::BTreeSet;
use std::io;

use crate::{invalid, RecordKey, Replica, Snapshot, INVENTORY_PAGE, MAX_OBJECT_BYTES};

/// Storage and export authority for one authorized replication session.
///
/// Implementations retain every exact variant, including conflicts. Snapshots
/// used to skip downloads must contain only durable evidence: readable bytes
/// after an uncertain write are insufficient. Errors never authorize an ack.
/// Policies must remain fixed for a session or fail [`Self::ensure_scope`] when
/// changed, including while cached chunks are being sent.
pub trait ReplicationStorage {
    /// Opaque caller-selected chain/sharing namespace used in negotiation.
    fn namespace(&self) -> &str;

    /// Check that this session's authorization is still valid.
    ///
    /// # Errors
    /// Rejects revocation, policy changes and unavailable authorization state.
    fn ensure_scope(&self) -> io::Result<()>;

    /// Freeze all durably retained variants authorized for export.
    ///
    /// # Errors
    /// Returns policy, read or durability errors.
    fn snapshot(&self) -> io::Result<Snapshot>;

    /// Freeze durable local evidence used to avoid duplicate downloads.
    /// It may include records that cannot be exported.
    ///
    /// # Errors
    /// Returns policy, read or durability errors.
    fn receiving_snapshot(&self) -> io::Result<Snapshot>;

    /// Validate and durably retain a complete batch, then update `snapshot`.
    ///
    /// Use [`Snapshot::insert_encoded`] only after persistence succeeds. A
    /// partial commit may remain on error; reconnect must safely rediscover it.
    /// Duplicates must re-establish durability and conflicts must retain bytes.
    ///
    /// # Errors
    /// Returns invalid-input or persistence errors; no batch ack is then sent.
    fn ingest_records(
        &mut self,
        records: &[(RecordKey, Vec<u8>)],
        snapshot: &mut Snapshot,
    ) -> io::Result<()>;

    /// Enumerate references, including children of available structured blobs.
    ///
    /// # Errors
    /// Returns scope, evidence or content-integrity errors.
    fn blob_hashes(&self, snapshot: &Snapshot, key: RecordKey) -> io::Result<BTreeSet<[u8; 32]>>;

    /// Read verified, explicitly authorized content for an offered record.
    /// Missing or withheld content is `None`.
    ///
    /// # Errors
    /// Returns scope, size, reference-length, hash or backend errors.
    fn read_blob(
        &self,
        snapshot: &Snapshot,
        key: RecordKey,
        hash: [u8; 32],
    ) -> io::Result<Option<Vec<u8>>>;

    /// Verify and durably publish content referenced by retained evidence.
    ///
    /// # Errors
    /// Returns invalid-reference, hash, length or persistence errors.
    fn ingest_blob(&mut self, key: RecordKey, hash: [u8; 32], bytes: &[u8]) -> io::Result<()>;

    /// Confirm existing content is durable before skipping its download.
    /// Readability alone cannot confirm a previous uncertain publication.
    ///
    /// # Errors
    /// Returns verification or persistence errors. `false` means unavailable.
    fn confirm_blob(
        &mut self,
        snapshot: &Snapshot,
        key: RecordKey,
        hash: [u8; 32],
    ) -> io::Result<bool> {
        let Some(bytes) = self.read_blob(snapshot, key, hash)? else {
            return Ok(false);
        };
        self.ingest_blob(key, hash, &bytes)?;
        Ok(true)
    }
}

impl ReplicationStorage for Replica {
    fn namespace(&self) -> &str {
        self.space()
    }

    fn ensure_scope(&self) -> io::Result<()> {
        Self::ensure_scope(self)
    }

    fn snapshot(&self) -> io::Result<Snapshot> {
        Self::snapshot(self)
    }

    fn receiving_snapshot(&self) -> io::Result<Snapshot> {
        Self::receiving_snapshot(self)
    }

    fn ingest_records(
        &mut self,
        records: &[(RecordKey, Vec<u8>)],
        snapshot: &mut Snapshot,
    ) -> io::Result<()> {
        let _durable = self.ingest_into(records, Some(snapshot))?;
        Ok(())
    }

    fn blob_hashes(&self, snapshot: &Snapshot, key: RecordKey) -> io::Result<BTreeSet<[u8; 32]>> {
        Self::blob_hashes(self, snapshot, key)
    }

    fn read_blob(
        &self,
        snapshot: &Snapshot,
        key: RecordKey,
        hash: [u8; 32],
    ) -> io::Result<Option<Vec<u8>>> {
        Self::read_blob(self, snapshot, key, hash)
    }

    fn ingest_blob(&mut self, key: RecordKey, hash: [u8; 32], bytes: &[u8]) -> io::Result<()> {
        Self::ingest_blob(self, key, hash, bytes)
    }
}

pub(crate) fn validate_records(records: &[(RecordKey, Vec<u8>)]) -> io::Result<()> {
    let total = records
        .iter()
        .fold(0usize, |size, (_, bytes)| size.saturating_add(bytes.len()));
    if records.len() > INVENTORY_PAGE || total > MAX_OBJECT_BYTES {
        return Err(invalid("record batch exceeds limit"));
    }
    for (key, bytes) in records {
        if RecordKey::from_encoded(bytes)? != *key {
            return Err(invalid("record identity or digest mismatch"));
        }
    }
    Ok(())
}
