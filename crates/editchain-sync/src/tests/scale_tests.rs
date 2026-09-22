//! Keep a large local corpus from making each small receipt a full replay.

use super::*;
use std::io::Write as _;
use std::time::Instant;

#[test]
fn small_receipts_over_a_large_history_keep_exact_bytes_and_measure_local_work() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let old = (1..=20_000)
        .map(|seq| record(seq, &vec![42; 512]))
        .collect::<io::Result<Vec<_>>>()?;
    seed(dir.path(), &old)?;
    let blobs = (20_001..=20_024)
        .map(|seq| {
            let bytes = format!("content for {seq}").into_bytes();
            let record = blob_record(
                seq,
                &bytes,
                u32::try_from(bytes.len()).map_err(io::Error::other)?,
            )?;
            Ok((record, bytes))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let start = Instant::now();
    let snapshot = replica.snapshot()?;
    check_eq!(
        replica.decoded_records(),
        20_000,
        "cold inventory reads each record once"
    );
    let initial_ms = start.elapsed().as_millis();
    let start = Instant::now();
    let entries = blobs
        .iter()
        .map(|(entry, _)| entry.clone())
        .collect::<Vec<_>>();
    let acks = replica.ingest_records(&entries)?;
    let batch_ms = start.elapsed().as_millis();
    let start = Instant::now();
    for ((key, _), bytes) in &blobs {
        replica.ingest_blob(*key, *blake3::hash(bytes).as_bytes(), bytes)?;
    }
    let blobs_ms = start.elapsed().as_millis();
    let start = Instant::now();
    let complete = replica.snapshot()?;
    let refresh_ms = start.elapsed().as_millis();
    check_eq!(
        replica.decoded_records(),
        20_024,
        "24 content receipts never replay the old corpus"
    );
    check_eq!(acks.len(), 24, "all receipts are durable");
    check_eq!(
        snapshot.len(),
        20_000,
        "the frozen outgoing inventory stays unchanged"
    );
    check_eq!(
        complete.len(),
        20_024,
        "later inventories include newly received records"
    );
    for ((key, _), bytes) in &blobs {
        check_eq!(
            replica.read_blob(&complete, *key, *blake3::hash(bytes).as_bytes())?,
            Some(bytes.clone()),
            "received content retains exact bytes"
        );
    }
    let first = old
        .first()
        .ok_or_else(|| io::Error::other("missing old record"))?
        .0;
    check_eq!(
        snapshot.record(first).map(<[u8]>::as_ptr),
        complete.record(first).map(<[u8]>::as_ptr),
        "frozen inventories share immutable bytes rather than copying history"
    );
    let late = record(20_025, b"concurrent local append")?;
    seed(dir.path(), std::slice::from_ref(&late))?;
    check!(
        replica.snapshot()?.contains(late.0),
        "the retained reader follows another writer"
    );
    for _ in 0..3 {
        let _snapshot = replica.snapshot()?;
    }
    check_eq!(
        replica.decoded_records(),
        20_025,
        "unchanged rounds do not replay sealed history"
    );
    writeln!(io::stderr().lock(), "replica scale: initial_ms={initial_ms} batch_ms={batch_ms} blobs_ms={blobs_ms} refresh_ms={refresh_ms} records=20000 blobs=24")?;
    Ok(())
}

#[test]
fn receipt_page_keeps_local_and_foreign_provenance_after_other_writers_append() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let baseline = blob_record(1, b"old", 3)?;
    let local = blob_record(2, b"local", 5)?;
    let foreign = blob_record(3, b"peer", 4)?;
    let conflict = record(3, b"conflicting peer variant")?;
    seed(dir.path(), std::slice::from_ref(&baseline))?;
    let mut store = BlobStore::new(dir.path().join("blobs"))?;
    for bytes in [b"old".as_slice(), b"local", b"peer"] {
        store.write(bytes)?;
    }
    let replica = Replica::open(dir.path(), "space-1", false)?;
    let mut incoming = replica.snapshot()?;
    let outgoing = replica.snapshot()?;
    seed(dir.path(), std::slice::from_ref(&local))?;
    let other = Replica::open(dir.path(), "space-1", true)?;
    let _acks = other.ingest_records(std::slice::from_ref(&foreign))?;
    let batch = [
        baseline.clone(),
        local.clone(),
        foreign.clone(),
        conflict.clone(),
        conflict,
    ];
    let acks = replica.ingest_into(&batch, Some(&mut incoming))?;
    check_eq!(
        acks.len(),
        batch.len(),
        "including retransmissions, every returned ack is durable"
    );
    check_eq!(
        incoming.len(),
        4,
        "receipt page updates the incoming view with both conflict variants"
    );
    check!(
        outgoing.is_empty(),
        "the outgoing round keeps its original consent-filtered inventory"
    );
    for (entry, bytes, allowed) in [
        (&baseline, b"old".as_slice(), false),
        (&local, b"local", true),
        (&foreign, b"peer", false),
    ] {
        check_eq!(
            replica
                .read_blob(&incoming, entry.0, *blake3::hash(bytes).as_bytes())?
                .is_some(),
            allowed,
            "the latest ledger preserves the source of each exact variant"
        );
    }
    let complete = replica.snapshot()?;
    check_eq!(
        complete.len(),
        4,
        "later inventories retain all distinct evidence"
    );
    check_eq!(
        replica.decoded_records(),
        4,
        "only appended records were decoded"
    );
    let chain = CanonicalChain::read(dir.path())?;
    check_eq!(
        chain.stats().quarantined,
        2,
        "conflicting identities remain inert"
    );
    check_eq!(
        chain.stats().duplicates,
        0,
        "retransmissions are not physically appended"
    );
    let restarted = Replica::open(dir.path(), "space-1", true)?;
    check_eq!(
        restarted.snapshot()?.len(),
        4,
        "restart derives the same exact evidence from disk"
    );
    Ok(())
}

#[test]
fn retained_evidence_rejects_sealed_replacement_and_active_truncation_before_ack() -> io::Result<()>
{
    for sealed in [true, false] {
        let dir = tempfile::tempdir()?;
        let entry = blob_record(1, b"blob", 4)?;
        seed(dir.path(), std::slice::from_ref(&entry))?;
        seed(dir.path(), &[record(2, b"active segment")?])?;
        let replica = Replica::open(dir.path(), "space-1", true)?;
        let _snapshot = replica.snapshot()?;
        let scope = std::fs::read(dir.path().join("multiplayer/scope.json"))?;
        if sealed {
            let path = dir.path().join("000000.eclog");
            let bytes = std::fs::read(&path)?;
            std::fs::rename(&path, dir.path().join("replaced"))?;
            std::fs::write(&path, bytes)?;
        } else {
            std::fs::write(dir.path().join("000001.eclog"), b"")?;
        }
        check!(
            replica.snapshot().is_err(),
            "a changed source invalidates the retained reader"
        );
        check!(
            replica
                .ingest_records(&[record(3, b"must not acknowledge")?])
                .is_err(),
            "records cannot be acknowledged against invalid evidence"
        );
        check!(
            replica
                .ingest_blob(entry.0, *blake3::hash(b"blob").as_bytes(), b"blob")
                .is_err(),
            "content cannot be acknowledged against invalid evidence"
        );
        check_eq!(
            std::fs::read(dir.path().join("multiplayer/scope.json"))?,
            scope,
            "invalid source leaves the durable receipt ledger untouched"
        );
    }
    Ok(())
}
