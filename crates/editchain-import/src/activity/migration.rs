//! Resumable physical conversion with the store's locked, verified publication.

use super::Converter;
use editchain_core::{OpKind, Payload};
use editchain_store::{
    format::{decode_op, encode_op},
    migration::{MigrationReport, RecordTransform},
    BlobReader, BlobResolution, BlobSource as _,
};
use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    path::Path,
};

const MARKER: &str = "schema3-migration.json";

/// True for a chain whose subsequent imports must use the migration namespace.
/// # Errors
/// Returns marker IO or version errors; a changed converter is never guessed.
pub fn uses_migration_ids(root: &Path) -> io::Result<bool> {
    match fs::read(root.join(MARKER)) {
        Ok(bytes) => {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if value.get("converter").and_then(serde_json::Value::as_str) != Some(super::CONTRACT) {
                return Err(io::Error::other(
                    "older or unsupported schema-three converter; migrate the original pre-schema-three chain or reimport its sources into a new destination",
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => infer_namespace(root),
        Err(error) => Err(error),
    }
}

fn infer_namespace(root: &Path) -> io::Result<bool> {
    if !root.exists() {
        return Ok(false);
    }
    let mut answer = None;
    let result = editchain_store::visit_records(root, &mut |_flags, bytes| {
        if let Ok(op) = decode_op(bytes) {
            if let OpKind::Activity(record) = op.kind {
                if let Some(old) = record.legacy {
                    let upgraded = editchain_core::activity::upgrade_id(old.operation);
                    answer = Some(if record.id == upgraded {
                        Ok(false)
                    } else if record.id == super::migration_id(upgraded) {
                        Ok(true)
                    } else {
                        Err(io::Error::other("older or unsupported schema-three converter; migrate the original pre-schema-three chain or reimport its sources into a new destination"))
                    });
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "namespace found",
                    ));
                }
            }
        }
        Ok(())
    });
    if let Some(answer) = answer {
        return answer;
    }
    result.map(|_stats| false)
}

/// Upgrade a chain to operation schema three in a separate destination.
/// Source files stay intact; originals, old-address mappings, and conflicts
/// are retained. Unknown binary records keep their original bytes.
/// # Errors
/// Returns source, converter, validation, cancellation, or publication errors.
pub fn migrate(
    source: &Path,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> io::Result<MigrationReport> {
    editchain_store::migration::migrate_with(
        source,
        destination,
        &mut Transform::default(),
        cancelled,
    )
}

#[derive(Default)]
struct Transform {
    converter: Converter,
    blobs: Option<crate::FsBlobSink>,
    originals: Option<BlobReader>,
    conflicts: BTreeSet<editchain_core::OpId>,
}

impl RecordTransform for Transform {
    fn name(&self) -> Option<&str> {
        Some(super::CONTRACT)
    }

    fn prepare(
        &mut self,
        source: &Path,
        destination: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        if source.join("multiplayer/scope.json").exists() {
            return Err(io::Error::other("schema-three migration of a chain with sharing rules requires an explicit policy conversion; the source is unchanged"));
        }
        self.converter = Converter::for_migration();
        self.conflicts.clear();
        self.blobs = Some(crate::FsBlobSink::new(destination.join("blobs"))?);
        let blobs = BlobReader::open(source)?;
        let mut seen = HashMap::new();
        let _stats = editchain_store::visit_records(source, &mut |_flags, bytes| {
            if cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "schema conversion interrupted",
                ));
            }
            let Ok(op) = decode_op(bytes) else {
                return Ok(());
            };
            if matches!(op.kind, OpKind::Activity(_)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "source already contains schema-three records; use the existing chain directly",
                ));
            }
            let hash = blake3::hash(bytes);
            if seen
                .insert(op.id, hash)
                .is_some_and(|previous| previous != hash)
            {
                let _inserted = self.conflicts.insert(op.id);
            }
            Ok(())
        })?;
        self.converter.protect_conflicts(self.conflicts.clone());
        let _stats = editchain_store::visit_records(source, &mut |_flags, bytes| {
            if cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "schema conversion interrupted",
                ));
            }
            let Ok(op) = decode_op(bytes) else {
                return Ok(());
            };
            if self.conflicts.contains(&op.id) {
                return Ok(());
            }
            let resolved = if let OpKind::Import(raw) = &op.kind {
                if let Payload::Blob(reference) = raw.raw_ref {
                    match blobs.read_blob(&reference)? {
                        BlobResolution::Found(bytes) => Some(bytes),
                        BlobResolution::Missing
                        | BlobResolution::Corrupt
                        | BlobResolution::Unresolvable => None,
                    }
                } else {
                    None
                }
            } else {
                None
            };
            self.converter
                .observe(&op, resolved.as_deref())
                .map_err(io::Error::other)
        })?;
        self.originals = Some(blobs);
        self.converter.finish_observations();
        Ok(())
    }

    fn convert(&mut self, encoded: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        let Ok(op) = decode_op(encoded) else {
            return Ok(vec![encoded.to_vec()]);
        };
        if self.conflicts.contains(&op.id) || matches!(op.kind, OpKind::Activity(_)) {
            return Ok(vec![encoded.to_vec()]);
        }
        let blobs = self
            .blobs
            .as_mut()
            .ok_or_else(|| io::Error::other("schema converter not prepared"))?;
        if let OpKind::Import(raw) = &op.kind {
            if let Payload::Blob(reference) = &raw.raw_ref {
                if let Some(source) = &self.originals {
                    if let BlobResolution::Found(bytes) = source.read_blob(reference)? {
                        let _reference =
                            crate::BlobSink::put(blobs, &bytes).map_err(io::Error::other)?;
                    }
                }
            }
        }
        self.converter
            .convert(&op, blobs)
            .map_err(io::Error::other)?
            .iter()
            .map(|op| encode_op(op).map_err(io::Error::other))
            .collect()
    }

    fn finish(&mut self, source: &Path, destination: &Path) -> io::Result<()> {
        // Existing offsets remain accepted. Subsequent new records use the same
        // conversion namespace; previously consumed source bytes are not recaptured.
        let cursors = match fs::read_dir(destination.join("cursors")) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if cursors.is_some() {
            fs::create_dir_all(destination.join("cursors-v3"))?;
        }
        for entry in cursors.into_iter().flatten() {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                editchain_store::durable::atomic_write(
                    &destination.join("cursors-v3").join(entry.file_name()),
                    &fs::read(entry.path())?,
                )?;
            }
        }
        let value =
            serde_json::json!({ "schema": 3, "converter": super::CONTRACT, "source": source });
        editchain_store::durable::atomic_write(
            &destination.join(MARKER),
            &serde_json::to_vec(&value).map_err(io::Error::other)?,
        )
    }
}
