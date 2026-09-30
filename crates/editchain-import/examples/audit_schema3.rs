//! Sequential full-corpus audit: exact sources, every old address, and all payload references.
use editchain_engine as _;
use editchain_import as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use sha2 as _;
use tempfile as _;
use time as _;
use tokio as _;

use editchain_core::{activity::Kind, ContentId, OpId, OpKind, Payload};
use editchain_store::{format, BlobReader, BlobResolution, BlobSource as _};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io,
    path::Path,
};

#[derive(Default)]
struct Audit {
    records: u64,
    bytes: u64,
    kinds: BTreeMap<String, u64>,
    identities: BTreeMap<OpId, [u8; 32]>,
    aliases: BTreeSet<OpId>,
    mappings: BTreeMap<OpId, OpId>,
    primary: BTreeMap<OpId, OpId>,
    parents: BTreeMap<OpId, Vec<OpId>>,
    parent_links: BTreeMap<String, u64>,
    originals: BTreeMap<OpId, [u8; 32]>,
    references: BTreeSet<OpId>,
    blobs: HashSet<(ContentId, Option<u32>)>,
}

fn payload(payload: &Payload, blobs: &BlobReader) -> io::Result<Vec<u8>> {
    match payload {
        Payload::Inline(bytes) => Ok(bytes.clone()),
        Payload::Empty => Ok(Vec::new()),
        Payload::Blob(reference) => match blobs.read_blob(reference)? {
            BlobResolution::Found(bytes) => Ok(bytes),
            BlobResolution::Missing | BlobResolution::Corrupt | BlobResolution::Unresolvable => {
                Err(io::Error::other("unavailable source blob"))
            }
        },
    }
}

fn scan(root: &Path) -> io::Result<Audit> {
    let mut audit = Audit::default();
    let blobs = BlobReader::open(root)?;
    let stats = editchain_store::visit_records(root, &mut |_flags, encoded| {
        let op = format::decode_op(encoded).map_err(io::Error::other)?;
        drop(audit.parents.insert(op.id, op.causal_parents()));
        audit.records = audit.records.saturating_add(1);
        audit.bytes = audit
            .bytes
            .saturating_add(u64::try_from(encoded.len()).map_err(io::Error::other)?);
        let hash = *blake3::hash(encoded).as_bytes();
        if audit
            .identities
            .insert(op.id, hash)
            .is_some_and(|previous| previous != hash)
        {
            return Err(io::Error::other("unexpected conflicting record in corpus"));
        }
        if let OpKind::Activity(record) = &op.kind {
            if format::encode_op(&op).map_err(io::Error::other)? != encoded {
                return Err(io::Error::other("schema-three roundtrip changed bytes"));
            }
            let count = audit
                .kinds
                .entry(format!("{:?}", record.kind.name()))
                .or_default();
            *count = count.saturating_add(1);
            if let Some(mapping) = &record.legacy {
                let _inserted = audit.aliases.insert(mapping.operation);
                audit.aliases.extend(&mapping.folded);
                let _previous = audit.primary.insert(mapping.operation, record.id);
                for old in std::iter::once(&mapping.operation).chain(&mapping.folded) {
                    let _previous = audit.mappings.insert(*old, record.id);
                }
            }
            if let Kind::Link(link) = &record.kind {
                count_parent_link(&mut audit.parent_links, &link.relation);
            }
            if let Some(original) = &record.original {
                let _inserted = audit.references.insert(original.operation);
            }
            if let Kind::Original(original) = &record.kind {
                let bytes = payload(&original.bytes, &blobs)?;
                let hash = *blake3::hash(&bytes).as_bytes();
                if original.hash.is_some_and(|expected| expected != hash) {
                    return Err(io::Error::other("Original hash differs from bytes"));
                }
                let old = record
                    .legacy
                    .as_ref()
                    .map_or(record.id, |mapping| mapping.operation);
                let _old = audit.originals.insert(old, hash);
            }
            audit.blobs.extend(record.kind.content_addresses());
        } else if let OpKind::Import(raw) = &op.kind {
            let bytes = payload(&raw.raw_ref, &blobs)?;
            let synthetic_title = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .is_some_and(|value| {
                    value.get("type").and_then(serde_json::Value::as_str) == Some("session_title")
                });
            if !synthetic_title {
                let _old = audit
                    .originals
                    .insert(op.id, *blake3::hash(&bytes).as_bytes());
            }
        } else if let OpKind::Note(note) = &op.kind {
            count_parent_link(&mut audit.parent_links, &format!("{:?}", note.relationship));
        }
        Ok(())
    })?;
    if stats.incomplete_tails != 0 {
        return Err(io::Error::other("incomplete corpus tail"));
    }
    for id in &audit.references {
        if !audit.identities.contains_key(id) {
            return Err(io::Error::other(format!("missing Original {id}")));
        }
    }
    for (id, length) in &audit.blobs {
        let value = if let Some(len) = length {
            blobs.read_blob(&editchain_core::BlobRef { id: *id, len: *len })?
        } else {
            blobs.read_content(*id)?
        };
        if !matches!(value, BlobResolution::Found(_)) {
            return Err(io::Error::other("missing or corrupt payload"));
        }
    }
    Ok(audit)
}

fn count_parent_link(counts: &mut BTreeMap<String, u64>, relation: &str) {
    if matches!(relation, "ProviderParent" | "LogicalParent") {
        let count = counts.entry(relation.into()).or_default();
        *count = count.saturating_add(1);
    }
}

fn check_parents(old: &Audit, converted: &Audit) -> io::Result<u64> {
    let mut checked = 0_u64;
    for (old_id, parents) in &old.parents {
        let Some(id) = converted
            .primary
            .get(old_id)
            .copied()
            .or_else(|| converted.identities.contains_key(old_id).then_some(*old_id))
        else {
            continue; // Folded records retain their address, but have no separate parent list.
        };
        for parent in parents
            .iter()
            .filter(|parent| old.identities.contains_key(parent))
        {
            let expected = converted.mappings.get(parent).copied().unwrap_or(*parent);
            if !converted.identities.contains_key(&expected)
                || !converted
                    .parents
                    .get(&id)
                    .is_some_and(|parents| parents.contains(&expected))
            {
                return Err(io::Error::other(format!(
                    "lost parent {parent} of {old_id} after conversion to {id}"
                )));
            }
            checked = checked.saturating_add(1);
        }
    }
    if old.parent_links != converted.parent_links {
        return Err(io::Error::other(
            "provider or logical parent links were lost",
        ));
    }
    Ok(checked)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [old, fresh, migrated] = args.as_slice() else {
        return Err("usage: audit_schema3 OLD FRESH MIGRATED".into());
    };
    let old = scan(Path::new(old))?;
    let fresh = scan(Path::new(fresh))?;
    let migrated = scan(Path::new(migrated))?;
    let fresh_parents = check_parents(&old, &fresh)?;
    let migrated_parents = check_parents(&old, &migrated)?;
    serde_json::to_writer_pretty(
        io::stderr().lock(),
        &serde_json::json!({
            "old_originals": old.originals.len(), "fresh_originals": fresh.originals.len(), "migrated_originals": migrated.originals.len(),
            "original_bytes_match_fresh": old.originals == fresh.originals, "original_bytes_match_migration": old.originals == migrated.originals,
            "fresh_kinds": fresh.kinds, "migrated_kinds": migrated.kinds
        }),
    )?;
    if old.originals != fresh.originals || old.originals != migrated.originals {
        return Err("exact Original identities/bytes differ from old corpus".into());
    }
    if fresh.kinds != migrated.kinds {
        return Err("fresh and migrated operation counts differ".into());
    }
    if old
        .identities
        .keys()
        .any(|id| !migrated.aliases.contains(id) && !migrated.identities.contains_key(id))
    {
        return Err("migration lost an old address".into());
    }
    serde_json::to_writer_pretty(
        io::stdout().lock(),
        &serde_json::json!({
            "status": "pass", "old_records": old.records, "originals": old.originals.len(),
            "fresh_parent_edges_checked": fresh_parents, "migrated_parent_edges_checked": migrated_parents,
            "parent_links": migrated.parent_links,
            "fresh_records": fresh.records, "migrated_records": migrated.records,
            "fresh_record_bytes": fresh.bytes, "migrated_record_bytes": migrated.bytes,
            "old_address_mappings": migrated.aliases.len(), "resolved_original_references": migrated.references.len(),
            "fresh_blob_references": fresh.blobs.len(), "kinds": migrated.kinds
        }),
    )?;
    Ok(())
}
