//! Portable evidence archives preserve encoded operations, conflicts and blobs.

use std::{collections::BTreeSet, path::Path};

use editchain_engine::{Admission, BlobRef, ChainSnapshot, ChainWriter, ContentId, Engine, OpId};
use editchain_sync::ReplicationStorage;
use serde::{Deserialize, Serialize};

use super::{
    error::{Failure, Result},
    input, operations,
    output::Output,
    replication, require_chain,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Entry {
    Header {
        version: u32,
    },
    Operation {
        encoded: Vec<u8>,
    },
    Blob {
        reference: BlobRef,
        bytes: Vec<u8>,
    },
    MissingBlob {
        id: ContentId,
    },
    Summary {
        stats: editchain_engine::ChainReadStats,
    },
}

pub(super) fn export(chain: &Path, output: &mut Output) -> Result<()> {
    require_chain(chain)?;
    let replica = replication::storage(chain, editchain_sync::ExportScope::all("archive")?)?;
    let source = ChainSnapshot::read(chain)?;
    let stats = source.stats();
    let mut snapshot = editchain_sync::Snapshot::default();
    for (_, encoded) in source.evidence().evidence() {
        let _key = snapshot.insert_encoded(encoded.to_vec())?;
    }
    output.begin_stream()?;
    output.emit(&Entry::Header { version: 1 })?;
    let mut after = None;
    let mut exported = BTreeSet::new();
    let mut missing = false;
    loop {
        let (keys, more) = snapshot.page(after);
        for key in &keys {
            let encoded = snapshot
                .record(*key)
                .ok_or_else(|| Failure::new(4, "export record disappeared"))?;
            output.emit(&Entry::Operation {
                encoded: encoded.to_vec(),
            })?;
            for hash in replica.blob_hashes(&snapshot, *key)? {
                if !exported.insert(hash) {
                    continue;
                }
                let id = ContentId::Hash256(hash);
                if let Some(bytes) = replica.read_blob(&snapshot, *key, hash)? {
                    let len = u32::try_from(bytes.len())
                        .map_err(|error| Failure::new(4, error.to_string()))?;
                    output.emit(&Entry::Blob {
                        reference: BlobRef { id, len },
                        bytes,
                    })?;
                } else {
                    missing = true;
                    output.emit(&Entry::MissingBlob { id })?;
                }
            }
        }
        if !more {
            break;
        }
        after = keys.last().copied();
    }
    output.emit(&Entry::Summary { stats })?;
    if stats.undecodable > 0 || stats.incomplete_tails > 0 {
        return Err(Failure::new(
            4,
            "export omitted unsupported or incomplete records; preserve original segment files",
        ));
    }
    if missing {
        Err(Failure::new(3, "export contains missing blobs"))
    } else {
        Ok(())
    }
}

pub(super) fn restore(chain: &Path, path: &Path, output: &mut Output) -> Result<()> {
    require_chain(chain)?;
    let mut batch = RestoreBatch {
        writer: Engine::open(chain)?.writer()?,
        records: Vec::new(),
        blobs: Vec::new(),
        replies: Vec::new(),
        bytes: 0,
    };
    let mut header = false;
    let mut summary = false;
    let mut conflict = false;
    let mut missing = false;
    let mut records = 0_usize;
    output.begin_stream()?;
    input::records::<Entry>(path, |entry| {
        if summary {
            return Err(Failure::input("archive contains data after its summary"));
        }
        if !header {
            if !matches!(entry, Entry::Header { version: 1 }) {
                return Err(Failure::input("expected archive header version 1"));
            }
            header = true;
            return Ok(());
        }
        match entry {
            Entry::Header { .. } => return Err(Failure::input("duplicate archive header")),
            Entry::Operation { encoded } => {
                let operation = editchain_engine::decode_op(&encoded)
                    .map_err(|error| Failure::input(error.to_string()))?;
                batch.bytes = batch.bytes.saturating_add(encoded.len());
                batch.records.push(encoded);
                batch.replies.push(Reply::Operation(operation.id));
                records = records.saturating_add(1);
            }
            Entry::Blob { reference, bytes } => {
                let expected = ContentId::Hash256(*blake3::hash(&bytes).as_bytes());
                if reference.id != expected
                    || usize::try_from(reference.len).ok() != Some(bytes.len())
                {
                    return Err(Failure::input("archive blob address or length mismatch"));
                }
                batch.bytes = batch.bytes.saturating_add(bytes.len());
                batch.blobs.push(bytes);
                batch.replies.push(Reply::Blob(reference));
            }
            Entry::MissingBlob { .. } => missing = true,
            Entry::Summary { stats } => {
                summary = true;
                if stats.accepted.checked_add(stats.quarantined) != Some(records) {
                    return Err(Failure::input(
                        "archive record count does not match its summary",
                    ));
                }
                if stats.undecodable > 0 || stats.incomplete_tails > 0 {
                    return Err(Failure::new(4, "source archive reports omitted records"));
                }
            }
        }
        if batch.replies.len() >= 1024 || batch.bytes >= 4 * 1024 * 1024 {
            conflict |= batch.flush(output)?;
        }
        Ok(())
    })?;
    conflict |= batch.flush(output)?;
    if !summary {
        return Err(Failure::input(
            "archive is incomplete (no summary); retained entries can be safely replayed",
        ));
    }
    operations::conflict_result(conflict)?;
    if missing {
        Err(Failure::new(3, "archive contains unavailable blobs"))
    } else {
        Ok(())
    }
}

enum Reply {
    Operation(OpId),
    Blob(BlobRef),
}

struct RestoreBatch {
    writer: ChainWriter,
    records: Vec<Vec<u8>>,
    blobs: Vec<Vec<u8>>,
    replies: Vec<Reply>,
    bytes: usize,
}

impl RestoreBatch {
    fn flush(&mut self, output: &mut Output) -> Result<bool> {
        if !self.blobs.is_empty() {
            let blobs: Vec<_> = self.blobs.iter().map(Vec::as_slice).collect();
            let _references = self.writer.store_blobs(&blobs)?;
        }
        let records: Vec<_> = self.records.iter().map(Vec::as_slice).collect();
        let mut admissions = self.writer.append_encoded_batch(&records)?.into_iter();
        let mut conflict = false;
        // Blobs were persisted before this fence. Emit results in archive order
        // only after the entire bounded operation batch is durable.
        for reply in self.replies.drain(..) {
            match reply {
                Reply::Operation(id) => {
                    let admission = admissions
                        .next()
                        .ok_or_else(|| Failure::new(4, "archive admission count mismatch"))?;
                    conflict |= admission == Admission::Conflict;
                    operations::emit_admission(id, admission, output)?;
                }
                Reply::Blob(reference) => output.emit(&serde_json::json!({"blob":reference}))?,
            }
        }
        self.records.clear();
        self.blobs.clear();
        self.bytes = 0;
        Ok(conflict)
    }
}
