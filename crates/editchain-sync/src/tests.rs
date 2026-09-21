//! Real-store regression tests for the replication boundary.

// Return a failed test result without adding lint exceptions for panic in Result.
macro_rules! check {
    ($condition:expr_2021, $message:literal $(,)?) => {
        if !$condition {
            return Err(std::io::Error::other($message));
        }
    };
}

macro_rules! check_eq {
    ($actual:expr_2021, $expected:expr_2021, $message:literal $(,)?) => {
        let actual = $actual;
        let expected = $expected;
        if actual != expected {
            return Err(std::io::Error::other(format!(
                "{}: actual={actual:?}, expected={expected:?}",
                $message
            )));
        }
    };
}

mod inventory_tests;
mod provenance_tests;
mod secure_tests;
mod session_tests;

use std::io;
use std::path::Path;

use editchain_core::{
    ActorId, BlobRef, Clock, ContentId, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload,
    ScopeRef, Tags,
};
use editchain_store::durable::atomic_write;
use editchain_store::format::{encode_op, Page};
use editchain_store::{BlobStore, CanonicalChain, SegmentStore};

use crate::{
    encode_message, FrameDecoder, Message, RecordKey, Replica, INVENTORY_PAGE, MAX_FRAME_BYTES,
};

fn operation(seq: u64, payload: Payload) -> Op {
    Op {
        id: OpId::new(NodeId(7), 1, seq),
        parents: ParentSet::None,
        actor: ActorId(17),
        clock: Clock::UnixMs(123),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: payload,
            content_type: Payload::Empty,
        }),
    }
}

fn record(seq: u64, text: &[u8]) -> io::Result<(RecordKey, Vec<u8>)> {
    let bytes =
        encode_op(&operation(seq, Payload::Inline(text.to_vec()))).map_err(io::Error::other)?;
    Ok((RecordKey::from_encoded(&bytes)?, bytes))
}

fn blob_record(seq: u64, content: &[u8], length: u32) -> io::Result<(RecordKey, Vec<u8>)> {
    let bytes = encode_op(&operation(
        seq,
        Payload::Blob(BlobRef {
            id: ContentId::Hash256(*blake3::hash(content).as_bytes()),
            len: length,
        }),
    ))
    .map_err(io::Error::other)?;
    Ok((RecordKey::from_encoded(&bytes)?, bytes))
}

fn seed(path: &Path, records: &[(RecordKey, Vec<u8>)]) -> io::Result<()> {
    let mut store = SegmentStore::open(path)?;
    let mut page = Page::new(0);
    for (_, bytes) in records {
        page.add_record(0, bytes.clone());
    }
    store.append_page(&page)
}

fn reconcile(source: &Replica, target: &Replica) -> io::Result<()> {
    let snapshot = source.snapshot()?;
    let mut after = None;
    loop {
        let (keys, more) = snapshot.page(after);
        let records = keys
            .iter()
            .map(|key| {
                let bytes = snapshot
                    .record(*key)
                    .ok_or_else(|| io::Error::other("missing snapshot key"))?;
                Ok((*key, bytes.to_vec()))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let _acks = target.ingest_records(&records)?;
        if !more {
            return Ok(());
        }
        after = keys.last().copied();
    }
}

#[test]
fn independent_stores_converge_gaps_conflicts_replays_and_restart() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let a_root = dir.path().join("a");
    let b_root = dir.path().join("b");
    seed(
        &a_root,
        &[
            record(1, b"one")?,
            record(2, b"alice variant")?,
            record(5, b"five")?,
        ],
    )?;
    seed(
        &b_root,
        &[
            record(2, b"bob variant")?,
            record(3, b"three")?,
            record(4, b"four")?,
        ],
    )?;
    let a = Replica::open(&a_root, "space-1", true)?;
    let b = Replica::open(&b_root, "space-1", true)?;
    reconcile(&a, &b)?;
    reconcile(&b, &a)?;
    reconcile(&a, &b)?;
    let a_chain = CanonicalChain::read(&a_root)?;
    let b_chain = CanonicalChain::read(&b_root)?;
    check_eq!(
        a_chain.evidence(),
        b_chain.evidence(),
        "all conflict variants converge"
    );
    check_eq!(a_chain.stats().accepted, 4, "gap records become visible");
    check_eq!(
        a_chain.stats().quarantined,
        2,
        "both versions of ID 2 stay inert"
    );
    check_eq!(
        a_chain.stats().duplicates,
        0,
        "replayed bytes are not physically appended"
    );
    let restarted = Replica::open(&b_root, "space-1", false)?;
    check_eq!(
        restarted.snapshot()?.len(),
        6,
        "durable scope and evidence survive restart"
    );
    let c = Replica::open(&dir.path().join("c"), "space-1", true)?;
    reconcile(&restarted, &c)?;
    check_eq!(
        c.snapshot()?.len(),
        6,
        "a third replica receives quarantined evidence too"
    );
    Ok(())
}

#[test]
fn scope_inspection_is_read_only_and_rejects_damaged_bindings() -> io::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("chain");
    check_eq!(
        Replica::bound_space(&root)?,
        None,
        "unconfigured chain has no binding"
    );
    check!(!root.exists(), "inspection cannot create storage");
    let _replica = Replica::open(&root, "bound-space", false)?;
    let path = root.join("multiplayer/scope.json");
    let original = std::fs::read(&path)?;
    check_eq!(
        Replica::bound_space(&root)?,
        Some("bound-space".to_owned()),
        "binding is recoverable"
    );
    check_eq!(
        &std::fs::read(&path)?,
        &original,
        "inspection preserves the full ledger"
    );
    for (field, value) in [
        ("version", serde_json::json!(2)),
        ("space", serde_json::json!("")),
    ] {
        let mut damaged: serde_json::Value = serde_json::from_slice(&original)?;
        let target = damaged
            .get_mut(field)
            .ok_or_else(|| io::Error::other("fixture field"))?;
        *target = value;
        std::fs::write(&path, serde_json::to_vec(&damaged)?)?;
        check!(
            Replica::bound_space(&root).is_err(),
            "unsupported metadata cannot be mistaken for an unconfigured chain"
        );
    }
    std::fs::write(&path, b"incomplete")?;
    check!(
        Replica::bound_space(&root).is_err(),
        "corruption cannot mint a new space"
    );
    Ok(())
}

#[test]
fn new_history_scope_persists_and_backfill_is_explicit() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    seed(dir.path(), &[record(1, b"private history")?])?;
    let replica = Replica::open(dir.path(), "space-1", false)?;
    seed(dir.path(), &[record(2, b"shared new history")?])?;
    check_eq!(replica.snapshot()?.len(), 1, "old history stays excluded");
    drop(replica);
    let replica = Replica::open(dir.path(), "space-1", true)?;
    check_eq!(
        replica.snapshot()?.len(),
        1,
        "reconnecting cannot change prior consent"
    );
    replica.include_backfill()?;
    check_eq!(
        replica.snapshot()?.len(),
        2,
        "explicit backfill publishes old records"
    );
    check!(
        Replica::open(dir.path(), "other-space", true).is_err(),
        "a network space cannot replace the local binding"
    );
    Ok(())
}

#[test]
fn inventory_pagination_remains_stable_during_local_append() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let records = (1..=300)
        .map(|seq| record(seq, b"entry"))
        .collect::<io::Result<Vec<_>>>()?;
    seed(dir.path(), &records)?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let snapshot = replica.snapshot()?;
    let (first, more) = snapshot.page(None);
    check_eq!(first.len(), INVENTORY_PAGE, "inventory pages are bounded");
    check!(more, "remaining keys require another page");
    seed(dir.path(), &[record(301, b"appended later")?])?;
    let (second, more) = snapshot.page(first.last().copied());
    let (third, done) = snapshot.page(second.last().copied());
    check!(
        more && !done,
        "snapshot terminates after its original third page"
    );
    check_eq!(
        first
            .len()
            .saturating_add(second.len())
            .saturating_add(third.len()),
        300,
        "snapshot excludes later appends"
    );
    check_eq!(
        replica.snapshot()?.len(),
        301,
        "a later round includes the append"
    );
    Ok(())
}

#[test]
fn malformed_record_batches_do_not_write_or_acknowledge() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let valid = record(1, b"valid")?;
    let (key, mut corrupt) = record(2, b"invalid")?;
    corrupt.push(0);
    check!(
        replica.ingest_records(&[valid, (key, corrupt)]).is_err(),
        "trailing bytes reject the complete batch"
    );
    check!(
        replica.snapshot()?.is_empty(),
        "nothing becomes durable from a rejected batch"
    );
    let valid = record(1, b"valid")?;
    let lock = SegmentStore::open(dir.path())?;
    check!(
        replica
            .ingest_records(std::slice::from_ref(&valid))
            .is_err(),
        "busy writers cannot yield durable acknowledgments"
    );
    drop(lock);
    let acks = replica.ingest_records(std::slice::from_ref(&valid))?;
    check_eq!(
        acks,
        [valid.0],
        "retry after releasing the local writer succeeds"
    );
    drop(replica);
    check_eq!(
        CanonicalChain::read(dir.path())?.stats().accepted,
        1,
        "acknowledged record survives reopening"
    );
    Ok(())
}

#[test]
fn a_short_local_capture_transaction_delays_peer_publication_without_disconnect() -> io::Result<()>
{
    let dir = tempfile::tempdir()?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let local = SegmentStore::open(dir.path())?;
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(60));
        drop(local);
    });
    let entry = record(1, b"wait for local capture")?;
    let acks = replica.ingest_records(std::slice::from_ref(&entry))?;
    release
        .join()
        .map_err(|_error| io::Error::other("capture thread failed"))?;
    check_eq!(
        acks,
        [entry.0],
        "short writer contention is retried before an ack"
    );
    check_eq!(
        CanonicalChain::read(dir.path())?.stats().accepted,
        1,
        "delayed publication remains durable"
    );
    Ok(())
}

#[test]
fn remote_references_cannot_read_preexisting_private_blobs() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let private = b"private bytes absent from published operations";
    let hash = *blake3::hash(private).as_bytes();
    BlobStore::new(dir.path().join("blobs"))?.write(private)?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let foreign = blob_record(
        1,
        private,
        u32::try_from(private.len()).map_err(io::Error::other)?,
    )?;
    let _acks = replica.ingest_records(std::slice::from_ref(&foreign))?;
    check!(
        replica
            .blob_chunk(&replica.snapshot()?, foreign.0, hash, 0)?
            .is_none(),
        "knowing a hash does not grant private content access"
    );
    check!(
        replica
            .ingest_blob(foreign.0, hash, b"wrong content")
            .is_err(),
        "a claimed hash is verified"
    );
    replica.ingest_blob(foreign.0, hash, private)?;
    let chunk = replica.blob_chunk(&replica.snapshot()?, foreign.0, hash, 0)?;
    check_eq!(
        chunk.map(|(_, bytes)| bytes),
        Some(private.to_vec()),
        "content supplied through this space can be forwarded"
    );
    Ok(())
}

#[test]
fn blob_chunks_require_reference_length_and_complete_verified_bytes() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let content = vec![42; 140_000];
    let hash = *blake3::hash(&content).as_bytes();
    let correct = blob_record(1, &content, 140_000)?;
    let wrong_length = blob_record(2, &content, 1)?;
    seed(dir.path(), &[correct.clone(), wrong_length.clone()])?;
    BlobStore::new(dir.path().join("blobs"))?.write(&content)?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let snapshot = replica.snapshot()?;
    let chunk = replica.blob_chunk(&snapshot, correct.0, hash, 0)?;
    check_eq!(
        chunk.map(|(length, bytes)| (length, bytes.len())),
        Some((140_000, crate::CHUNK_BYTES)),
        "large blobs use bounded chunks"
    );
    check!(
        replica
            .blob_chunk(&snapshot, wrong_length.0, hash, 0)
            .is_err(),
        "declared lengths must match"
    );
    check!(
        replica
            .blob_chunk(&snapshot, correct.0, hash, 140_001)
            .is_err(),
        "offset beyond EOF is rejected"
    );
    check!(
        replica.ingest_blob(wrong_length.0, hash, &content).is_err(),
        "a receiver checks the reference length too"
    );
    Ok(())
}

#[test]
fn framing_accepts_fragmentation_and_rejects_oversize_truncation_and_trailing_bytes(
) -> io::Result<()> {
    let message = Message::Hello {
        version: 1,
        encoding: 1,
        space: "space-1".into(),
    };
    let frame = encode_message(&message)?;
    let mut decoder = FrameDecoder::default();
    let mut messages = Vec::new();
    for byte in &frame {
        messages.extend(decoder.push(&[*byte])?);
    }
    check_eq!(
        messages.as_slice(),
        std::slice::from_ref(&message),
        "every possible single-byte transport split is accepted"
    );
    decoder.finish()?;
    let mut decoder = FrameDecoder::default();
    let _messages = decoder.push(
        frame
            .get(..5)
            .ok_or_else(|| io::Error::other("short test frame"))?,
    )?;
    check!(
        decoder.finish().is_err(),
        "partial frames cannot become complete operations"
    );
    let oversized = u32::try_from(MAX_FRAME_BYTES.saturating_add(1))
        .map_err(io::Error::other)?
        .to_le_bytes();
    check!(
        FrameDecoder::default().push(&oversized).is_err(),
        "oversized headers fail before body allocation"
    );
    let mut payload = postcard::to_stdvec(&message).map_err(io::Error::other)?;
    payload.push(0);
    check!(
        crate::decode_message(&payload).is_err(),
        "future trailing fields cannot masquerade as a supported message"
    );
    Ok(())
}

#[test]
fn echoed_local_baseline_records_local_provenance_without_exposing_blobs() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let private = b"preexisting private baseline bytes";
    let baseline = blob_record(
        1,
        private,
        u32::try_from(private.len()).map_err(io::Error::other)?,
    )?;
    let hash = *blake3::hash(private).as_bytes();
    BlobStore::new(dir.path().join("blobs"))?.write(private)?;
    seed(dir.path(), std::slice::from_ref(&baseline))?;
    let foreign = blob_record(9, b"peer content", 12)?;
    let replica = Replica::open(dir.path(), "space-1", false)?;
    check_eq!(
        replica.snapshot()?.len(),
        0,
        "consent withholds the local baseline"
    );
    let acks = replica.ingest_records(&[baseline.clone(), foreign.clone()])?;
    check_eq!(
        acks,
        vec![baseline.0, foreign.0],
        "both independently supplied records are acknowledged"
    );
    let snapshot = replica.snapshot()?;
    check_eq!(
        snapshot.len(),
        2,
        "echoed baseline and foreign evidence are shareable"
    );
    check!(
        snapshot.contains(baseline.0) && snapshot.contains(foreign.0),
        "both records leave the withheld baseline"
    );
    check!(
        replica.read_blob(&snapshot, baseline.0, hash)?.is_none(),
        "preexisting private content stays unexportable"
    );
    let baseline_json = serde_json::to_value(baseline.0).map_err(io::Error::other)?;
    let foreign_json = serde_json::to_value(foreign.0).map_err(io::Error::other)?;
    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("multiplayer/scope.json"))?)?;
    let local = ledger
        .get("local")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| io::Error::other("missing local provenance set"))?;
    check_eq!(
        local.len(),
        1,
        "only the echoed baseline is locally authored"
    );
    check!(
        local.contains(&baseline_json),
        "the echoed baseline keeps its local provenance"
    );
    check!(
        !local.contains(&foreign_json),
        "a genuinely foreign record never becomes local provenance"
    );
    drop(replica);
    let replica = Replica::open(dir.path(), "space-1", false)?;
    let snapshot = replica.snapshot()?;
    check_eq!(snapshot.len(), 2, "scope decisions survive reopening");
    check!(
        replica.read_blob(&snapshot, baseline.0, hash)?.is_none(),
        "blob gating survives reopening"
    );
    replica.ingest_blob(baseline.0, hash, private)?;
    check_eq!(
        replica.read_blob(&replica.snapshot()?, baseline.0, hash)?,
        Some(private.to_vec()),
        "content supplied through this space can still be forwarded"
    );
    Ok(())
}

#[test]
fn retransmitted_foreign_records_never_gain_local_provenance() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let replica = Replica::open(dir.path(), "space-1", false)?;
    let foreign = blob_record(1, b"peer bytes", 10)?;
    let _acks = replica.ingest_records(std::slice::from_ref(&foreign))?;
    let _acks = replica.ingest_records(std::slice::from_ref(&foreign))?;
    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("multiplayer/scope.json"))?)?;
    check_eq!(
        ledger
            .get("received")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1),
        "retransmission keeps one receipt"
    );
    check_eq!(
        ledger
            .get("local")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0),
        "retransmission never invents local authorship"
    );
    Ok(())
}

#[test]
fn version_one_ledgers_without_local_provenance_still_load() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    seed(dir.path(), &[record(1, b"legacy history")?])?;
    std::fs::create_dir_all(dir.path().join("multiplayer"))?;
    atomic_write(
        &dir.path().join("multiplayer/scope.json"),
        &serde_json::to_vec(&serde_json::json!({
            "version": 1, "space": "space-1", "excluded": [], "received": [],
            "received_blobs": [],
        }))?,
    )?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    check_eq!(
        replica.snapshot()?.len(),
        1,
        "a version-1 ledger without local provenance still loads"
    );
    let entry = record(2, b"new shared history")?;
    let _acks = replica.ingest_records(std::slice::from_ref(&entry))?;
    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("multiplayer/scope.json"))?)?;
    check_eq!(
        ledger
            .get("local")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0),
        "no authorship is invented for legacy receipts"
    );
    Ok(())
}
