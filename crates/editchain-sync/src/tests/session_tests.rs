//! Exercise complete peers over fragmented frames and interruptible delivery.

use std::collections::VecDeque;

use super::*;
use crate::Session;

mod ancestry_tests;
mod progress_tests;

type Queue = VecDeque<(bool, Message)>;
type EncodedRecord = (RecordKey, Vec<u8>);

fn session(root: &Path) -> io::Result<Session> {
    Ok(Session::new(Replica::open(root, "space-1", true)?))
}

fn start(a: &Session, b: &Session) -> Queue {
    [(true, b.hello()), (false, a.hello())].into()
}

fn deliver(a: &mut Session, b: &mut Session, queue: &mut Queue) -> io::Result<()> {
    let (to_a, message) = queue
        .pop_front()
        .ok_or_else(|| io::Error::other("empty wire"))?;
    let peer = if to_a { a } else { b };
    let mut decoder = FrameDecoder::default();
    for fragment in encode_message(&message)?.chunks(997) {
        for message in decoder.push(fragment)? {
            queue.extend(
                peer.receive(message)?
                    .into_iter()
                    .map(|reply| (!to_a, reply)),
            );
        }
    }
    decoder.finish()
}

fn drain(a: &mut Session, b: &mut Session, queue: &mut Queue) -> io::Result<()> {
    for _ in 0..20_000 {
        if queue.is_empty() {
            return Ok(());
        }
        deliver(a, b, queue)?;
    }
    Err(io::Error::other("peer round did not terminate"))
}

fn work(seq: u64, hash: [u8; 32], external: bool) -> io::Result<(EncodedRecord, Vec<u8>)> {
    use editchain_core::human::{HumanRevision, HumanWorkKind, HumanWorkRecord};
    let work = HumanWorkRecord {
        source: "vscode.work".into(),
        schema: 1,
        session: "portable-session".into(),
        identity: None,
        user_name: None,
        turn: 1,
        edit_group: None,
        source_event: OpId::new(NodeId(7), 1, 1),
        kind: HumanWorkKind::Read,
        path: Some("src/shared.rs".into()),
        before: None,
        after: Some(HumanRevision {
            document: "buffer".into(),
            version: 1,
            content: ContentId::Hash256(hash),
            occurrence: None,
        }),
        git: None,
        context_observed_ms: None,
        summary: "Read shared source".into(),
    };
    let content = serde_json::to_vec(&work).map_err(io::Error::other)?;
    let raw_ref = if external {
        Payload::Blob(BlobRef {
            id: ContentId::Hash256(*blake3::hash(&content).as_bytes()),
            len: u32::try_from(content.len()).map_err(io::Error::other)?,
        })
    } else {
        Payload::Inline(content.clone())
    };
    let mut op = operation(seq, Payload::Empty);
    op.kind = OpKind::Import(editchain_core::ImportOp {
        raw_ref,
        raw_hash: None,
    });
    let bytes = encode_op(&op).map_err(io::Error::other)?;
    Ok(((RecordKey::from_encoded(&bytes)?, bytes), content))
}

#[test]
fn complete_peers_converge_pages_conflicts_large_records_and_nested_content() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    let content = vec![71; 190_000];
    let hash = *blake3::hash(&content).as_bytes();
    let (work, metadata) = work(300, hash, true)?;
    let mut records = (1..=270)
        .map(|seq| record(seq, b"alice"))
        .collect::<io::Result<Vec<_>>>()?;
    records.extend([record(299, &vec![37; 200_000])?, work.clone()]);
    seed(&ar, &records)?;
    seed(&br, &[record(9, b"conflict")?, record(280, b"bob")?])?;
    let mut blobs = BlobStore::new(ar.join("blobs"))?;
    blobs.write(&metadata)?;
    blobs.write(&content)?;
    let mut a = session(&ar)?;
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    drain(&mut a, &mut b, &mut queue)?;
    check_eq!(a.progress().rounds, 1, "alice completed an inventory");
    check_eq!(b.progress().rounds, 1, "bob completed an inventory");
    check_eq!(
        b.progress().blobs,
        2,
        "structured metadata and nested revision hydrated"
    );
    check_eq!(b.progress().unavailable, 0, "complete content available");
    check_eq!(
        a.progress().sent_records,
        b.progress().records,
        "alice's sends match bob's durable receipts"
    );
    check_eq!(
        b.progress().sent_records,
        a.progress().records,
        "bob's sends match alice's durable receipts"
    );
    check_eq!(
        a.progress().sent_blobs,
        b.progress().blobs,
        "content is counted after remote acknowledgment"
    );
    let ac = CanonicalChain::read(&ar)?;
    let bc = CanonicalChain::read(&br)?;
    check_eq!(
        ac.evidence().evidence().collect::<Vec<_>>(),
        bc.evidence().evidence().collect::<Vec<_>>(),
        "same gaps and exact conflict evidence"
    );
    check_eq!(
        bc.stats().quarantined,
        2,
        "both variants of the conflicted identity are quarantined"
    );
    let replica = Replica::open(&br, "space-1", true)?;
    check_eq!(
        replica.read_blob(&replica.snapshot()?, work.0, hash)?,
        Some(content),
        "historical revision bytes available"
    );
    // Source goes offline. A third replica obtains original evidence and content from B.
    drop(a);
    let cr = dir.path().join("c");
    let mut b = session(&br)?;
    let mut c = session(&cr)?;
    let mut queue = start(&b, &c);
    drain(&mut b, &mut c, &mut queue)?;
    let cc = CanonicalChain::read(&cr)?;
    check_eq!(
        cc.evidence().evidence().collect::<Vec<_>>(),
        bc.evidence().evidence().collect::<Vec<_>>(),
        "third-party exact evidence forwarding"
    );
    check_eq!(
        c.progress().blobs,
        2,
        "third-party nested content forwarding"
    );
    Ok(())
}

#[test]
fn disconnect_mid_blob_repairs_from_disk_and_later_ticks_follow_offline_work() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    let bytes = vec![31; 190_000];
    seed(&ar, &[blob_record(1, &bytes, 190_000)?])?;
    BlobStore::new(ar.join("blobs"))?.write(&bytes)?;
    let mut a = session(&ar)?;
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    for _ in 0..100 {
        if queue.iter().any(|(_, message)| matches!(message, Message::Chunk { blob: Some(_), offset, .. } if *offset > 0)) { break; }
        deliver(&mut a, &mut b, &mut queue)?;
    }
    check_eq!(b.progress().records, 1, "record durable before content");
    check_eq!(b.progress().blobs, 0, "partial content unacknowledged");
    check_eq!(
        a.progress().sent_blobs,
        0,
        "partial content is not a completed send"
    );
    drop((a, b, queue));
    let mut a = session(&ar)?;
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    drain(&mut a, &mut b, &mut queue)?;
    check_eq!(
        b.progress().records,
        0,
        "durable record needs no retransmission"
    );
    check_eq!(
        b.progress().blobs,
        1,
        "missing content repaired after restart"
    );
    seed(&br, &[record(3, b"offline contribution")?])?;
    queue.extend(a.tick()?.into_iter().map(|message| (false, message)));
    queue.extend(b.tick()?.into_iter().map(|message| (true, message)));
    drain(&mut a, &mut b, &mut queue)?;
    check_eq!(
        CanonicalChain::read(&ar)?.stats().accepted,
        2,
        "new local work reaches other replica"
    );
    Ok(())
}

#[test]
fn lost_ack_is_repaired_without_a_duplicate_physical_record() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    seed(&ar, &[record(1, b"durable")?])?;
    let mut a = session(&ar)?;
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    for _ in 0..100 {
        if queue
            .iter()
            .any(|(_, message)| matches!(message, Message::Ack { blob: None, .. }))
        {
            break;
        }
        deliver(&mut a, &mut b, &mut queue)?;
    }
    check!(
        queue
            .iter()
            .any(|(_, message)| matches!(message, Message::Ack { .. })),
        "connection interrupted before ack delivery"
    );
    check_eq!(
        a.progress().sent_records,
        0,
        "unacknowledged sends are not reported as saved remotely"
    );
    check_eq!(
        CanonicalChain::read(&br)?.stats().accepted,
        1,
        "ack follows disk persistence"
    );
    drop((a, b, queue));
    let mut a = session(&ar)?;
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    drain(&mut a, &mut b, &mut queue)?;
    check_eq!(
        CanonicalChain::read(&br)?.stats().duplicates,
        0,
        "reconnect uses durable inventory"
    );
    Ok(())
}

#[test]
fn mismatched_hello_and_unsolicited_or_corrupt_transfers_fail_closed() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    for message in [
        Message::Hello {
            version: 1,
            encoding: 1,
            space: "space-1".into(),
        },
        Message::Hello {
            version: crate::PEER_VERSION,
            encoding: 1,
            space: "other-space".into(),
        },
        Message::Inventory { after: None },
    ] {
        let mut peer = session(dir.path())?;
        check!(
            peer.receive(message).is_err(),
            "no inventory before matching hello"
        );
        check!(!peer.progress().accepted, "negotiation rejected");
    }
    let (key, bytes) = record(1, b"only exact bytes")?;
    for mode in 0..4 {
        let mut peer = session(dir.path())?;
        let _replies = peer.receive(peer.hello())?;
        if mode == 0 {
            check!(
                peer.receive(Message::Page {
                    offset: 0,
                    total: u64::try_from(INVENTORY_PAGE.saturating_add(1))
                        .map_err(io::Error::other)?,
                    records: vec![key; INVENTORY_PAGE.saturating_add(1)],
                    more: false
                })
                .is_err(),
                "page bound enforced"
            );
        } else {
            let _replies = peer.receive(Message::Page {
                offset: 0,
                total: 1,
                records: vec![key],
                more: false,
            })?;
            let mut payload = bytes.clone();
            if mode == 3 {
                payload.push(0);
            }
            let message = Message::Chunk {
                record: key,
                blob: None,
                offset: u32::from(mode == 1),
                total: if mode == 2 {
                    u32::MAX
                } else {
                    u32::try_from(payload.len()).map_err(io::Error::other)?
                },
                bytes: payload,
            };
            check!(
                peer.receive(message).is_err(),
                "offset, limit or digest failure closes the session"
            );
        }
    }
    check_eq!(
        CanonicalChain::read(dir.path())?.stats().accepted,
        0,
        "invalid records never published"
    );
    Ok(())
}

#[test]
fn independent_receipt_of_a_private_baseline_does_not_authorize_its_blob() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let private = b"existing private revision";
    let entry = blob_record(
        1,
        private,
        u32::try_from(private.len()).map_err(io::Error::other)?,
    )?;
    seed(dir.path(), std::slice::from_ref(&entry))?;
    BlobStore::new(dir.path().join("blobs"))?.write(private)?;
    let replica = Replica::open(dir.path(), "space-1", false)?;
    let _acks = replica.ingest_records(std::slice::from_ref(&entry))?;
    let snapshot = replica.snapshot()?;
    check_eq!(
        snapshot.len(),
        1,
        "independently supplied evidence can be forwarded"
    );
    check!(
        replica
            .read_blob(&snapshot, entry.0, *blake3::hash(private).as_bytes())?
            .is_none(),
        "preexisting content remains private until supplied"
    );
    Ok(())
}

#[test]
fn structured_content_requires_the_known_schema_and_verified_parent() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let bytes = b"private nested revision";
    let hash = *blake3::hash(bytes).as_bytes();
    let (entry, metadata) = work(1, hash, true)?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let mut store = BlobStore::new(dir.path().join("blobs"))?;
    store.write(&metadata)?;
    store.write(bytes)?;
    let _acks = replica.ingest_records(std::slice::from_ref(&entry))?;
    check!(
        !replica
            .blob_hashes(&replica.snapshot()?, entry.0)?
            .contains(&hash),
        "unsupplied private parent cannot grant child authority"
    );
    replica.ingest_blob(entry.0, *blake3::hash(&metadata).as_bytes(), &metadata)?;
    check!(
        replica
            .blob_hashes(&replica.snapshot()?, entry.0)?
            .contains(&hash),
        "verified known schema grants a reference"
    );
    check!(
        replica
            .read_blob(&replica.snapshot()?, entry.0, hash)?
            .is_none(),
        "reference alone never exposes private child bytes"
    );
    let mut future: serde_json::Value =
        serde_json::from_slice(&metadata).map_err(io::Error::other)?;
    *future
        .get_mut("schema")
        .ok_or_else(|| io::Error::other("missing schema"))? = 2.into();
    let mut op = operation(2, Payload::Empty);
    op.kind = OpKind::Import(editchain_core::ImportOp {
        raw_ref: Payload::Inline(serde_json::to_vec(&future).map_err(io::Error::other)?),
        raw_hash: None,
    });
    check!(
        crate::content::hashes(&op).is_empty(),
        "future schema never grants unknown references"
    );
    Ok(())
}

#[test]
fn writer_contention_cannot_produce_an_acknowledgment() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut peer = session(dir.path())?;
    let _replies = peer.receive(peer.hello())?;
    let (key, bytes) = record(1, b"waiting for writer")?;
    let _replies = peer.receive(Message::Page {
        offset: 0,
        total: 1,
        records: vec![key],
        more: false,
    })?;
    let writer = SegmentStore::open(dir.path())?;
    let message = Message::Chunk {
        record: key,
        blob: None,
        offset: 0,
        total: u32::try_from(bytes.len()).map_err(io::Error::other)?,
        bytes,
    };
    check!(
        peer.receive(message.clone()).is_err(),
        "busy durable publication returns an error, no ack"
    );
    check_eq!(peer.progress().records, 0, "no false durable progress");
    drop((writer, peer));
    let mut peer = session(dir.path())?;
    let _replies = peer.receive(peer.hello())?;
    let _replies = peer.receive(Message::Page {
        offset: 0,
        total: 1,
        records: vec![key],
        more: false,
    })?;
    check!(
        peer.receive(message)?
            .iter()
            .any(|m| matches!(m, Message::Ack { .. })),
        "fresh connection repairs after contention"
    );
    Ok(())
}

#[test]
fn missing_content_is_reported_and_a_later_round_repairs_it() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    seed(&ar, &[blob_record(1, b"late", 4)?])?;
    let mut a = session(&ar)?;
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    drain(&mut a, &mut b, &mut queue)?;
    check_eq!(
        b.progress().unavailable,
        1,
        "inventory completion does not claim complete content"
    );
    BlobStore::new(ar.join("blobs"))?.write(b"late")?;
    queue.extend(b.tick()?.into_iter().map(|message| (true, message)));
    drain(&mut a, &mut b, &mut queue)?;
    check_eq!(
        b.progress().unavailable,
        0,
        "fresh source snapshot repairs missing content"
    );
    check_eq!(b.progress().blobs, 1, "late bytes persisted");
    Ok(())
}
