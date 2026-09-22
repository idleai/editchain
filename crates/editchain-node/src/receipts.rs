//! Exact receipt provenance shared by capture and the live history view.

use editchain_core::OpId;
use editchain_store::{read_encoded_at, IndexedChain};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Receipt {
    id: OpId,
    digest: [u8; 32],
}

/// Read only the versioned receipt fields of the multiplayer scope ledger.
/// Its other fields remain owned by the replication writer.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Receipts {
    version: u16,
    received: BTreeSet<Receipt>,
    /// Exact records retained here before a peer independently supplied the
    /// same bytes. Such a variant also has a received receipt, so authorship
    /// must be read from this set rather than inferred from the receipt.
    ///
    /// Version-1 ledgers written before this field existed default to empty;
    /// their ambiguous receipts are never retroactively treated as local.
    #[serde(default)]
    local: BTreeSet<Receipt>,
}

impl Receipts {
    pub(crate) fn read(root: &Path) -> io::Result<Self> {
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

    /// Whether this exact variant is known to have been retained locally,
    /// independent of any received receipt. Only bytes that were never
    /// supplied by a peer, or were held before a peer supplied them, are
    /// evidence of local authorship.
    pub(crate) fn locally_retained(&self, id: OpId, digest: [u8; 32]) -> bool {
        let receipt = Receipt { id, digest };
        self.local.contains(&receipt) || !self.received.contains(&receipt)
    }

    pub(crate) fn foreign(&self, chain: &IndexedChain, root: &Path, id: OpId) -> io::Result<bool> {
        let any_receipt = |set: &BTreeSet<Receipt>| {
            set.range(
                Receipt {
                    id,
                    digest: [0; 32],
                }..=Receipt {
                    id,
                    digest: [255; 32],
                },
            )
            .next()
            .is_some()
        };
        if !any_receipt(&self.received) && !any_receipt(&self.local) {
            return Ok(false);
        }
        let mut found = false;
        for location in chain.evidence_locations(id) {
            found = true;
            let digest = *blake3::hash(&read_encoded_at(root, location)?).as_bytes();
            // A locally retained variant, including a baseline a peer later
            // supplied exactly, must still be derived on this device.
            if self.locally_retained(id, digest) {
                return Ok(false);
            }
        }
        Ok(found)
    }
}
