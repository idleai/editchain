//! Received source evidence already has its author's immutable derivations.

use std::io;
use std::path::Path;

use crate::receipts::Receipts;
use editchain_core::{Op, OpId};
use editchain_store::{
    format::{decode_op, encode_op},
    read_encoded_at, IndexedChain,
};

/// Recover the exact local admission for retries and sequence checks only.
/// This does not restore a quarantined operation to the canonical projection.
pub(super) fn retained_source(
    chain: &IndexedChain,
    root: &Path,
    id: OpId,
) -> io::Result<Option<Op>> {
    if let Some(op) = chain.get(id) {
        return Ok(Some(op.clone()));
    }
    let locations: Vec<_> = chain.evidence_locations(id).collect();
    if locations.is_empty() {
        return Ok(None);
    }
    let receipts = Receipts::read(root)?;
    let mut local = None;
    for location in locations {
        let encoded = read_encoded_at(root, location)?;
        let digest = *blake3::hash(&encoded).as_bytes();
        if receipts.locally_retained(id, digest) {
            // Without a unique local admission there is no safe retry parent.
            if local.is_some() {
                return Ok(None);
            }
            local = Some(decode_op(&encoded).map_err(io::Error::other)?);
        }
    }
    Ok(local)
}

/// An exact retry can acknowledge already durable bytes even when every
/// variant has a received receipt. The event must match beyond just its ID.
pub(super) fn retry_source(
    chain: &IndexedChain,
    root: &Path,
    expected: &Op,
) -> io::Result<Option<Op>> {
    if let Some(local) = retained_source(chain, root, expected.id)? {
        return Ok(Some(local));
    }
    for location in chain.evidence_locations(expected.id) {
        let encoded = read_encoded_at(root, location)?;
        let retained = decode_op(&encoded).map_err(io::Error::other)?;
        let mut retry = expected.clone();
        retry.parents = retained.parents.clone();
        if encode_op(&retry).map_err(io::Error::other)? == encoded {
            return Ok(Some(retained));
        }
    }
    Ok(None)
}

/// Quarantine of content need not invent a recorder sequence gap when all
/// retained variants still agree on the predecessor's actor and stream.
pub(super) fn predecessor_matches(
    chain: &IndexedChain,
    root: &Path,
    id: OpId,
    next: &Op,
) -> io::Result<bool> {
    let mut found = false;
    for location in chain.evidence_locations(id) {
        found = true;
        let previous = decode_op(&read_encoded_at(root, location)?).map_err(io::Error::other)?;
        if previous.actor != next.actor || previous.scope != next.scope {
            return Ok(false);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{event_op, record, Encoding};
    use editchain_protocol::editor::{EditorEvent, RecordEditorEvents};
    use editchain_store::{durable::atomic_write, format::Page, CanonicalChain, SegmentStore};
    use serde_json::json;

    fn check(condition: bool, message: &'static str) -> super::super::Result<()> {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn started(session: &str) -> super::super::Result<EditorEvent> {
        Ok(serde_json::from_value(
            json!({ "schema": 1, "session": session, "sequence": 1, "time_ms": 10,
            "event": { "type": "tracking_started", "dwell_ms": 2000, "vscode_version": "1.137.0" } }),
        )?)
    }

    #[test]
    fn received_sources_and_their_conflicts_do_not_block_or_rederive_local_capture(
    ) -> super::super::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".editchain");
        let remote = started("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")?;
        let raw = serde_json::to_vec(&json!({ "source": "vscode.editor", "event": remote }))?;
        let foreign = event_op(&remote, &raw)?;
        let encoded = encode_op(&foreign)?;
        let mut page = Page::new(0);
        page.add_record(0, encoded.clone());
        SegmentStore::open(&root)?.append_page(&page)?;
        std::fs::create_dir_all(root.join("multiplayer"))?;
        let first = json!({ "id": foreign.id, "digest": blake3::hash(&encoded).as_bytes() });
        let ledger = root.join("multiplayer/scope.json");
        atomic_write(
            &ledger,
            &serde_json::to_vec(&json!({ "version": 1, "received": [first] }))?,
        )?;
        let local = started("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")?;
        let request = RecordEditorEvents {
            workspace_path: dir.path().to_string_lossy().into_owned(),
            chain_dir: ".editchain".into(),
            events: vec![local],
        };
        let result = record(&request, &mut Encoding::default())?;
        check(
            result.get("accepted") == Some(&json!(1)),
            "missing remote blob cannot block local recording",
        )?;
        let chain = CanonicalChain::read(&root)?;
        check(
            chain
                .located_ops()
                .filter(|(op, _)| op.id.node == foreign.id.node)
                .count()
                == 1,
            "remote raw evidence remains unchanged",
        )?;
        let mut conflict = foreign.clone();
        conflict.clock = editchain_core::Clock::UnixMs(11);
        let encoded = encode_op(&conflict)?;
        let second = json!({ "id": conflict.id, "digest": blake3::hash(&encoded).as_bytes() });
        atomic_write(
            &ledger,
            &serde_json::to_vec(&json!({ "version": 1, "received": [first, second] }))?,
        )?;
        let mut page = Page::new(0);
        page.add_record(0, encoded);
        SegmentStore::open(&root)?.append_page(&page)?;
        let result = record(&request, &mut Encoding::default())?;
        check(
            result.get("replayed") == Some(&json!(1)),
            "foreign conflict does not poison local recorder checkpoint",
        )?;
        check(
            CanonicalChain::read(&root)?.stats().quarantined == 2,
            "foreign conflict still quarantines its exact variants",
        )?;
        // An unknown local variant cannot be mislabeled as received merely by ID.
        let indexed = editchain_store::IndexedTail::open(&root)?;
        atomic_write(
            &ledger,
            &serde_json::to_vec(&json!({ "version": 1, "received": [first] }))?,
        )?;
        check(
            !Receipts::read(&root)?.foreign(indexed.chain(), &root, foreign.id)?,
            "receipt matching checks every exact variant",
        )
    }
}
