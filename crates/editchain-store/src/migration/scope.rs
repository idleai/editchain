//! Translate exact sharing exclusions and receipt provenance, never infer consent.

use super::{atomic_write, check_cancel, create_dir_all, invalid, segment_sequences};
use crate::format::scan::{PageScanner, ScanErrorKind, ScanItem};
use crate::format::{decode_op, migrate_record};
use editchain_core::OpId;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    io::Read as _,
    path::Path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Key {
    id: OpId,
    digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    version: u16,
    space: String,
    excluded: BTreeSet<Key>,
    received: BTreeSet<Key>,
    received_blobs: BTreeSet<[u8; 32]>,
    #[serde(default)]
    local: BTreeSet<Key>,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    cutoff: Option<Cutoff>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cutoff {
    first_segment: u32,
    selected_at_ms: u64,
}

pub(super) fn migrate(
    source: &Path,
    destination: &Path,
    cancelled: &impl Fn() -> bool,
) -> io::Result<()> {
    let path = source.join("multiplayer/scope.json");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    let _read = file.take(128 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 128 * 1024 * 1024 {
        return Err(invalid("scope metadata exceeds limit"));
    }
    let mut scope: Scope = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if scope.version != 1 {
        return Err(invalid("unsupported scope version"));
    }
    match fs::read(source.join("multiplayer/scope-revision")) {
        Ok(fence) if fence == scope.revision.to_le_bytes() => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound && scope.revision == 0 => {}
        _ => {
            return Err(invalid(
                "sharing policy has an incomplete revision; repair it before migration",
            ))
        }
    }
    let original = destination.join("migration-v1/original/multiplayer");
    create_dir_all(&original)?;
    atomic_write(&original.join("scope.json"), &bytes)?;
    let requested: BTreeSet<_> = scope
        .excluded
        .iter()
        .chain(&scope.received)
        .chain(&scope.local)
        .copied()
        .collect();
    let mut translated = BTreeMap::new();
    let mut excluded_by_cutoff = BTreeSet::new();
    for sequence in segment_sequences(source)? {
        check_cancel(cancelled)?;
        let bytes = fs::read(source.join(format!("{sequence:06}.eclog")))?;
        for item in PageScanner::new(&bytes) {
            match item {
                Ok(ScanItem::Page { .. }) => {}
                Ok(ScanItem::Record(record)) => {
                    let Ok(op) = decode_op(record.data) else {
                        continue;
                    };
                    let old = Key {
                        id: op.id,
                        digest: *blake3::hash(record.data).as_bytes(),
                    };
                    let before = scope
                        .cutoff
                        .as_ref()
                        .is_some_and(|cutoff| sequence < cutoff.first_segment);
                    if before || requested.contains(&old) {
                        let new = Key {
                            id: op.id,
                            digest: *blake3::hash(
                                &migrate_record(record.data).map_err(io::Error::other)?,
                            )
                            .as_bytes(),
                        };
                        if before {
                            let _: bool = excluded_by_cutoff.insert(new);
                        }
                        if requested.contains(&old) {
                            let _: Option<Key> = translated.insert(old, new);
                        }
                    }
                }
                Err(error) if error.kind == ScanErrorKind::IncompleteTail => break,
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
            }
        }
    }
    let remap = |keys: &BTreeSet<Key>| {
        keys.iter()
            .map(|key| translated.get(key).copied().unwrap_or(*key))
            .collect::<BTreeSet<_>>()
    };
    scope.excluded = remap(&scope.excluded);
    scope.excluded.extend(excluded_by_cutoff);
    scope.received = remap(&scope.received);
    scope.local = remap(&scope.local);
    // Segment boundaries changed. Exact exclusions preserve the old cutoff;
    // operations appended after publication remain eligible under that consent.
    scope.cutoff = None;
    scope.revision = scope
        .revision
        .checked_add(1)
        .ok_or_else(|| invalid("scope revision exhausted"))?;
    let serialized = serde_json::to_vec(&scope).map_err(io::Error::other)?;
    if serialized.len() > 128 * 1024 * 1024 {
        return Err(invalid("migrated sharing policy exceeds limit"));
    }
    let root = destination.join("multiplayer");
    create_dir_all(&root)?;
    atomic_write(&root.join("scope-revision"), &scope.revision.to_le_bytes())?;
    atomic_write(&root.join("scope.json"), &serialized)
}
