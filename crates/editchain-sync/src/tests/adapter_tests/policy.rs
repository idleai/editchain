//! Caller-selected export grants and routing remain outside host coordination.

use super::*;
use crate::ExportPolicy;

#[test]
fn selected_scope_never_expands_from_receipts_or_guessed_private_hashes() {
    let directory = tempfile::tempdir().unwrap();
    let private = b"private blob";
    let permitted_record = blob_record(1, private, 12).unwrap();
    let hidden = record(2, b"withheld record").unwrap();
    seed(
        directory.path(),
        &[permitted_record.clone(), hidden.clone()],
    )
    .unwrap();
    let mut blobs = BlobStore::new(directory.path().join("blobs")).unwrap();
    blobs.write(private).unwrap();
    let mut storage = StoreReplica::new(
        SegmentStore::open(directory.path()).unwrap(),
        blobs,
        ExportScope::selected(
            "selected",
            BTreeSet::from([permitted_record.0]),
            BTreeSet::new(),
        )
        .unwrap(),
    );
    let snapshot = storage.snapshot().unwrap();
    assert_eq!(snapshot.len(), 1);
    assert!(!snapshot.contains(hidden.0));
    let hash = *blake3::hash(private).as_bytes();
    assert_eq!(
        storage
            .read_blob(&snapshot, permitted_record.0, hash)
            .unwrap(),
        None
    );
    let guessed = blob_record(3, private, 12).unwrap();
    let mut local = storage.receiving_snapshot().unwrap();
    storage
        .ingest_records(std::slice::from_ref(&guessed), &mut local)
        .unwrap();
    assert!(local.contains(guessed.0), "incoming evidence is retained");
    assert_eq!(
        storage.snapshot().unwrap().len(),
        1,
        "receipt cannot expand export grants"
    );
    assert!(storage.read_blob(&local, guessed.0, hash).is_err());
    assert!(
        storage.confirm_blob(&local, guessed.0, hash).unwrap(),
        "local durable content can avoid a download without granting export"
    );
    assert_eq!(
        storage
            .read_blob(&snapshot, permitted_record.0, hash)
            .unwrap(),
        None
    );
}

struct Revocable {
    scope: ExportScope,
    active: Rc<Cell<bool>>,
}

impl ExportPolicy for Revocable {
    fn namespace(&self) -> &str {
        self.scope.namespace()
    }

    fn check(&self) -> io::Result<()> {
        if !self.active.get() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "revoked policy",
            ));
        }
        Ok(())
    }

    fn share_record(&self, key: RecordKey, encoded: &[u8]) -> io::Result<bool> {
        self.scope.share_record(key, encoded)
    }

    fn share_blob(&self, key: RecordKey, hash: [u8; 32]) -> io::Result<bool> {
        self.scope.share_blob(key, hash)
    }
}

#[test]
fn revocation_stops_an_in_flight_cached_record() {
    let directory = tempfile::tempdir().unwrap();
    let entry = record(1, &vec![17; 190_000]).unwrap();
    seed(directory.path(), std::slice::from_ref(&entry)).unwrap();
    let active = Rc::new(Cell::new(true));
    let storage = StoreReplica::new(
        SegmentStore::open(directory.path()).unwrap(),
        BlobStore::new(directory.path().join("blobs")).unwrap(),
        Revocable {
            scope: ExportScope::all("revocable").unwrap(),
            active: Rc::clone(&active),
        },
    );
    let mut session = Session::new(storage);
    let _requests = session.receive(session.hello()).unwrap();
    let _page = session.receive(Message::Inventory { after: None }).unwrap();
    let first = session
        .receive(Message::Need {
            record: entry.0,
            blob: None,
            offset: 0,
        })
        .unwrap();
    assert!(
        matches!(first.as_slice(), [Message::Chunk { bytes, .. }] if bytes.len() == crate::CHUNK_BYTES)
    );
    active.set(false);
    assert!(
        session
            .receive(Message::Need {
                record: entry.0,
                blob: None,
                offset: u32::try_from(crate::CHUNK_BYTES).unwrap(),
            })
            .is_err(),
        "cached content must still obey revocation"
    );
    assert!(session.tick().is_err());
}

#[test]
fn wrong_peer_mismatched_namespace_and_truncated_streams_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let network = Network::default();
    let mut peer = PeerConnection::new("alice", replica(directory.path()), network.wire("bob"));
    peer.start().unwrap();
    let hello = Message::Hello {
        version: crate::PEER_VERSION,
        encoding: 1,
        space: "caller/chain:7".into(),
    };
    assert!(peer
        .receive("mallory", &encode_message(&hello).unwrap())
        .is_err());
    assert!(!peer.progress().accepted);
    assert!(peer.tick().is_err());
    let (storage, wire) = peer.into_parts();
    let mut peer = PeerConnection::new("alice", storage, wire);
    peer.start().unwrap();
    let other = Message::Hello {
        version: crate::PEER_VERSION,
        encoding: 1,
        space: "other-chain".into(),
    };
    assert!(peer
        .receive("alice", &encode_message(&other).unwrap())
        .is_err());
    assert!(!peer.progress().accepted);
    let (storage, wire) = peer.into_parts();
    let mut peer = PeerConnection::new("alice", storage, wire);
    peer.start().unwrap();
    peer.receive("alice", &[10, 0]).unwrap();
    assert!(peer.finish().is_err());
    assert!(peer.tick().is_err());
}

#[test]
fn structured_metadata_and_nested_revision_can_arrive_in_separate_rounds() {
    let directory = tempfile::tempdir().unwrap();
    let ar = directory.path().join("a");
    let br = directory.path().join("b");
    let content = b"historical revision\0\xff";
    let hash = *blake3::hash(content).as_bytes();
    let metadata = serde_json::to_vec(&serde_json::json!({
        "source": "vscode.work", "schema": 1, "session": "retained", "turn": 1,
        "source_event": OpId::new(NodeId(7), 1, 1), "kind": "read", "summary": "read source",
        "after": { "document": "buffer", "version": 1, "content": ContentId::Hash256(hash) }
    }))
    .unwrap();
    let metadata_hash = *blake3::hash(&metadata).as_bytes();
    let mut op = operation(1, Payload::Empty);
    op.kind = OpKind::Import(editchain_core::ImportOp {
        raw_ref: Payload::Blob(BlobRef {
            id: ContentId::Hash256(metadata_hash),
            len: u32::try_from(metadata.len()).unwrap(),
        }),
        raw_hash: None,
    });
    let bytes = encode_op(&op).unwrap();
    let key = RecordKey::from_encoded(&bytes).unwrap();
    seed(&ar, &[(key, bytes)]).unwrap();
    let network = Network::default();
    let (mut a, mut b) = network.peers(&ar, &br);
    a.start().unwrap();
    b.start().unwrap();
    network.drain(&mut a, &mut b).unwrap();
    assert_eq!(b.progress().unavailable, 1);
    BlobStore::new(ar.join("blobs"))
        .unwrap()
        .write(&metadata)
        .unwrap();
    b.tick().unwrap();
    network.drain(&mut a, &mut b).unwrap();
    assert_eq!(b.progress().blobs, 1, "metadata was hydrated");
    assert_eq!(
        b.progress().unavailable,
        1,
        "nested content is explicitly missing"
    );
    BlobStore::new(ar.join("blobs"))
        .unwrap()
        .write(content)
        .unwrap();
    b.tick().unwrap();
    network.drain(&mut a, &mut b).unwrap();
    assert_eq!(b.progress().blobs, 2);
    assert_eq!(b.progress().unavailable, 0);
    assert_eq!(
        BlobStore::new(br.join("blobs"))
            .unwrap()
            .get(&hash)
            .unwrap()
            .as_deref(),
        Some(content.as_slice())
    );
}
