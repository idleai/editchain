//! Exact evidence, explicit export scope, and durable receiving transactions.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::ops::Bound::{Excluded, Unbounded};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editchain_core::{ContentId, OpId, Payload};
use editchain_store::durable::{atomic_write, sync_parent_dir};
use editchain_store::format::{decode_op, Page};
use editchain_store::BlobStore;
use serde::{Deserialize, Serialize};

use crate::evidence::{Evidence, Records};
use crate::scope::{ensure_revision, Scope};
use crate::SharingScope;
use crate::{content, invalid, CHUNK_BYTES, INVENTORY_PAGE, MAX_OBJECT_BYTES};

/// Identity of one exact representation, including quarantined variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RecordKey {
    /// Original operation identity.
    pub id: OpId,
    /// BLAKE3 of the exact encoded operation bytes.
    pub digest: [u8; 32],
}

impl RecordKey {
    /// Validate an encoded operation and derive its evidence identity.
    ///
    /// # Errors
    /// Rejects unsupported, incomplete, trailing or oversized operation bytes.
    pub fn from_encoded(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_OBJECT_BYTES {
            return Err(invalid("operation exceeds replication limit"));
        }
        let op = decode_op(bytes).map_err(io::Error::other)?;
        Ok(Self {
            id: op.id,
            digest: *blake3::hash(bytes).as_bytes(),
        })
    }
}

pub(crate) fn validate_space(space: &str) -> io::Result<()> {
    if space.is_empty()
        || space.len() > 128
        || !space
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(invalid("invalid collaboration space"));
    }
    Ok(())
}

/// Stable inventory and exact bytes for one bounded reconciliation round.
#[derive(Debug, Default)]
pub struct Snapshot {
    records: Records,
    received: BTreeSet<RecordKey>,
    received_blobs: BTreeSet<[u8; 32]>,
    scope_revision: u64,
}

impl Snapshot {
    pub(crate) fn ordered_keys(&self) -> io::Result<Vec<RecordKey>> {
        crate::inventory::parent_first(&self.records)
    }

    pub(crate) fn received_blob(&mut self, hash: [u8; 32]) {
        let _: bool = self.received_blobs.insert(hash);
    }

    /// Read at most one page following an exclusive, stable record cursor.
    #[must_use]
    pub fn page(&self, after: Option<RecordKey>) -> (Vec<RecordKey>, bool) {
        let lower = after.map_or(Unbounded, Excluded);
        let mut keys = self.records.range((lower, Unbounded)).map(|(key, _)| *key);
        let page = keys.by_ref().take(INVENTORY_PAGE).collect();
        (page, keys.next().is_some())
    }

    /// Inspect exact bytes without re-encoding an operation.
    #[must_use]
    pub fn record(&self, key: RecordKey) -> Option<&[u8]> {
        self.records.get(&key).map(AsRef::as_ref)
    }

    /// Whether this round includes an exact variant.
    #[must_use]
    pub fn contains(&self, key: RecordKey) -> bool {
        self.records.contains_key(&key)
    }

    /// Number of retained, shareable variants, including conflicts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the shareable inventory is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Enumerate directly referenced, supported content hashes for a record.
    ///
    /// # Errors
    /// Rejects absent records or unsupported operation encodings.
    pub fn blob_hashes(&self, key: RecordKey) -> io::Result<BTreeSet<[u8; 32]>> {
        let encoded = self
            .record(key)
            .ok_or_else(|| invalid("record outside export scope"))?;
        let op = decode_op(encoded).map_err(io::Error::other)?;
        Ok(content::hashes(&op))
    }

    fn permits_blob(&self, key: RecordKey, hash: [u8; 32]) -> bool {
        !self.received.contains(&key) || self.received_blobs.contains(&hash)
    }
}

/// Local durable history bound to one explicitly approved collaboration space.
#[derive(Debug)]
pub struct Replica {
    root: PathBuf,
    space: String,
    evidence: Evidence,
    scope_revision: Cell<u64>,
}

impl Replica {
    /// Inspect the durable binding without creating storage or changing consent.
    ///
    /// # Errors
    /// Rejects unreadable, oversized, invalid or unsupported scope metadata.
    pub fn bound_space(root: &Path) -> io::Result<Option<String>> {
        match Scope::read(root) {
            Ok(scope) => Ok(Some(scope.space)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Inspect effective sharing consent without reading history or changing it.
    ///
    /// # Errors
    /// Rejects damaged metadata. An interrupted change is reported as inactive.
    pub fn sharing_scope(root: &Path) -> io::Result<Option<SharingScope>> {
        match Scope::read(root) {
            Ok(scope) => {
                let mut summary = scope.summary();
                if let Err(error) = ensure_revision(root, scope.revision) {
                    if error.get_ref().is_some_and(
                        <dyn std::error::Error + Send + Sync>::is::<crate::scope::ScopeChanged>,
                    ) {
                        summary.active = false;
                    } else {
                        return Err(error);
                    }
                }
                Ok(Some(summary))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Enable a space, capturing an exclusion baseline unless backfill was chosen.
    /// Existing scope decisions persist across reconnects and process restarts.
    ///
    /// # Errors
    /// Rejects invalid/mismatched spaces, a busy writer, or failed durable metadata.
    pub fn open(root: &Path, space: &str, backfill: bool) -> io::Result<Self> {
        validate_space(space)?;
        let replica = Self {
            root: root.to_owned(),
            space: space.to_owned(),
            evidence: Evidence::new(root),
            scope_revision: Cell::new(0),
        };
        let _writer = crate::writer(root)?;
        if replica.scope_path().exists() {
            let scope = Scope::read(root)?;
            replica.check_space(&scope)?;
            replica.scope_revision.set(scope.revision);
        } else {
            let excluded = if backfill {
                BTreeSet::new()
            } else {
                replica
                    .evidence
                    .read(root)?
                    .records
                    .keys()
                    .copied()
                    .collect()
            };
            replica.save_scope(&Scope {
                version: 1,
                space: space.to_owned(),
                excluded,
                received: BTreeSet::new(),
                received_blobs: BTreeSet::new(),
                local: BTreeSet::new(),
                revision: 0,
                cutoff: None,
            })?;
        }
        Ok(replica)
    }

    /// Space identity used in protocol negotiation.
    #[must_use]
    pub fn space(&self) -> &str {
        &self.space
    }

    /// Build a stable inventory after prior writers have completed their fsync.
    ///
    /// # Errors
    /// Returns writer contention, scope, or canonical storage errors.
    pub fn snapshot(&self) -> io::Result<Snapshot> {
        self.snapshot_for(false)
    }

    pub(crate) fn receiving_snapshot(&self) -> io::Result<Snapshot> {
        self.snapshot_for(true)
    }

    fn snapshot_for(&self, receiving: bool) -> io::Result<Snapshot> {
        self.ensure_scope()?;
        self.evidence.warm(&self.root)?;
        let _writer = crate::writer(&self.root)?;
        let scope = self.load_scope()?;
        let evidence = self.evidence.read(&self.root)?;
        let records = evidence
            .records
            .iter()
            .filter(|(key, record)| {
                !scope.excludes(**key, record.segment) || receiving && scope.received.contains(key)
            })
            .map(|(key, record)| (*key, Arc::clone(&record.bytes)))
            .collect();
        Ok(Snapshot {
            records,
            received: scope.received,
            received_blobs: scope.received_blobs,
            scope_revision: scope.revision,
        })
    }

    /// Operations decoded from segments by this replica's retained reader.
    /// In-memory reference inspection is not included. Useful for measuring
    /// whether small receipts replay previously indexed history.
    #[must_use]
    pub fn decoded_records(&self) -> u64 {
        self.evidence.decoded()
    }

    /// Explicitly include previously withheld history in future inventories.
    ///
    /// # Errors
    /// Returns writer contention or a failed durable scope update.
    pub fn include_backfill(&self) -> io::Result<()> {
        self.set_sharing_scope(true).map(|_| ())
    }

    /// Explicitly select all retained history or a fresh append cutoff. Reopening
    /// and reconnecting never call this. Receipts, history and approvals survive.
    ///
    /// # Errors
    /// Returns writer contention or metadata failure. Interrupted selections
    /// fail closed until the user repeats this explicit selection.
    pub fn set_sharing_scope(&self, backfill: bool) -> io::Result<SharingScope> {
        let writer = crate::writer(&self.root)?;
        let mut scope = Scope::read(&self.root)?;
        self.check_space(&scope)?;
        scope.select(&self.root, (!backfill).then(|| writer.segment_sequence()))?;
        self.save_scope(&scope)?;
        self.scope_revision.set(scope.revision);
        Ok(scope.summary())
    }

    pub(crate) fn ensure_scope(&self) -> io::Result<()> {
        ensure_revision(&self.root, self.scope_revision.get())
    }

    fn ensure_snapshot(&self, snapshot: &Snapshot) -> io::Result<()> {
        self.ensure_scope()?;
        if snapshot.scope_revision != self.scope_revision.get() {
            return Err(crate::scope::changed());
        }
        Ok(())
    }

    /// Persist exact variants, returning only keys whose bytes are now durable.
    ///
    /// Origin metadata is persisted first: a crash cannot cause received records
    /// to authorize access to unrelated private blobs already on this machine.
    ///
    /// # Errors
    /// Rejects malformed/mismatched records before writing, writer contention,
    /// or any failed fsync. A caller must not acknowledge an error.
    pub fn ingest_records(&self, records: &[(RecordKey, Vec<u8>)]) -> io::Result<Vec<RecordKey>> {
        self.ingest_into(records, None)
    }

    pub(crate) fn ingest_into(
        &self,
        records: &[(RecordKey, Vec<u8>)],
        snapshot: Option<&mut Snapshot>,
    ) -> io::Result<Vec<RecordKey>> {
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
        self.evidence.warm(&self.root)?;
        let mut writer = crate::writer(&self.root)?;
        let known = self.evidence.read(&self.root)?;
        let mut staged = BTreeSet::new();
        let mut scope = self.load_scope()?;
        if let Some(snapshot) = snapshot.as_ref() {
            self.ensure_snapshot(snapshot)?;
        }
        let mut page = Page::new(0);
        let mut scope_changed = false;
        for (key, bytes) in records {
            // An independently supplied exact baseline record is now shared
            // evidence, but cannot expose this device's preexisting blobs.
            // Its local provenance is retained separately: the bytes existed
            // here before a peer supplied them, so capture must keep deriving
            // from them even though export now needs a receipt.
            let excluded = scope.excluded.remove(key);
            let withheld = known
                .records
                .get(key)
                .is_some_and(|record| scope.before_cutoff(record.segment));
            if excluded || withheld {
                if scope.received.insert(*key) {
                    let _: bool = scope.local.insert(*key);
                    scope_changed = true;
                }
                scope_changed |= excluded;
            }
            if !known.records.contains_key(key) && staged.insert(*key) {
                let _: bool = scope.received.insert(*key);
                page.add_record(0, bytes.clone());
            }
        }
        if scope_changed || !page.records.is_empty() {
            self.save_scope(&scope)?;
        }
        if !page.records.is_empty() {
            writer.append_page(&page)?;
        }
        if let Some(snapshot) = snapshot {
            // Only the receipt page changes the receiver's view. The outgoing
            // snapshot and its pagination remain frozen for the entire round.
            for (key, bytes) in records {
                drop(snapshot.records.insert(*key, Arc::from(bytes.as_slice())));
            }
            snapshot.received = scope.received;
            snapshot.received_blobs = scope.received_blobs;
        }
        Ok(records.iter().map(|(key, _)| *key).collect())
    }

    /// Read one authorized blob chunk with a verified full content hash.
    ///
    /// # Errors
    /// Rejects out-of-scope references, corrupt/oversized content or bad offsets.
    /// Missing or not-yet-received content is returned as `None`.
    pub fn blob_chunk(
        &self,
        snapshot: &Snapshot,
        key: RecordKey,
        hash: [u8; 32],
        offset: u64,
    ) -> io::Result<Option<(u32, Vec<u8>)>> {
        let Some(bytes) = self.read_blob(snapshot, key, hash)? else {
            return Ok(None);
        };
        let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
        let offset = usize::try_from(offset).map_err(io::Error::other)?;
        let tail = bytes
            .get(offset..)
            .ok_or_else(|| invalid("blob offset exceeds length"))?;
        Ok(Some((
            length,
            tail.iter().take(CHUNK_BYTES).copied().collect(),
        )))
    }

    /// Read bounded, verified bytes for one outgoing blob transfer.
    /// Callers can retain these immutable bytes while sending multiple chunks.
    ///
    /// # Errors
    /// Rejects corrupt content, mismatched reference lengths and storage errors.
    pub fn read_blob(
        &self,
        snapshot: &Snapshot,
        key: RecordKey,
        hash: [u8; 32],
    ) -> io::Result<Option<Vec<u8>>> {
        self.ensure_snapshot(snapshot)?;
        if !snapshot.permits_blob(key, hash) {
            return Ok(None);
        }
        let references = self.references(snapshot, key)?;
        self.read_content(&references, hash)
    }

    /// Resolve typed content references, including retained human-work revisions.
    /// Missing structured payloads are hydrated before their child references.
    ///
    /// # Errors
    /// Rejects records outside the scope, corrupt metadata or content.
    pub fn blob_hashes(
        &self,
        snapshot: &Snapshot,
        key: RecordKey,
    ) -> io::Result<BTreeSet<[u8; 32]>> {
        Ok(self.references(snapshot, key)?.into_keys().collect())
    }

    fn references(&self, snapshot: &Snapshot, key: RecordKey) -> io::Result<content::References> {
        self.ensure_snapshot(snapshot)?;
        let encoded = snapshot
            .record(key)
            .ok_or_else(|| invalid("record outside scope"))?;
        self.record_references(encoded, |hash| snapshot.permits_blob(key, hash))
    }

    fn record_references(
        &self,
        encoded: &[u8],
        permits_blob: impl Fn([u8; 32]) -> bool,
    ) -> io::Result<content::References> {
        let op = decode_op(encoded).map_err(io::Error::other)?;
        let mut references = content::references(&op);
        if let Some(Payload::Blob(reference)) = content::structured_payload(&op) {
            if let ContentId::Hash256(hash) = reference.id {
                if permits_blob(hash) {
                    if let Some(bytes) = self.read_content(&references, hash)? {
                        content::nested(&mut references, &bytes);
                    }
                }
            }
        }
        Ok(references)
    }

    fn read_content(
        &self,
        references: &content::References,
        hash: [u8; 32],
    ) -> io::Result<Option<Vec<u8>>> {
        if !references.contains_key(&hash) {
            return Ok(None);
        }
        let Some(store) = BlobStore::open_read_only(self.root.join("blobs"))? else {
            return Ok(None);
        };
        let file = match fs::File::open(store.path_for(&hash)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let length = file.metadata()?.len();
        if !content::matches_len(references, hash, length) {
            return Err(invalid("blob reference length mismatch"));
        }
        if length > u64::try_from(MAX_OBJECT_BYTES).map_err(io::Error::other)? {
            return Err(invalid("blob length exceeds limit"));
        }
        let mut bytes = Vec::new();
        let _: usize = file
            .take(length.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_err(io::Error::other)? != length
            || blake3::hash(&bytes).as_bytes() != &hash
        {
            return Err(invalid("stored blob hash or length mismatch"));
        }
        Ok(Some(bytes))
    }

    /// Verify and publish a blob referenced by retained evidence in this space.
    ///
    /// # Errors
    /// Rejects absent references, hash mismatches, size overflow and failed fsync.
    pub fn ingest_blob(&self, key: RecordKey, hash: [u8; 32], bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_OBJECT_BYTES || blake3::hash(bytes).as_bytes() != &hash {
            return Err(invalid("received blob hash or length mismatch"));
        }
        self.evidence.warm(&self.root)?;
        let _writer = crate::writer(&self.root)?;
        let mut scope = self.load_scope()?;
        let evidence = self.evidence.read(&self.root)?;
        let encoded = evidence
            .records
            .get(&key)
            .filter(|record| !scope.excludes(key, record.segment) || scope.received.contains(&key))
            .ok_or_else(|| invalid("record outside scope"))?;
        let references = self.record_references(&encoded.bytes, |hash| {
            !scope.received.contains(&key) || scope.received_blobs.contains(&hash)
        })?;
        if !content::matches_len(
            &references,
            hash,
            u64::try_from(bytes.len()).map_err(io::Error::other)?,
        ) {
            return Err(invalid("blob is not referenced with this length"));
        }
        BlobStore::new(self.root.join("blobs"))?.write(bytes)?;
        sync_parent_dir(&self.root.join("blobs"))?;
        let _: bool = scope.received_blobs.insert(hash);
        self.save_scope(&scope)
    }

    fn scope_path(&self) -> PathBuf {
        self.root.join("multiplayer").join("scope.json")
    }

    fn load_scope(&self) -> io::Result<Scope> {
        self.ensure_scope()?;
        let scope = Scope::read(&self.root)?;
        self.check_space(&scope)?;
        if scope.revision != self.scope_revision.get() {
            return Err(crate::scope::changed());
        }
        Ok(scope)
    }

    fn check_space(&self, scope: &Scope) -> io::Result<()> {
        if scope.space != self.space {
            return Err(invalid(
                "workspace belongs to a different collaboration space",
            ));
        }
        Ok(())
    }

    fn save_scope(&self, scope: &Scope) -> io::Result<()> {
        fs::create_dir_all(self.root.join("multiplayer"))?;
        sync_parent_dir(&self.root.join("multiplayer"))?;
        let bytes = serde_json::to_vec(scope).map_err(io::Error::other)?;
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(invalid("scope metadata exceeds limit"));
        }
        atomic_write(&self.scope_path(), &bytes)
    }
}
