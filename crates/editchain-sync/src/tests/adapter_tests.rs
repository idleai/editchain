//! Public adapter/transport contracts against independent durable peers.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;

use editchain_store::{AppendLog, BlobSource, BlobStorage};

use super::*;
use crate::{
    ExportScope, PeerConnection, ReplicationStorage, Session, Snapshot, StoreReplica, Transport,
};

mod failures;
mod policy;
mod scale;

type DiskReplica = StoreReplica<SegmentStore, BlobStore, ExportScope>;
type DiskPeer = PeerConnection<DiskReplica, Wire>;

struct Delivery {
    from: &'static str,
    to: String,
    frame: Vec<u8>,
}

#[derive(Default)]
struct Network {
    queue: Rc<RefCell<VecDeque<Delivery>>>,
}

struct Wire {
    from: &'static str,
    queue: Rc<RefCell<VecDeque<Delivery>>>,
    fail_send: Rc<Cell<bool>>,
}

impl Transport for Wire {
    fn send(&mut self, peer: &str, frame: &[u8]) -> io::Result<()> {
        if self.fail_send.get() {
            return Err(io::Error::other("injected transport failure"));
        }
        self.queue.borrow_mut().push_back(Delivery {
            from: self.from,
            to: peer.to_owned(),
            frame: frame.to_vec(),
        });
        Ok(())
    }
}

impl Network {
    fn wire(&self, from: &'static str) -> Wire {
        Wire {
            from,
            queue: Rc::clone(&self.queue),
            fail_send: Rc::default(),
        }
    }

    fn peers(&self, a: &Path, b: &Path) -> (DiskPeer, DiskPeer) {
        (
            PeerConnection::new("bob", replica(a), self.wire("alice")),
            PeerConnection::new("alice", replica(b), self.wire("bob")),
        )
    }

    fn deliver<A: ReplicationStorage, B: ReplicationStorage>(
        &self,
        a: &mut PeerConnection<A, Wire>,
        b: &mut PeerConnection<B, Wire>,
    ) -> io::Result<bool> {
        let Some(delivery) = self.queue.borrow_mut().pop_front() else {
            return Ok(false);
        };
        for chunk in delivery.frame.chunks(997) {
            if delivery.to == "alice" {
                a.receive(delivery.from, chunk)?;
            } else {
                b.receive(delivery.from, chunk)?;
            }
        }
        Ok(true)
    }

    fn drain<A: ReplicationStorage, B: ReplicationStorage>(
        &self,
        a: &mut PeerConnection<A, Wire>,
        b: &mut PeerConnection<B, Wire>,
    ) -> io::Result<()> {
        for _ in 0..20_000 {
            if !self.deliver(a, b)? {
                return Ok(());
            }
        }
        Err(io::Error::other("replication did not terminate"))
    }

    fn pending(&self, predicate: impl Fn(&Message) -> bool) -> bool {
        self.queue.borrow().iter().any(|delivery| {
            FrameDecoder::default()
                .push(&delivery.frame)
                .unwrap()
                .iter()
                .any(&predicate)
        })
    }
}

fn replica(root: &Path) -> DiskReplica {
    StoreReplica::new(
        SegmentStore::open(root).unwrap(),
        BlobStore::new(root.join("blobs")).unwrap(),
        ExportScope::all("caller/chain:7").unwrap(),
    )
}

fn assert_converged(a: &Path, b: &Path, variants: usize) {
    let a = CanonicalChain::read(a).unwrap();
    let b = CanonicalChain::read(b).unwrap();
    assert_eq!(a.evidence(), b.evidence());
    assert_eq!(a.evidence().evidence().count(), variants);
    assert_eq!(a.stats().duplicates, 0);
    assert_eq!(b.stats().duplicates, 0);
}

#[test]
fn caller_peers_converge_exact_variants_and_late_blobs_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let ar = directory.path().join("a");
    let br = directory.path().join("b");
    let content = vec![0xff; 190_000];
    let blob = blob_record(300, &content, 190_000).unwrap();
    let mut records = (1..=270)
        .map(|seq| record(seq, b"alice\0\xff\r\n").unwrap())
        .collect::<Vec<_>>();
    records.push(blob.clone());
    seed(&ar, &records).unwrap();
    // A valid, noncanonical encoding of the same operation is distinct evidence.
    let original = &records.first().unwrap().1;
    let mut alternate = vec![0x87, 0];
    alternate.extend_from_slice(original.get(1..).unwrap());
    assert_eq!(
        editchain_store::format::decode_op(&alternate).unwrap(),
        editchain_store::format::decode_op(original).unwrap()
    );
    seed(
        &br,
        &[
            (RecordKey::from_encoded(&alternate).unwrap(), alternate),
            record(9, b"bob conflict").unwrap(),
            record(280, b"bob contribution").unwrap(),
        ],
    )
    .unwrap();

    let network = Network::default();
    let (mut a, mut b) = network.peers(&ar, &br);
    a.start().unwrap();
    b.start().unwrap();
    network.drain(&mut a, &mut b).unwrap();
    assert_converged(&ar, &br, 274);
    assert_eq!(CanonicalChain::read(&br).unwrap().stats().quarantined, 4);
    assert_eq!(
        a.progress().unavailable,
        0,
        "bob's frozen inventory predates receipt of the blob record"
    );
    assert_eq!(b.progress().unavailable, 1);
    assert_eq!(a.progress().sent_records, b.progress().records);
    assert_eq!(b.progress().sent_records, a.progress().records);
    assert!(!ar.join("multiplayer").exists(), "no host binding created");
    a.tick().unwrap();
    network.drain(&mut a, &mut b).unwrap();
    assert_eq!(
        a.progress().unavailable,
        1,
        "the next inventory includes the newly received record"
    );

    drop((a, b));
    BlobStore::new(ar.join("blobs"))
        .unwrap()
        .write(&content)
        .unwrap();
    let (mut a, mut b) = network.peers(&ar, &br);
    a.start().unwrap();
    b.start().unwrap();
    network.drain(&mut a, &mut b).unwrap();
    assert_eq!(b.progress().records, 0, "durable records are resumed");
    assert_eq!(b.progress().blobs, 1);
    assert_eq!(a.progress().sent_blobs, 1);
    assert_eq!(
        BlobStore::new(br.join("blobs"))
            .unwrap()
            .read_content(ContentId::Hash256(*blake3::hash(&content).as_bytes()))
            .unwrap(),
        editchain_store::BlobResolution::Found(content)
    );
    a.tick().unwrap();
    b.tick().unwrap();
    network.drain(&mut a, &mut b).unwrap();
    assert_eq!(a.progress().unavailable, 0);
    assert_eq!(b.progress().unavailable, 0);
    assert_converged(&ar, &br, 274);
}

#[test]
fn catch_up_resumes_after_lost_page_ack_and_partial_blob() {
    let directory = tempfile::tempdir().unwrap();
    let ar = directory.path().join("a");
    let br = directory.path().join("b");
    let content = vec![29; 190_000];
    let mut records = (1..=270)
        .map(|seq| record(seq, b"paged history").unwrap())
        .collect::<Vec<_>>();
    records.push(blob_record(300, &content, 190_000).unwrap());
    seed(&ar, &records).unwrap();
    BlobStore::new(ar.join("blobs"))
        .unwrap()
        .write(&content)
        .unwrap();
    let network = Network::default();
    let (mut a, mut b) = network.peers(&ar, &br);
    a.start().unwrap();
    b.start().unwrap();
    for _ in 0..2000 {
        if network.pending(|message| matches!(message, Message::Ack { blob: None, .. })) {
            break;
        }
        assert!(
            network.deliver(&mut a, &mut b).unwrap(),
            "expected pending work"
        );
    }
    assert_eq!(
        b.progress().records,
        128,
        "first page is durable before ack"
    );
    assert_eq!(a.progress().sent_records, 0, "ack delivery was interrupted");
    assert_eq!(CanonicalChain::read(&br).unwrap().stats().records, 128);
    drop((a, b));
    network.queue.borrow_mut().clear();
    let (mut a, mut b) = network.peers(&ar, &br);
    a.start().unwrap();
    b.start().unwrap();
    for _ in 0..2000 {
        if network.pending(|message| matches!(message, Message::Chunk { blob: Some(_), offset, .. } if *offset > 0)) {
            break;
        }
        assert!(
            network.deliver(&mut a, &mut b).unwrap(),
            "expected catch-up work"
        );
    }
    assert_eq!(
        b.progress().records,
        143,
        "only remaining records transferred"
    );
    assert_eq!(
        b.progress().blobs,
        0,
        "partial content cannot be acknowledged"
    );
    assert_eq!(a.progress().sent_blobs, 0);
    assert_eq!(BlobStore::new(br.join("blobs")).unwrap().len().unwrap(), 0);
    drop((a, b));
    network.queue.borrow_mut().clear();

    // New offline work sorts before the old inventory cursor; resume cannot
    // use a timestamp/high-water mark that would omit it.
    seed(&ar, &[record(0, b"late earlier identity").unwrap()]).unwrap();
    let (mut a, mut b) = network.peers(&ar, &br);
    a.start().unwrap();
    b.start().unwrap();
    network.drain(&mut a, &mut b).unwrap();
    assert_eq!(b.progress().records, 1);
    assert_eq!(b.progress().blobs, 1);
    assert_converged(&ar, &br, 272);
}

#[test]
fn repeated_record_and_blob_receipts_do_not_duplicate_or_revive_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let mut replica = replica(directory.path());
    let original = blob_record(1, b"exact evidence", 14).unwrap();
    let conflict = record(1, b"conflicting evidence").unwrap();
    let mut snapshot = Snapshot::default();
    for _ in 0..3 {
        replica
            .ingest_records(
                &[original.clone(), original.clone(), conflict.clone()],
                &mut snapshot,
            )
            .unwrap();
        replica
            .ingest_blob(
                original.0,
                *blake3::hash(b"exact evidence").as_bytes(),
                b"exact evidence",
            )
            .unwrap();
    }
    let chain = CanonicalChain::read(directory.path()).unwrap();
    assert_eq!(chain.stats().records, 2);
    assert_eq!(chain.stats().quarantined, 2);
    assert_eq!(chain.stats().accepted, 0);
    assert_eq!(snapshot.len(), 2);
    let (_, blobs, _) = replica.into_parts();
    assert_eq!(blobs.len().unwrap(), 1);
}
