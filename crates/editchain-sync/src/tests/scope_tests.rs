//! Explicit consent changes use durable append boundaries, never wall clocks.

use super::*;
use crate::{Session, CHUNK_BYTES};

#[test]
fn selecting_from_now_replaces_all_history_and_can_be_changed_again() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let old = record(1, b"old history")?;
    seed(dir.path(), std::slice::from_ref(&old))?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let stale = Replica::open(dir.path(), "space-1", false)?;
    let before = stale.snapshot()?;
    let selected = replica.set_sharing_scope(false)?;
    check_eq!(
        selected.mode,
        "from_now",
        "explicit selection installs a cutoff"
    );
    check!(selected.cutoff_ms.is_some(), "the selected time is visible");
    check_eq!(
        replica.decoded_records(),
        0,
        "selecting a cutoff never scans old history"
    );
    check!(stale.snapshot().is_err(), "an older replica must reconnect");
    check!(
        replica.blob_hashes(&before, old.0).is_err(),
        "a frozen earlier snapshot loses authority"
    );
    check!(
        replica.snapshot()?.is_empty(),
        "earlier history is excluded"
    );
    // Its recorded clock is deliberately the same old clock as the first record.
    let later = record(2, b"newly appended, old timestamp")?;
    seed(dir.path(), &[old.clone(), later.clone()])?;
    let shared = replica.snapshot()?;
    check!(
        !shared.contains(old.0),
        "a duplicate cannot move old evidence past the cutoff"
    );
    check!(
        shared.contains(later.0),
        "append order controls scope despite the timestamp"
    );
    let reopened = Replica::open(dir.path(), "space-1", false)?;
    check_eq!(
        reopened.snapshot()?.len(),
        1,
        "reopening preserves the cutoff and backlog"
    );
    let saved =
        Replica::sharing_scope(dir.path())?.ok_or_else(|| io::Error::other("scope missing"))?;
    check_eq!(
        saved.cutoff_ms,
        selected.cutoff_ms,
        "restart preserves the selected time"
    );
    check_eq!(
        saved.revision,
        selected.revision,
        "restart does not change consent"
    );
    let all = replica.set_sharing_scope(true)?;
    check_eq!(all.mode, "all", "the user can opt back into history");
    check_eq!(
        replica.snapshot()?.len(),
        2,
        "existing records become eligible again"
    );
    let second = replica.set_sharing_scope(false)?;
    check!(
        second.revision > all.revision,
        "a new selection creates another boundary"
    );
    check!(
        replica.snapshot()?.is_empty(),
        "the new cutoff excludes the earlier shared backlog too"
    );
    let newest = record(3, b"after the replacement cutoff")?;
    seed(dir.path(), std::slice::from_ref(&newest))?;
    check!(
        replica.snapshot()?.contains(newest.0),
        "new records still synchronize"
    );
    check_eq!(
        CanonicalChain::read(dir.path())?.stats().accepted,
        3,
        "changing scope never deletes history"
    );
    Ok(())
}

#[test]
fn scope_change_invalidates_a_buffered_outgoing_content_transfer() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let bytes = vec![7; CHUNK_BYTES * 3];
    let entry = blob_record(
        1,
        &bytes,
        u32::try_from(bytes.len()).map_err(io::Error::other)?,
    )?;
    seed(dir.path(), std::slice::from_ref(&entry))?;
    BlobStore::new(dir.path().join("blobs"))?.write(&bytes)?;
    let hash = *blake3::hash(&bytes).as_bytes();
    let control = Replica::open(dir.path(), "space-1", true)?;
    let mut session = Session::new(Replica::open(dir.path(), "space-1", false)?);
    let _hello = session.receive(session.hello())?;
    let _page = session.receive(Message::Inventory { after: None })?;
    let first = session.receive(Message::Need {
        record: entry.0,
        blob: Some(hash),
        offset: 0,
    })?;
    check!(
        matches!(first.first(), Some(Message::Chunk { .. })),
        "an outgoing object is buffered"
    );
    let _scope = control.set_sharing_scope(false)?;
    check!(
        session
            .receive(Message::Need {
                record: entry.0,
                blob: Some(hash),
                offset: u32::try_from(CHUNK_BYTES).map_err(io::Error::other)?
            })
            .is_err(),
        "a queued request cannot continue the old transfer after the boundary changes"
    );
    check!(
        session.tick().is_err(),
        "idle turns also invalidate old workers"
    );
    check!(
        control.snapshot()?.is_empty(),
        "reconnecting offers no old records"
    );
    Ok(())
}

#[test]
fn independently_received_old_evidence_does_not_undo_a_new_cutoff() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let bytes = b"private earlier revision";
    let entry = blob_record(
        1,
        bytes,
        u32::try_from(bytes.len()).map_err(io::Error::other)?,
    )?;
    seed(dir.path(), std::slice::from_ref(&entry))?;
    BlobStore::new(dir.path().join("blobs"))?.write(bytes)?;
    let hash = *blake3::hash(bytes).as_bytes();
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let _scope = replica.set_sharing_scope(false)?;
    check!(
        replica.ingest_blob(entry.0, hash, bytes).is_err(),
        "a private record has not been independently received"
    );
    let mut receiving = replica.receiving_snapshot()?;
    let _acks = replica.ingest_into(std::slice::from_ref(&entry), Some(&mut receiving))?;
    check!(
        replica.read_blob(&receiving, entry.0, hash)?.is_none(),
        "a record receipt does not publish private content"
    );
    replica.ingest_blob(entry.0, hash, bytes)?;
    let receiving = replica.receiving_snapshot()?;
    check_eq!(
        replica.read_blob(&receiving, entry.0, hash)?,
        Some(bytes.to_vec()),
        "received content is usable locally"
    );
    check!(
        replica.snapshot()?.is_empty(),
        "receipts cannot move pre-cutoff evidence into an export inventory"
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("multiplayer/scope.json"))?)?;
    check_eq!(
        saved
            .get("local")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1),
        "existing local provenance survives a receipt"
    );
    let reopened = Replica::open(dir.path(), "space-1", false)?;
    check!(
        reopened.snapshot()?.is_empty(),
        "the outgoing cutoff survives restart"
    );
    check!(
        reopened.receiving_snapshot()?.contains(entry.0),
        "incoming checks do not redownload the same earlier receipt forever"
    );
    Ok(())
}

#[test]
fn an_interrupted_scope_change_fails_closed_and_explicit_selection_repairs_it() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    seed(dir.path(), &[record(1, b"old")?])?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    // The fence is durable before the consent ledger, including on a crash.
    atomic_write(
        &dir.path().join("multiplayer/scope-revision"),
        &1u64.to_le_bytes(),
    )?;
    check!(
        replica.snapshot().is_err(),
        "an existing worker cannot retain earlier consent"
    );
    check!(
        Replica::sharing_scope(dir.path())?.is_some_and(|scope| !scope.active),
        "status cannot present an interrupted change as active"
    );
    let reopened = Replica::open(dir.path(), "space-1", false)?;
    check!(
        reopened.snapshot().is_err(),
        "reopening cannot restore earlier consent"
    );
    let _repaired = reopened.set_sharing_scope(false)?;
    check!(
        reopened.snapshot()?.is_empty(),
        "an explicit retry repairs the ledger with a new cutoff"
    );
    check!(
        Replica::sharing_scope(dir.path())?.is_some(),
        "the repaired policy is observable"
    );
    Ok(())
}

#[test]
fn a_large_history_cutoff_is_constant_size_and_preserves_legacy_boundaries() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let old = (1..=20_000)
        .map(|seq| record(seq, b"old"))
        .collect::<io::Result<Vec<_>>>()?;
    seed(dir.path(), &old)?;
    let replica = Replica::open(dir.path(), "space-1", false)?;
    let legacy =
        Replica::sharing_scope(dir.path())?.ok_or_else(|| io::Error::other("scope missing"))?;
    check_eq!(
        legacy.mode,
        "legacy_from_now",
        "the existing baseline remains valid without a fabricated timestamp"
    );
    check_eq!(
        legacy.legacy_excluded_records,
        20_000,
        "the legacy summary reports exclusions"
    );
    let _scope = replica.set_sharing_scope(false)?;
    check!(
        std::fs::metadata(dir.path().join("multiplayer/scope.json"))?.len() < 1024,
        "a cutoff does not enumerate every old record in JSON"
    );
    check!(
        replica.snapshot()?.is_empty(),
        "all earlier evidence is still excluded"
    );
    Ok(())
}
