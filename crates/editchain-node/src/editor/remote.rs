//! Received source evidence already has its author's immutable derivations.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

use editchain_core::OpId;
use editchain_store::{read_encoded_at, IndexedChain};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Receipt {
    id: OpId,
    digest: [u8; 32],
}

/// Read only the versioned receipt fields of the multiplayer scope ledger.
/// Its other fields remain owned by the replication writer.
#[derive(Debug, Default, Deserialize)]
pub(super) struct Receipts {
    version: u16,
    received: BTreeSet<Receipt>,
}

impl Receipts {
    pub(super) fn read(root: &Path) -> io::Result<Self> {
        let file = match File::open(root.join("multiplayer/scope.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > 128 * 1024 * 1024 {
            return Err(io::Error::other("replication receipts exceed limit"));
        }
        let mut bytes = Vec::new();
        let _: usize = file.take(128 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        let receipts: Self = serde_json::from_slice(&bytes)?;
        if receipts.version != 1 {
            return Err(io::Error::other("unsupported replication receipt version"));
        }
        Ok(receipts)
    }

    pub(super) fn foreign(&self, chain: &IndexedChain, root: &Path, id: OpId) -> io::Result<bool> {
        if self
            .received
            .range(
                Receipt {
                    id,
                    digest: [0; 32],
                }..=Receipt {
                    id,
                    digest: [255; 32],
                },
            )
            .next()
            .is_none()
        {
            return Ok(false);
        }
        let mut found = false;
        for location in chain.evidence_locations(id) {
            found = true;
            let digest = *blake3::hash(&read_encoded_at(root, location)?).as_bytes();
            if !self.received.contains(&Receipt { id, digest }) {
                return Ok(false);
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{event_op, record, Encoding};
    use editchain_protocol::editor::{EditorEvent, RecordEditorEvents};
    use editchain_store::{
        durable::atomic_write,
        format::{encode_op, Page},
        CanonicalChain, SegmentStore,
    };
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
