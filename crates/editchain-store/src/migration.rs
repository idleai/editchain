//! Resumable, source-preserving EC03 migration. Publication is a directory rename.

mod scope;

use crate::durable::{atomic_write, create_dir_all, sync_parent_dir};
use crate::format::scan::{PageScanner, ScanErrorKind, ScanItem};
use crate::format::{decode_op, encode_op, migrate_record};
use crate::segment::segment_sequences;
use crate::{AppendLog as _, SegmentStore};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Instant,
};

const STATE: &str = ".migration.json";

/// Verified migration outcome, with wall-clock phase timings in seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationReport {
    /// Complete record occurrences, including duplicates and unknown schemas.
    pub records: u64,
    /// Output record occurrences after a schema conversion.
    #[serde(default)]
    pub output_records: u64,
    /// Undecodable records retained byte for byte.
    pub unknown_records: u64,
    /// Interrupted source tails retained in the original-evidence archive.
    pub incomplete_tails: u64,
    /// Number of original segments retained and checked.
    pub source_segments: usize,
    /// Original segment bytes.
    pub source_bytes: u64,
    /// Migrated segment bytes.
    pub destination_bytes: u64,
    /// Source read, decode, re-encode, and original-evidence archival time.
    pub conversion_seconds: f64,
    /// Destination append and durable synchronization time.
    pub write_seconds: f64,
    /// Blob and cursor copy time.
    pub auxiliary_seconds: f64,
    /// Independent full source/destination comparison time.
    pub validation_seconds: f64,
    /// Digest of ordered canonical envelopes, unknown evidence, lengths and flags.
    pub semantic_digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    complete: bool,
    source: PathBuf,
    originals: Vec<String>,
    frontier: Option<(u32, u64)>,
    report: MigrationReport,
    #[serde(default)]
    transform: Option<String>,
    #[serde(default)]
    exact_digest: bool,
}

/// An explicit versioned record conversion, run under the source writer lock.
/// The same converter is run again to verify every output before publication.
pub trait RecordTransform {
    /// Stable conversion contract, or `None` for the existing wire-only migration.
    fn name(&self) -> Option<&str>;
    /// Read source metadata and prepare destination blob storage.
    /// # Errors
    /// Returns validation, cancellation, or source IO errors.
    fn prepare(
        &mut self,
        source: &Path,
        destination: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<()>;
    /// Convert one complete record into zero or more immutable records.
    /// # Errors
    /// Returns conversion errors without publishing the destination.
    fn convert(&mut self, encoded: &[u8]) -> io::Result<Vec<Vec<u8>>>;
    /// Publish converter-specific metadata before the final directory rename.
    /// # Errors
    /// Returns IO or validation errors.
    fn finish(&mut self, source: &Path, destination: &Path) -> io::Result<()>;
}

struct WireTransform;

impl RecordTransform for WireTransform {
    fn name(&self) -> Option<&str> {
        None
    }
    fn prepare(
        &mut self,
        _source: &Path,
        _destination: &Path,
        _cancelled: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        Ok(())
    }
    fn convert(&mut self, encoded: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        Ok(vec![migrate_record(encoded).map_err(io::Error::other)?])
    }
    fn finish(&mut self, _source: &Path, _destination: &Path) -> io::Result<()> {
        Ok(())
    }
}

/// Migrate into a new directory, resuming `<destination>.migrating` on retry.
/// Original segment bytes are retained under `migration-v1/original`; blobs
/// and import cursors are copied. Derived indexes are rebuilt by their readers.
/// A published destination is never replaced. The source writer lock is held
/// throughout conversion, auxiliary copying, and independent validation.
///
/// # Errors
/// Rejects changed sources, existing destinations, damaged framing, competing
/// writers, failed validation, IO errors, and cancellation. Interrupted work
/// remains resumable; an uncheckpointed output suffix is discarded on retry.
pub fn migrate(
    source: &Path,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> io::Result<MigrationReport> {
    migrate_with(source, destination, &mut WireTransform, cancelled)
}

/// Source-preserving migration with a versioned record converter.
/// # Errors
/// Returns the same publication and resume errors as [`migrate`], plus converter errors.
pub fn migrate_with(
    source: &Path,
    destination: &Path,
    transform: &mut dyn RecordTransform,
    cancelled: impl Fn() -> bool,
) -> io::Result<MigrationReport> {
    let source = fs::canonicalize(source)?;
    if destination.try_exists()? {
        return finish_publication(&source, destination, transform.name(), &cancelled);
    }
    let name = destination
        .file_name()
        .ok_or_else(|| invalid("destination needs a directory name"))?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_dir_all(parent)?;
    let parent = fs::canonicalize(parent)?;
    let destination = parent.join(name);
    if destination.starts_with(&source) {
        return Err(invalid("destination must be outside the source chain"));
    }
    let staging = parent.join(format!("{}.migrating", name.to_string_lossy()));
    let _source_lock = lock(&source.join(".writer.lock"))?;
    create_dir_all(&staging)?;
    if !fs::symlink_metadata(&staging)?.file_type().is_dir() {
        return Err(invalid("migration staging path must be a real directory"));
    }
    let _migration_lock = lock(&staging.join(".migration.lock"))?;
    let mut state = checkpoint(&staging, &source)?;
    let contract = transform.name().map(str::to_owned);
    if !state.originals.is_empty() && state.transform != contract {
        return Err(invalid("migration staging uses another record converter"));
    }
    state.transform = contract;
    state.exact_digest = true;
    create_dir_all(&staging.join("migration-v1/original"))?;
    let sequences = segment_sequences(&source)?;
    if state.originals.len() > sequences.len() {
        return Err(invalid("migration source lost segments"));
    }
    for (sequence, hash) in state.originals.iter().enumerate() {
        let filename = format!("{sequence:06}.eclog");
        check_cancel(&cancelled)?;
        for path in [
            source.join(&filename),
            staging.join("migration-v1/original").join(&filename),
        ] {
            if blake3::hash(&fs::read(path)?).to_hex().as_str() != hash {
                return Err(invalid("migration source or evidence archive changed"));
            }
        }
    }
    rollback(&staging, state.frontier)?;
    transform.prepare(&source, &staging, &cancelled)?;
    let mut writer = SegmentStore::open_migration(staging.clone())?;
    for sequence in sequences.into_iter().skip(state.originals.len()) {
        check_cancel(&cancelled)?;
        let started = Instant::now();
        let filename = format!("{sequence:06}.eclog");
        let bytes = fs::read(source.join(&filename))?;
        let original = staging.join("migration-v1/original").join(&filename);
        atomic_write(&original, &bytes)?;
        let mut records = Vec::new();
        for item in PageScanner::new(&bytes) {
            check_cancel(&cancelled)?;
            match item {
                Ok(ScanItem::Page { .. }) => {}
                Ok(ScanItem::Record(record)) => {
                    state.report.records = state.report.records.saturating_add(1);
                    if decode_op(record.data).is_err() {
                        state.report.unknown_records =
                            state.report.unknown_records.saturating_add(1);
                    }
                    for encoded in transform.convert(record.data)? {
                        state.report.output_records = state.report.output_records.saturating_add(1);
                        records.push((record.flags, encoded));
                    }
                }
                Err(error) if error.kind == ScanErrorKind::IncompleteTail => {
                    state.report.incomplete_tails = state.report.incomplete_tails.saturating_add(1);
                    break;
                }
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
            }
        }
        state.report.conversion_seconds += started.elapsed().as_secs_f64();
        let started = Instant::now();
        let borrowed: Vec<_> = records
            .iter()
            .map(|(flags, data)| (*flags, data.as_slice()))
            .collect();
        writer.append_records(&borrowed)?;
        state.report.write_seconds += started.elapsed().as_secs_f64();
        state
            .originals
            .push(blake3::hash(&bytes).to_hex().to_string());
        state.report.source_segments = state.originals.len();
        state.report.source_bytes = state
            .report
            .source_bytes
            .saturating_add(u64::try_from(bytes.len()).map_err(io::Error::other)?);
        state.frontier = segment_sequences(&staging)?
            .last()
            .map(|sequence| {
                fs::metadata(staging.join(format!("{sequence:06}.eclog")))
                    .map(|metadata| (*sequence, metadata.len()))
            })
            .transpose()?;
        save(&staging, &state)?;
    }
    drop(writer);
    let started = Instant::now();
    for directory in ["blobs", "cursors"] {
        copy_tree(
            &source.join(directory),
            &staging.join(directory),
            &cancelled,
        )?;
    }
    scope::migrate(&source, &staging, &cancelled)?;
    transform.finish(&source, &staging)?;
    state.report.auxiliary_seconds += started.elapsed().as_secs_f64();
    check_cancel(&cancelled)?;
    let started = Instant::now();
    let original = transformed_digest(&source, transform, &cancelled)?;
    let migrated = encoded_digest(&staging, &cancelled)?;
    if original != migrated {
        return Err(invalid(
            "migrated records differ from the complete source history",
        ));
    }
    state.report.semantic_digest = original;
    state.report.validation_seconds += started.elapsed().as_secs_f64();
    state.report.destination_bytes =
        segment_sequences(&staging)?
            .into_iter()
            .try_fold(0_u64, |total, sequence| {
                fs::metadata(staging.join(format!("{sequence:06}.eclog")))
                    .map(|metadata| total.saturating_add(metadata.len()))
            })?;
    state.complete = true;
    save(&staging, &state)?;
    atomic_write(
        &staging.join("migration-v1/manifest.json"),
        &serde_json::to_vec_pretty(&state).map_err(io::Error::other)?,
    )?;
    // The staging marker blocks ordinary writers until publication. Rename
    // preserves the same filesystem durability boundary as a new segment.
    if destination.try_exists()? {
        return Err(invalid("migration destination appeared during conversion"));
    }
    fs::rename(&staging, &destination)?;
    sync_parent_dir(&destination)?;
    fs::remove_file(destination.join(STATE))?;
    crate::durable::sync_dir(&destination)?;
    Ok(state.report)
}

fn lock(path: &Path) -> io::Result<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

fn checkpoint(staging: &Path, source: &Path) -> io::Result<Checkpoint> {
    match fs::read(staging.join(STATE)) {
        Ok(bytes) => {
            let state: Checkpoint = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if state.version != 1 || state.source != source {
                return Err(invalid("staging directory belongs to another migration"));
            }
            Ok(state)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            for entry in fs::read_dir(staging)? {
                if entry?.file_name() != ".migration.lock" {
                    return Err(invalid("unowned migration staging directory"));
                }
            }
            let state = Checkpoint {
                version: 1,
                complete: false,
                source: source.into(),
                originals: Vec::new(),
                frontier: None,
                report: MigrationReport::default(),
                transform: None,
                exact_digest: false,
            };
            save(staging, &state)?;
            Ok(state)
        }
        Err(error) => Err(error),
    }
}

fn save(staging: &Path, state: &Checkpoint) -> io::Result<()> {
    atomic_write(
        &staging.join(STATE),
        &serde_json::to_vec_pretty(state).map_err(io::Error::other)?,
    )
}

fn rollback(staging: &Path, frontier: Option<(u32, u64)>) -> io::Result<()> {
    let sequences = segment_sequences(staging)?;
    if frontier.is_some_and(|(last, _)| !sequences.contains(&last)) {
        return Err(invalid("checkpointed migration segment is missing"));
    }
    for sequence in sequences.into_iter().rev() {
        let path = staging.join(format!("{sequence:06}.eclog"));
        match frontier {
            Some((last, length)) if sequence == last => {
                let file = fs::OpenOptions::new().write(true).open(path)?;
                if file.metadata()?.len() < length {
                    return Err(invalid("checkpointed migration segment was truncated"));
                }
                file.set_len(length)?;
                file.sync_all()?;
            }
            Some((last, _)) if sequence < last => {}
            _ => fs::remove_file(path)?,
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path, cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if !source.try_exists()? {
        return Ok(());
    }
    create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        check_cancel(cancelled)?;
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, cancelled)?;
        } else if kind.is_file() {
            let temporary = target.with_extension("migration-tmp");
            let _copied = fs::copy(entry.path(), &temporary)?;
            fs::File::open(&temporary)?.sync_all()?;
            if file_hash(&entry.path())? != file_hash(&temporary)? {
                return Err(invalid("copied auxiliary evidence differs"));
            }
            fs::rename(temporary, &target)?;
            sync_parent_dir(&target)?;
        } else {
            return Err(invalid(
                "migration auxiliary evidence contains a symlink or special file",
            ));
        }
    }
    Ok(())
}

fn semantic_digest(root: &Path, cancelled: &impl Fn() -> bool) -> io::Result<String> {
    let mut hash = blake3::Hasher::new_derive_key("editchain.migration.semantic.v1");
    for sequence in segment_sequences(root)? {
        check_cancel(cancelled)?;
        let bytes = fs::read(root.join(format!("{sequence:06}.eclog")))?;
        for item in PageScanner::new(&bytes) {
            match item {
                Ok(ScanItem::Page { .. }) => {}
                Ok(ScanItem::Record(record)) => {
                    let encoded = decode_op(record.data)
                        .ok()
                        .map(|op| encode_op(&op))
                        .transpose()
                        .map_err(io::Error::other)?;
                    let bytes = encoded.as_deref().unwrap_or(record.data);
                    let _: &mut blake3::Hasher = hash.update(&[record.flags]);
                    let _: &mut blake3::Hasher = hash.update(
                        &u64::try_from(bytes.len())
                            .map_err(io::Error::other)?
                            .to_le_bytes(),
                    );
                    let _: &mut blake3::Hasher = hash.update(bytes);
                }
                Err(error) if error.kind == ScanErrorKind::IncompleteTail => break,
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
            }
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

fn check_cancel(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "migration interrupted; rerun the same command to resume",
        ))
    } else {
        Ok(())
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn finish_publication(
    source: &Path,
    destination: &Path,
    transform: Option<&str>,
    cancelled: &impl Fn() -> bool,
) -> io::Result<MigrationReport> {
    let pending = destination.join(STATE);
    if !pending.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "migration destination already exists",
        ));
    }
    let _lock = lock(&destination.join(".migration.lock"))?;
    let state: Checkpoint =
        serde_json::from_slice(&fs::read(&pending)?).map_err(io::Error::other)?;
    if state.source != source
        || state.version != 1
        || !state.complete
        || state.transform.as_deref() != transform
    {
        return Err(invalid(
            "existing destination is not a verified migration awaiting publication",
        ));
    }
    let digest = if state.exact_digest {
        encoded_digest(destination, cancelled)?
    } else {
        semantic_digest(destination, cancelled)?
    };
    if digest != state.report.semantic_digest {
        return Err(invalid(
            "published migration no longer matches its verified digest",
        ));
    }
    sync_parent_dir(destination)?;
    fs::remove_file(pending)?;
    crate::durable::sync_dir(destination)?;
    Ok(state.report)
}

fn file_hash(path: &Path) -> io::Result<blake3::Hash> {
    let mut hash = blake3::Hasher::new();
    let _: &mut blake3::Hasher = hash.update_reader(fs::File::open(path)?)?;
    Ok(hash.finalize())
}

fn transformed_digest(
    root: &Path,
    transform: &mut dyn RecordTransform,
    cancelled: &impl Fn() -> bool,
) -> io::Result<String> {
    let mut hash = blake3::Hasher::new();
    let _stats = crate::visit_records(root, &mut |flags, bytes| {
        check_cancel(cancelled)?;
        for record in transform.convert(bytes)? {
            hash_record(&mut hash, flags, &record)?;
        }
        Ok(())
    })?;
    Ok(hash.finalize().to_hex().to_string())
}

fn encoded_digest(root: &Path, cancelled: &impl Fn() -> bool) -> io::Result<String> {
    let mut hash = blake3::Hasher::new();
    let _stats = crate::visit_records(root, &mut |flags, bytes| {
        check_cancel(cancelled)?;
        hash_record(&mut hash, flags, bytes)
    })?;
    Ok(hash.finalize().to_hex().to_string())
}

fn hash_record(hash: &mut blake3::Hasher, flags: u8, bytes: &[u8]) -> io::Result<()> {
    let _hash = hash.update(&[flags]);
    let _hash = hash.update(
        &u64::try_from(bytes.len())
            .map_err(io::Error::other)?
            .to_le_bytes(),
    );
    let _hash = hash.update(bytes);
    Ok(())
}
