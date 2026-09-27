//! No protocol success can be manufactured from uncertain backend outcomes.

use super::*;

#[derive(Default)]
struct Faults {
    append: Cell<bool>,
    sync: Cell<bool>,
    blob: Cell<bool>,
}

struct UncertainLog {
    inner: SegmentStore,
    faults: Rc<Faults>,
}

impl AppendLog for UncertainLog {
    fn visit_records(
        &self,
        visitor: &mut editchain_store::RecordVisitor<'_>,
    ) -> io::Result<editchain_store::LogReadStats> {
        self.inner.visit_records(visitor)
    }

    fn append_record(&mut self, flags: u8, encoded: &[u8]) -> io::Result<()> {
        self.inner.append_record(flags, encoded)?;
        if self.faults.append.get() {
            return Err(io::Error::other("injected uncertain append"));
        }
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        if self.faults.sync.get() {
            return Err(io::Error::other("injected failed durability fence"));
        }
        self.inner.sync()
    }
}

struct UncertainBlobs {
    inner: BlobStore,
    faults: Rc<Faults>,
}

impl BlobSource for UncertainBlobs {
    fn read_content(&self, id: ContentId) -> io::Result<editchain_store::BlobResolution> {
        self.inner.read_content(id)
    }
}

impl BlobStorage for UncertainBlobs {
    fn put(&mut self, bytes: &[u8]) -> io::Result<BlobRef> {
        let reference = self.inner.put(bytes)?;
        if self.faults.blob.get() {
            return Err(io::Error::other("injected uncertain blob publication"));
        }
        Ok(reference)
    }
}

fn faulty_replica(
    root: &Path,
    faults: &Rc<Faults>,
) -> StoreReplica<UncertainLog, UncertainBlobs, ExportScope> {
    StoreReplica::new(
        UncertainLog {
            inner: SegmentStore::open(root).unwrap(),
            faults: Rc::clone(faults),
        },
        UncertainBlobs {
            inner: BlobStore::new(root.join("blobs")).unwrap(),
            faults: Rc::clone(faults),
        },
        ExportScope::all("caller/chain:7").unwrap(),
    )
}

fn page(key: RecordKey) -> Message {
    Message::Page {
        offset: 0,
        total: 1,
        records: vec![key],
        more: false,
    }
}

fn chunk(record: RecordKey, blob: Option<[u8; 32]>, bytes: &[u8]) -> Message {
    Message::Chunk {
        record,
        blob,
        offset: 0,
        total: u32::try_from(bytes.len()).unwrap(),
        bytes: bytes.to_vec(),
    }
}

#[test]
fn uncertain_records_conflicts_and_failed_sync_never_return_ack_or_checked() {
    let directory = tempfile::tempdir().unwrap();
    let faults = Rc::new(Faults::default());
    let mut storage = faulty_replica(directory.path(), &faults);
    for variant in [b"original\0\xff".as_slice(), b"conflict".as_slice()] {
        let (key, bytes) = record(1, variant).unwrap();
        let mut session = Session::new(storage);
        let _requests = session.receive(session.hello()).unwrap();
        let _requests = session.receive(page(key)).unwrap();
        faults.append.set(true);
        assert!(
            session.receive(chunk(key, None, &bytes)).is_err(),
            "backend failure cannot produce replies"
        );
        assert_eq!(session.progress().records, 0);
        assert!(session.tick().is_err(), "failed sessions must reconnect");
        storage = session.into_storage();
        assert!(CanonicalChain::read(directory.path())
            .unwrap()
            .evidence()
            .evidence()
            .any(|(_, value)| value == bytes));

        faults.sync.set(true);
        let mut retry = Session::new(storage);
        assert!(
            retry.receive(retry.hello()).is_err(),
            "readable records cannot enter a durable resume inventory"
        );
        storage = retry.into_storage();
        let mut snapshot = Snapshot::default();
        assert!(storage
            .ingest_records(&[(key, bytes)], &mut snapshot)
            .is_err());
        assert!(
            snapshot.is_empty(),
            "a failed duplicate cannot update receipt state"
        );

        faults.sync.set(false);
        faults.append.set(false);
        let mut retry = Session::new(storage);
        let _requests = retry.receive(retry.hello()).unwrap();
        assert_eq!(
            retry.receive(page(key)).unwrap(),
            vec![Message::Checked { end: 1 }]
        );
        assert_eq!(
            retry.progress().records,
            0,
            "durable resume avoids retransmission"
        );
        storage = retry.into_storage();
    }
    let chain = CanonicalChain::read(directory.path()).unwrap();
    assert_eq!(chain.stats().records, 2);
    assert_eq!(chain.stats().quarantined, 2);
    assert_eq!(chain.stats().duplicates, 0);
}

#[test]
fn uncertain_blob_publication_cannot_be_skipped_on_reconnect() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = b"late evidence\0\xff";
    let entry = blob_record(1, bytes, u32::try_from(bytes.len()).unwrap()).unwrap();
    seed(directory.path(), std::slice::from_ref(&entry)).unwrap();
    let faults = Rc::new(Faults::default());
    let mut session = Session::new(faulty_replica(directory.path(), &faults));
    let _requests = session.receive(session.hello()).unwrap();
    let _requests = session.receive(page(entry.0)).unwrap();
    let hash = *blake3::hash(bytes).as_bytes();
    faults.blob.set(true);
    assert!(session.receive(chunk(entry.0, Some(hash), bytes)).is_err());
    assert_eq!(session.progress().blobs, 0);
    assert_eq!(session.progress().incoming.checked_records, 0);
    assert_eq!(
        BlobStore::new(directory.path().join("blobs"))
            .unwrap()
            .get(&hash)
            .unwrap()
            .as_deref(),
        Some(bytes.as_slice())
    );

    let mut retry = Session::new(session.into_storage());
    let _requests = retry.receive(retry.hello()).unwrap();
    assert!(
        retry.receive(page(entry.0)).is_err(),
        "readable content still requires a successful durability fence"
    );
    assert!(!retry.progress().incoming.complete);
    faults.blob.set(false);
    let mut retry = Session::new(retry.into_storage());
    let _requests = retry.receive(retry.hello()).unwrap();
    assert_eq!(
        retry.receive(page(entry.0)).unwrap(),
        vec![Message::Checked { end: 1 }]
    );
    assert_eq!(retry.progress().blobs, 0);
    assert!(
        retry.progress().incoming.complete,
        "durable retry repairs the uncertain publication"
    );
}

#[test]
fn invalid_batches_and_corrupt_or_unreferenced_blobs_cannot_be_published() {
    let directory = tempfile::tempdir().unwrap();
    let mut storage = replica(directory.path());
    let entry = blob_record(1, b"good", 4).unwrap();
    let mut bad = record(2, b"valid before mutation").unwrap();
    bad.1.push(0);
    let mut snapshot = Snapshot::default();
    assert!(storage
        .ingest_records(&[entry.clone(), bad], &mut snapshot)
        .is_err());
    assert!(storage.receiving_snapshot().unwrap().is_empty());
    storage
        .ingest_records(std::slice::from_ref(&entry), &mut snapshot)
        .unwrap();
    for (hash, bytes) in [
        (*blake3::hash(b"good").as_bytes(), b"evil".as_slice()),
        (
            *blake3::hash(b"unreferenced").as_bytes(),
            b"unreferenced".as_slice(),
        ),
    ] {
        assert!(storage.ingest_blob(entry.0, hash, bytes).is_err());
    }
    let wrong_length = blob_record(3, b"good", 5).unwrap();
    storage
        .ingest_records(std::slice::from_ref(&wrong_length), &mut snapshot)
        .unwrap();
    assert!(storage
        .ingest_blob(wrong_length.0, *blake3::hash(b"good").as_bytes(), b"good")
        .is_err());
    let (_, blobs, _) = storage.into_parts();
    assert_eq!(blobs.len().unwrap(), 0);
}

#[test]
fn failed_transport_after_durable_append_recovers_without_another_record() {
    let directory = tempfile::tempdir().unwrap();
    let network = Network::default();
    let wire = network.wire("bob");
    let failure = Rc::clone(&wire.fail_send);
    let storage = replica(directory.path());
    let hello = Session::new(storage).hello();
    // Release the writer held by that temporary session before opening again.
    let mut peer = PeerConnection::new("alice", replica(directory.path()), wire);
    peer.start().unwrap();
    peer.receive("alice", &encode_message(&hello).unwrap())
        .unwrap();
    let entry = record(1, b"stored before lost ack").unwrap();
    peer.receive("alice", &encode_message(&page(entry.0)).unwrap())
        .unwrap();
    failure.set(true);
    assert!(peer
        .receive(
            "alice",
            &encode_message(&chunk(entry.0, None, &entry.1)).unwrap()
        )
        .is_err());
    assert_eq!(peer.progress().records, 1, "local persistence succeeded");
    assert!(
        peer.tick().is_err(),
        "transport failure invalidates the stream"
    );
    let (storage, wire) = peer.into_parts();
    failure.set(false);
    let mut retry = PeerConnection::new("alice", storage, wire);
    retry.start().unwrap();
    retry
        .receive("alice", &encode_message(&hello).unwrap())
        .unwrap();
    retry
        .receive("alice", &encode_message(&page(entry.0)).unwrap())
        .unwrap();
    assert_eq!(retry.progress().records, 0);
    assert_eq!(
        CanonicalChain::read(directory.path())
            .unwrap()
            .stats()
            .records,
        1
    );
}
