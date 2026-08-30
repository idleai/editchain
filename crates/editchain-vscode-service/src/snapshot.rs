//! Versioned, immutable render snapshots for fast VS Code history startup.
//!
//! A snapshot stores the fixed default history view as newline-delimited
//! protocol rows plus compact random-access indices. It is derived data: the
//! segment log and blob store remain authoritative, and callers fall back to
//! the live projection whenever a snapshot is absent, stale, or incompatible.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use editchain_core::OpId;
use editchain_git::RepositoryDiscovery;
use editchain_import::hash_raw;
use editchain_protocol::{HistoryRow, HistoryWindow};
use serde::{Deserialize, Serialize};

use crate::{OpRecordLocation, OpenDiagnostics, SnapshotOpLocator};

/// On-disk schema for the immutable render snapshot.
pub(crate) const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
/// Revision of projection/default-view semantics represented by this schema.
///
/// Bumped when the fixed default view's semantics change so stale snapshots
/// (which would otherwise silently serve the old default) live under a
/// different identity hash and are rebuilt from the live projection.
///
/// Revision 6: compacted import JSON now carries an explicit
/// `payload.echo_text_truncated` flag whenever a record's echo message text
/// was truncated by the display budget, so truncated texts never participate
/// in cross-record duplicate pairing; the projection also recovers
/// payload-level `exitCode` evidence from truncated previews and decodes
/// complete JSON string escapes recovered from a bounded prefix.
const SNAPSHOT_PROJECTION_REVISION: u32 = 6;
/// Root directory for render snapshot schema versions.
const SNAPSHOT_ROOT: &str = "render";
/// Manifest written last, after every data file is durable.
const MANIFEST_FILE: &str = "manifest.json";
/// Expanded history rows, one JSON object per line.
const ROWS_FILE: &str = "rows.ndjson";
/// Little-endian `u64` byte offsets into [`ROWS_FILE`], including a sentinel.
const ROW_OFFSETS_FILE: &str = "rows.offsets";
/// Little-endian `u32` bundled-child counts, one per top-level row.
const SUB_OP_COUNTS_FILE: &str = "sub-op-counts.bin";
/// Fixed-width operation id to segment-record location index.
const OP_LOCATORS_FILE: &str = "op-locators.bin";
/// Bytes in one encoded operation locator record.
const OP_LOCATOR_BYTES: usize = 36;

/// A successful render-snapshot preparation result.
#[derive(Debug, Clone, Serialize)]
pub struct RenderSnapshotReport {
    /// Published immutable snapshot directory.
    pub path: PathBuf,
    /// Whether an already-valid snapshot was reused without rebuilding.
    pub reused: bool,
    /// Fully expanded row count served to the extension.
    pub rows: u64,
    /// Canonical top-level row count before bundled children are expanded.
    pub top_level_rows: u64,
    /// Number of accepted source operations represented by the snapshot.
    pub chain_generation: u64,
    /// Total bytes across the snapshot's regular files.
    pub bytes: u64,
}

/// Fast freshness identity for the source chain, Git overlay, and projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SnapshotIdentity {
    schema_version: u32,
    projection_revision: u32,
    chain_files: Vec<ChainFileStamp>,
    repositories: Vec<RepositoryStamp>,
}

/// Metadata stamp for one append-only segment file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ChainFileStamp {
    name: String,
    length: u64,
    modified_secs: u64,
    modified_nanos: u32,
}

/// Metadata stamp for one repository whose HEAD contributes live Git history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RepositoryStamp {
    id: u64,
    path: String,
    is_worktree: bool,
    head: String,
}

impl SnapshotIdentity {
    /// Capture a cheap identity without decoding operations or walking history.
    pub(crate) fn capture(
        chain_dir: &Path,
        repositories: &[RepositoryDiscovery],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut chain_files = Vec::new();
        for entry in fs::read_dir(chain_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("eclog") {
                continue;
            }
            let metadata = entry.metadata()?;
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            let duration = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
            chain_files.push(ChainFileStamp {
                name: entry.file_name().to_string_lossy().into_owned(),
                length: metadata.len(),
                modified_secs: duration.as_secs(),
                modified_nanos: duration.subsec_nanos(),
            });
        }
        chain_files.sort_by(|left, right| left.name.cmp(&right.name));

        let mut repository_stamps = repositories
            .iter()
            .map(|repository| {
                let head = crate::open_repository_handle(repository)
                    .ok()
                    .and_then(|handle| {
                        let head = handle.repo.head().ok()?;
                        head.id().map(|id| id.to_string())
                    })
                    .unwrap_or_default();
                RepositoryStamp {
                    id: repository.id.0,
                    path: repository.path.to_string_lossy().into_owned(),
                    is_worktree: repository.is_worktree,
                    head,
                }
            })
            .collect::<Vec<_>>();
        repository_stamps.sort_by(|left, right| {
            (left.id, left.path.as_str()).cmp(&(right.id, right.path.as_str()))
        });

        Ok(Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            projection_revision: SNAPSHOT_PROJECTION_REVISION,
            chain_files,
            repositories: repository_stamps,
        })
    }

    /// Stable lowercase BLAKE3 hex key used as the immutable directory name.
    fn hash(&self) -> Result<String, Box<dyn std::error::Error>> {
        let encoded = serde_json::to_vec(self)?;
        hex_string(&hash_raw(&encoded)).map_err(Into::into)
    }
}

/// Snapshot metadata loaded before any row data.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotManifest {
    format: String,
    identity: SnapshotIdentity,
    identity_hash: String,
    generated_at_ms: u64,
    projection_nodes: u64,
    chain_generation: u64,
    expanded_rows: u64,
    top_level_rows: u64,
    max_lane: usize,
    diagnostics: OpenDiagnostics,
    op_locator_count: u64,
}

/// Inputs fixed after the projection has been computed and before publication.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotManifestData {
    pub(crate) projection_nodes: u64,
    pub(crate) chain_generation: u64,
    pub(crate) max_lane: usize,
    pub(crate) diagnostics: OpenDiagnostics,
}

/// A streaming writer that never retains the complete protocol row corpus.
#[derive(Debug)]
pub(crate) struct SnapshotBuilder {
    identity: SnapshotIdentity,
    identity_hash: String,
    schema_root: PathBuf,
    staging_path: PathBuf,
    rows: Option<BufWriter<File>>,
    row_offsets: Vec<u64>,
    row_bytes: u64,
    published: bool,
}

impl SnapshotBuilder {
    /// Start writing an unpublished snapshot under a unique staging directory.
    pub(crate) fn new(
        chain_dir: &Path,
        identity: SnapshotIdentity,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let identity_hash = identity.hash()?;
        let schema_root = snapshot_schema_root(chain_dir);
        fs::create_dir_all(&schema_root)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let staging_path = schema_root.join(format!(".staging-{}-{nonce}", std::process::id()));
        fs::create_dir(&staging_path)?;
        let rows = BufWriter::new(File::create(staging_path.join(ROWS_FILE))?);
        Ok(Self {
            identity,
            identity_hash,
            schema_root,
            staging_path,
            rows: Some(rows),
            row_offsets: vec![0],
            row_bytes: 0,
            published: false,
        })
    }

    /// Append one contiguous row window to the snapshot.
    pub(crate) fn write_rows(
        &mut self,
        rows: &[HistoryRow],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let writer = self.rows.as_mut().ok_or("snapshot row writer is closed")?;
        for row in rows {
            let encoded = serde_json::to_vec(row)?;
            writer.write_all(&encoded)?;
            writer.write_all(b"\n")?;
            let encoded_len = u64::try_from(encoded.len())?;
            self.row_bytes = self.row_bytes.saturating_add(encoded_len).saturating_add(1);
            self.row_offsets.push(self.row_bytes);
        }
        Ok(())
    }

    /// Flush, validate, and atomically publish the completed snapshot.
    pub(crate) fn finish(
        mut self,
        manifest_data: SnapshotManifestData,
        sub_op_counts: &[usize],
        op_locators: &[SnapshotOpLocator],
    ) -> Result<RenderSnapshotReport, Box<dyn std::error::Error>> {
        if let Some(mut rows) = self.rows.take() {
            rows.flush()?;
            rows.get_ref().sync_all()?;
        }
        write_u64_file(&self.staging_path.join(ROW_OFFSETS_FILE), &self.row_offsets)?;
        write_sub_op_counts(&self.staging_path.join(SUB_OP_COUNTS_FILE), sub_op_counts)?;
        write_op_locators(&self.staging_path.join(OP_LOCATORS_FILE), op_locators)?;

        let expanded_rows = u64::try_from(self.row_offsets.len().saturating_sub(1))?;
        let top_level_rows = u64::try_from(sub_op_counts.len())?;
        let manifest = SnapshotManifest {
            format: "editchain-render-snapshot".to_string(),
            identity: self.identity.clone(),
            identity_hash: self.identity_hash.clone(),
            generated_at_ms: unix_time_ms(),
            projection_nodes: manifest_data.projection_nodes,
            chain_generation: manifest_data.chain_generation,
            expanded_rows,
            top_level_rows,
            max_lane: manifest_data.max_lane,
            diagnostics: manifest_data.diagnostics,
            op_locator_count: u64::try_from(op_locators.len())?,
        };
        write_json_file(&self.staging_path.join(MANIFEST_FILE), &manifest)?;
        sync_directory(&self.staging_path)?;

        let final_path = self.schema_root.join(&self.identity_hash);
        let displaced = if final_path.exists() {
            let path = self.schema_root.join(format!(
                ".invalid-{}-{}",
                self.identity_hash,
                std::process::id()
            ));
            fs::rename(&final_path, &path)?;
            Some(path)
        } else {
            None
        };
        fs::rename(&self.staging_path, &final_path)?;
        sync_directory(&self.schema_root)?;
        self.published = true;
        if let Some(path) = displaced {
            drop(fs::remove_dir_all(path));
        }
        cleanup_stale_snapshots(&self.schema_root, &self.identity_hash);

        Ok(RenderSnapshotReport {
            path: final_path.clone(),
            reused: false,
            rows: manifest.expanded_rows,
            top_level_rows: manifest.top_level_rows,
            chain_generation: manifest.chain_generation,
            bytes: snapshot_size(&final_path)?,
        })
    }
}

impl Drop for SnapshotBuilder {
    fn drop(&mut self) {
        if !self.published {
            drop(fs::remove_dir_all(&self.staging_path));
        }
    }
}

/// A validated random-access render snapshot.
#[derive(Debug)]
pub(crate) struct RenderSnapshot {
    path: PathBuf,
    chain_dir: PathBuf,
    manifest: SnapshotManifest,
    rows: File,
    row_offsets: Vec<u64>,
    sub_op_counts: Option<Vec<usize>>,
    op_locators: Vec<SnapshotOpLocator>,
}

impl RenderSnapshot {
    /// Open the exact snapshot for `identity`; return `None` on a cache miss.
    pub(crate) fn open(
        chain_dir: &Path,
        identity: &SnapshotIdentity,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let identity_hash = identity.hash()?;
        let path = snapshot_schema_root(chain_dir).join(&identity_hash);
        if !path.is_dir() {
            return Ok(None);
        }
        let manifest: SnapshotManifest =
            serde_json::from_reader(File::open(path.join(MANIFEST_FILE))?)?;
        if manifest.format != "editchain-render-snapshot"
            || manifest.identity != *identity
            || manifest.identity_hash != identity_hash
        {
            return Ok(None);
        }

        let row_offsets = read_u64_file(&path.join(ROW_OFFSETS_FILE))?;
        let expected_offsets = usize::try_from(manifest.expanded_rows)?.saturating_add(1);
        if row_offsets.len() != expected_offsets {
            return Err(invalid_data("render snapshot row-offset count mismatch").into());
        }
        let rows = File::open(path.join(ROWS_FILE))?;
        let rows_len = rows.metadata()?.len();
        if row_offsets.last().copied() != Some(rows_len) {
            return Err(invalid_data("render snapshot row sentinel mismatch").into());
        }
        let op_locators = read_op_locators(&path.join(OP_LOCATORS_FILE))?;
        if u64::try_from(op_locators.len())? != manifest.op_locator_count
            || !op_locators.windows(2).all(|pair| {
                pair.first()
                    .zip(pair.get(1))
                    .is_some_and(|(left, right)| left.id < right.id)
            })
        {
            return Err(invalid_data("render snapshot operation index is invalid").into());
        }
        let sub_op_counts =
            read_sub_op_counts(&path.join(SUB_OP_COUNTS_FILE), manifest.top_level_rows)?;

        Ok(Some(Self {
            path,
            chain_dir: chain_dir.to_path_buf(),
            manifest,
            rows,
            row_offsets,
            sub_op_counts: Some(sub_op_counts),
            op_locators,
        }))
    }

    /// Report an already-valid snapshot without rebuilding it.
    pub(crate) fn report(&self) -> Result<RenderSnapshotReport, Box<dyn std::error::Error>> {
        Ok(RenderSnapshotReport {
            path: self.path.clone(),
            reused: true,
            rows: self.manifest.expanded_rows,
            top_level_rows: self.manifest.top_level_rows,
            chain_generation: self.manifest.chain_generation,
            bytes: snapshot_size(&self.path)?,
        })
    }

    /// Read a bounded expanded-row window, optionally retaining lane geometry.
    pub(crate) fn history_window(
        &mut self,
        offset: u64,
        limit: u64,
        include_layout: bool,
    ) -> Result<HistoryWindow, Box<dyn std::error::Error>> {
        let total = usize::try_from(self.manifest.expanded_rows)?;
        let start_index = usize::try_from(offset).unwrap_or(usize::MAX).min(total);
        let requested = usize::try_from(limit).unwrap_or(usize::MAX);
        let end_index = start_index.saturating_add(requested).min(total);
        let start_byte = self
            .row_offsets
            .get(start_index)
            .copied()
            .ok_or_else(|| invalid_data("missing render snapshot start offset"))?;
        let end_byte = self
            .row_offsets
            .get(end_index)
            .copied()
            .ok_or_else(|| invalid_data("missing render snapshot end offset"))?;
        let byte_len = usize::try_from(end_byte.saturating_sub(start_byte))?;
        let mut encoded = vec![0u8; byte_len];
        let _: u64 = self.rows.seek(SeekFrom::Start(start_byte))?;
        self.rows.read_exact(&mut encoded)?;
        let mut rows = Vec::with_capacity(end_index.saturating_sub(start_index));
        for line in encoded
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let mut row: HistoryRow = serde_json::from_slice(line)?;
            if !include_layout {
                row.lane = 0;
                row.above.clear();
                row.below.clear();
                row.transitions.clear();
            }
            rows.push(row);
        }
        if rows.len() != end_index.saturating_sub(start_index) {
            return Err(invalid_data("render snapshot row count mismatch").into());
        }
        let sub_op_counts = if offset == 0 {
            Some(self.load_sub_op_counts()?.clone())
        } else {
            None
        };
        Ok(HistoryWindow {
            rows,
            total: self.manifest.expanded_rows,
            chain_generation: self.manifest.chain_generation,
            max_lane: if include_layout {
                self.manifest.max_lane
            } else {
                0
            },
            sub_op_counts,
            layout_ready: include_layout,
        })
    }

    /// Locate one source operation for lazy detail hydration.
    pub(crate) fn op_location(&self, id: OpId) -> Option<OpRecordLocation> {
        self.op_locators
            .binary_search_by_key(&id, |entry| entry.id)
            .ok()
            .and_then(|index| self.op_locators.get(index))
            .map(|entry| entry.location)
    }

    /// Authoritative source chain directory for operation-offset reads.
    pub(crate) fn chain_dir(&self) -> &Path {
        &self.chain_dir
    }

    /// Cached projection node count used by the Open handshake.
    pub(crate) const fn projection_nodes(&self) -> u64 {
        self.manifest.projection_nodes
    }

    /// Accepted source operation count used as the chain generation.
    pub(crate) const fn chain_generation(&self) -> u64 {
        self.manifest.chain_generation
    }

    /// Cached diagnostics produced while building this exact snapshot.
    pub(crate) const fn diagnostics(&self) -> OpenDiagnostics {
        self.manifest.diagnostics
    }

    /// Load the compact top-level expansion index once per service process.
    fn load_sub_op_counts(&mut self) -> Result<&Vec<usize>, Box<dyn std::error::Error>> {
        if self.sub_op_counts.is_none() {
            self.sub_op_counts = Some(read_sub_op_counts(
                &self.path.join(SUB_OP_COUNTS_FILE),
                self.manifest.top_level_rows,
            )?);
        }
        self.sub_op_counts
            .as_ref()
            .ok_or_else(|| invalid_data("render snapshot sub-op index unavailable").into())
    }
}

/// Read and validate the compact top-level bundled-child index.
fn read_sub_op_counts(
    path: &Path,
    expected_count: u64,
) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() % 4 != 0 {
        return Err(invalid_data("invalid render snapshot sub-op index").into());
    }
    let mut counts = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let value = u32::from_le_bytes(
            chunk
                .try_into()
                .map_err(|_error| invalid_data("invalid render snapshot sub-op entry"))?,
        );
        counts.push(usize::try_from(value)?);
    }
    if u64::try_from(counts.len())? != expected_count {
        return Err(invalid_data("render snapshot sub-op count mismatch").into());
    }
    Ok(counts)
}

/// Snapshot directory for one schema version.
fn snapshot_schema_root(chain_dir: &Path) -> PathBuf {
    chain_dir
        .join(SNAPSHOT_ROOT)
        .join(format!("v{SNAPSHOT_SCHEMA_VERSION}"))
}

/// Current Unix time in milliseconds, saturating if it exceeds `u64`.
fn unix_time_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

/// Lowercase hexadecimal encoding without another runtime dependency.
fn hex_string(bytes: &[u8]) -> Result<String, std::fmt::Error> {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(&mut output, "{byte:02x}")?;
    }
    Ok(output)
}

/// Write a JSON value durably to one new snapshot file.
fn write_json_file(path: &Path, value: &impl Serialize) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = File::create(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

/// Write little-endian `u64` values durably.
fn write_u64_file(path: &Path, values: &[u64]) -> io::Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    for value in values {
        file.write_all(&value.to_le_bytes())?;
    }
    file.flush()?;
    file.get_ref().sync_all()
}

/// Read a complete little-endian `u64` vector.
fn read_u64_file(path: &Path) -> io::Result<Vec<u64>> {
    let bytes = fs::read(path)?;
    if bytes.len() % 8 != 0 {
        return Err(invalid_data("invalid render snapshot u64 index"));
    }
    bytes
        .chunks_exact(8)
        .map(|chunk| {
            chunk
                .try_into()
                .map(u64::from_le_bytes)
                .map_err(|_error| invalid_data("invalid render snapshot u64 entry"))
        })
        .collect()
}

/// Write compact bundled-child counts durably.
fn write_sub_op_counts(path: &Path, counts: &[usize]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = BufWriter::new(File::create(path)?);
    for count in counts {
        file.write_all(&u32::try_from(*count)?.to_le_bytes())?;
    }
    file.flush()?;
    file.get_ref().sync_all()?;
    Ok(())
}

/// Write the sorted fixed-width operation locator index durably.
fn write_op_locators(
    path: &Path,
    locators: &[SnapshotOpLocator],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = BufWriter::new(File::create(path)?);
    for entry in locators {
        file.write_all(&entry.id.node.0.to_le_bytes())?;
        file.write_all(&entry.id.boot.to_le_bytes())?;
        file.write_all(&entry.id.seq.to_le_bytes())?;
        file.write_all(&entry.location.segment_seq.to_le_bytes())?;
        file.write_all(&entry.location.data_offset.to_le_bytes())?;
        file.write_all(&entry.location.data_len.to_le_bytes())?;
    }
    file.flush()?;
    file.get_ref().sync_all()?;
    Ok(())
}

/// Read and validate the sorted fixed-width operation locator index.
fn read_op_locators(path: &Path) -> io::Result<Vec<SnapshotOpLocator>> {
    let bytes = fs::read(path)?;
    if bytes.len() % OP_LOCATOR_BYTES != 0 {
        return Err(invalid_data("invalid render snapshot operation index"));
    }
    bytes
        .chunks_exact(OP_LOCATOR_BYTES)
        .map(decode_op_locator)
        .collect()
}

/// Decode one fixed-width operation locator entry.
fn decode_op_locator(chunk: &[u8]) -> io::Result<SnapshotOpLocator> {
    let node = read_array::<8>(chunk, 0)?;
    let boot = read_array::<4>(chunk, 8)?;
    let seq = read_array::<8>(chunk, 12)?;
    let segment_seq = read_array::<4>(chunk, 20)?;
    let data_offset = read_array::<8>(chunk, 24)?;
    let data_len = read_array::<4>(chunk, 32)?;
    Ok(SnapshotOpLocator {
        id: OpId::new(
            editchain_core::NodeId(u64::from_le_bytes(node)),
            u32::from_le_bytes(boot),
            u64::from_le_bytes(seq),
        ),
        location: OpRecordLocation {
            segment_seq: u32::from_le_bytes(segment_seq),
            data_offset: u64::from_le_bytes(data_offset),
            data_len: u32::from_le_bytes(data_len),
        },
    })
}

/// Read one exact fixed-width byte array from a binary index record.
fn read_array<const N: usize>(chunk: &[u8], offset: usize) -> io::Result<[u8; N]> {
    chunk
        .get(offset..offset.saturating_add(N))
        .ok_or_else(|| invalid_data("truncated render snapshot index entry"))?
        .try_into()
        .map_err(|_error| invalid_data("invalid render snapshot index entry"))
}

/// Sum regular files immediately inside one immutable snapshot directory.
fn snapshot_size(path: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let metadata = entry?.metadata()?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

/// Delete obsolete immutable generations after the new generation is durable.
fn cleanup_stale_snapshots(schema_root: &Path, current_hash: &str) {
    let Ok(entries) = fs::read_dir(schema_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_snapshot_hash =
            name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
        if is_snapshot_hash && name != current_hash {
            drop(fs::remove_dir_all(entry.path()));
        }
    }
}

/// Flush a directory entry on platforms that support directory fsync.
#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Directory fsync is best-effort on non-Unix targets.
#[cfg(not(unix))]
const fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Construct a consistent invalid-data error.
fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
