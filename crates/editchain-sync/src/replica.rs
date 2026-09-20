//! Exact evidence, explicit export scope, and durable receiving transactions.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::ops::Bound::{Excluded, Unbounded};
use std::path::{Path, PathBuf};

use editchain_core::{Admission, ContentId, OpId, Payload};
use editchain_store::durable::{atomic_write, sync_parent_dir};
use editchain_store::format::{decode_op, Page};
use editchain_store::{BlobStore, CanonicalChain};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    version: u16,
    space: String,
    excluded: BTreeSet<RecordKey>,
    received: BTreeSet<RecordKey>,
    received_blobs: BTreeSet<[u8; 32]>,
    /// Exact records this device retained locally before a peer independently
    /// supplied the same bytes. Kept separate from `received` so that export
    /// permission and blob gating never imply peer authorship.
    ///
    /// Version-1 ledgers written before this field existed default to empty.
    /// Their received-but-formerly-excluded entries are ambiguous and are not
    /// retroactively attributed to this device.
    #[serde(default)]
    local: BTreeSet<RecordKey>,
}

impl Scope {
    fn read(root: &Path) -> io::Result<Self> {
        const LIMIT: u64 = 128 * 1024 * 1024;
        let file = fs::File::open(root.join("multiplayer/scope.json"))?;
        if file.metadata()?.len() > LIMIT {
            return Err(invalid("scope metadata exceeds limit"));
        }
        let mut bytes = Vec::new();
        let _read = file.take(LIMIT.saturating_add(1)).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_err(io::Error::other)? > LIMIT {
            return Err(invalid("scope metadata exceeds limit"));
        }
        let scope: Self =
            serde_json::from_slice(&bytes).map_err(|_error| invalid("invalid scope metadata"))?;
        if scope.version != 1 {
            return Err(invalid("unsupported scope version"));
        }
        validate_space(&scope.space)?;
        Ok(scope)
    }
}

fn validate_space(space: &str) -> io::Result<()> {
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
    records: BTreeMap<RecordKey, Vec<u8>>,
    received: BTreeSet<RecordKey>,
    received_blobs: BTreeSet<[u8; 32]>,
}

impl Snapshot {
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
        self.records.get(&key).map(Vec::as_slice)
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
        };
        let _writer = crate::writer(root)?;
        if replica.scope_path().exists() {
            let _scope = replica.load_scope()?;
        } else {
            let chain = CanonicalChain::read(root)?;
            let excluded = if backfill {
                BTreeSet::new()
            } else {
                evidence(&chain).keys().copied().collect()
            };
            replica.save_scope(&Scope {
                version: 1,
                space: space.to_owned(),
                excluded,
                received: BTreeSet::new(),
                received_blobs: BTreeSet::new(),
                local: BTreeSet::new(),
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
        let _writer = crate::writer(&self.root)?;
        let scope = self.load_scope()?;
        let chain = CanonicalChain::read(&self.root)?;
        let mut records = evidence(&chain);
        records.retain(|key, _| !scope.excluded.contains(key));
        Ok(Snapshot {
            records,
            received: scope.received,
            received_blobs: scope.received_blobs,
        })
    }

    /// Explicitly include previously withheld history in future inventories.
    ///
    /// # Errors
    /// Returns writer contention or a failed durable scope update.
    pub fn include_backfill(&self) -> io::Result<()> {
        let _writer = crate::writer(&self.root)?;
        let mut scope = self.load_scope()?;
        scope.excluded.clear();
        self.save_scope(&scope)
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
        let mut writer = crate::writer(&self.root)?;
        let chain = CanonicalChain::read(&self.root)?;
        let mut known = chain.evidence().clone();
        let mut scope = self.load_scope()?;
        let mut page = Page::new(0);
        let mut scope_changed = false;
        for (key, bytes) in records {
            // An independently supplied exact baseline record is now shared
            // evidence, but cannot expose this device's preexisting blobs.
            // Its local provenance is retained separately: the bytes existed
            // here before a peer supplied them, so capture must keep deriving
            // from them even though export now needs a receipt.
            if scope.excluded.remove(key) {
                let _: bool = scope.received.insert(*key);
                let _: bool = scope.local.insert(*key);
                scope_changed = true;
            }
            if known.classify(key.id, bytes) != Admission::Duplicate {
                let _: bool = scope.received.insert(*key);
                let _: Admission = known.insert(key.id, bytes.clone());
                page.add_record(0, bytes.clone());
            }
        }
        if scope_changed || !page.records.is_empty() {
            self.save_scope(&scope)?;
        }
        if !page.records.is_empty() {
            writer.append_page(&page)?;
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
        let encoded = snapshot
            .record(key)
            .ok_or_else(|| invalid("record outside scope"))?;
        let op = decode_op(encoded).map_err(io::Error::other)?;
        let mut references = content::references(&op);
        if let Some(Payload::Blob(reference)) = content::structured_payload(&op) {
            if let ContentId::Hash256(hash) = reference.id {
                if snapshot.permits_blob(key, hash) {
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
        let _writer = crate::writer(&self.root)?;
        let mut scope = self.load_scope()?;
        let chain = CanonicalChain::read(&self.root)?;
        let mut records = evidence(&chain);
        records.retain(|key, _| !scope.excluded.contains(key));
        let snapshot = Snapshot {
            records,
            received: scope.received.clone(),
            received_blobs: scope.received_blobs.clone(),
        };
        if !content::matches_len(
            &self.references(&snapshot, key)?,
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
        let scope = Scope::read(&self.root)?;
        if scope.space != self.space {
            return Err(invalid(
                "workspace belongs to a different collaboration space",
            ));
        }
        Ok(scope)
    }

    fn save_scope(&self, scope: &Scope) -> io::Result<()> {
        fs::create_dir_all(self.root.join("multiplayer"))?;
        sync_parent_dir(&self.root.join("multiplayer"))?;
        atomic_write(
            &self.scope_path(),
            &serde_json::to_vec(scope).map_err(io::Error::other)?,
        )
    }
}

fn evidence(chain: &CanonicalChain) -> BTreeMap<RecordKey, Vec<u8>> {
    chain
        .evidence()
        .evidence()
        .map(|(id, bytes)| {
            (
                RecordKey {
                    id: *id,
                    digest: *blake3::hash(bytes).as_bytes(),
                },
                bytes.to_vec(),
            )
        })
        .collect()
}
