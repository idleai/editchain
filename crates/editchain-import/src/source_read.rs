//! One captured JSONL source shared by cursor validation and provider derivation.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::cancellation::ImportCancellation;
use crate::error::ImportError;
use crate::ids::hash_raw;
use crate::sink::CursorValue;

/// Bounds checked before allocating source records or capturing source bytes.
#[derive(Debug, Clone, Copy)]
pub struct SourceReadLimits {
    /// Maximum captured file size, including an incomplete final record.
    pub source_bytes: u64,
    /// Maximum physical record size, including its newline when complete.
    pub record_bytes: u64,
    /// Maximum complete physical records in a source generation.
    pub records: u64,
}

impl Default for SourceReadLimits {
    fn default() -> Self {
        Self {
            source_bytes: 512 * 1024 * 1024,
            record_bytes: 64 * 1024 * 1024,
            records: 1_000_000,
        }
    }
}

/// Resource and cancellation controls retained by a captured source.
#[derive(Debug, Clone, Default)]
pub struct SourceReadControl {
    /// Bounds on source bytes and physical records.
    pub limits: SourceReadLimits,
    /// Shared cancellation signal, also used by historical record replay.
    pub cancellation: ImportCancellation,
}

/// One complete physical source occurrence, retaining its exact newline bytes.
#[derive(Debug, Clone)]
pub struct LineWithHash {
    /// Raw JSONL bytes, including the newline.
    pub data: Vec<u8>,
    /// BLAKE3 of exactly `data`.
    pub hash: [u8; 32],
}

/// Continuity of a captured source relative to its accepted cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceReadState {
    /// No accepted cursor exists.
    Fresh,
    /// Accepted bytes match and there are no new complete records.
    Unchanged,
    /// Accepted bytes match and complete records follow them.
    Append,
    /// Accepted bytes differ or are missing; a new generation is proposed.
    Rewritten,
}

/// A bounded private copy of a source, retained for the lifetime of a read plan.
#[derive(Debug)]
struct CapturedSource {
    _directory: tempfile::TempDir,
    path: PathBuf,
    original: PathBuf,
    file_size: u64,
    limits: SourceReadLimits,
    cancellation: ImportCancellation,
}

impl CapturedSource {
    fn capture(
        path: &Path,
        limits: SourceReadLimits,
        cancellation: &ImportCancellation,
    ) -> Result<Self, ImportError> {
        cancellation.check(path)?;
        let mut source = File::open(path)?;
        let file_size = source.metadata()?.len();
        check_limit(path, "source bytes", file_size, limits.source_bytes)?;
        let directory = tempfile::Builder::new()
            .prefix("editchain-source-")
            .tempdir()?;
        // Keep the original basename for helpers that use the rollout filename
        // as a legacy identity fallback. The private directory owns its lifetime.
        let captured_path = directory.path().join(path.file_name().ok_or_else(|| {
            ImportError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "source path has no filename",
            ))
        })?);
        let mut captured = File::create(&captured_path)?;
        let copied = copy_checked(
            &mut (&mut source).take(file_size),
            &mut captured,
            path,
            cancellation,
        )?;
        if copied != file_size {
            return Err(ImportError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("source {} shrank during capture", path.display()),
            )));
        }
        let mut permissions = captured.metadata()?.permissions();
        permissions.set_readonly(true);
        captured.set_permissions(permissions)?;
        Ok(Self {
            _directory: directory,
            path: captured_path,
            original: path.to_path_buf(),
            file_size,
            limits,
            cancellation: cancellation.clone(),
        })
    }

    fn read(&self, cursor: Option<&CursorValue>) -> Result<CapturedRecords, ImportError> {
        let offset = cursor.map_or(0, |value| value.byte_offset);
        let mut file = File::open(&self.path)?;
        let mut hasher = blake3::Hasher::new();
        let prefix_bytes = copy_checked(
            &mut (&mut file).take(offset),
            &mut hasher,
            &self.original,
            &self.cancellation,
        )?;
        let prefix_hash = *hasher.finalize().as_bytes();
        if let Some(cursor) = cursor {
            let empty_legacy = offset == 0 && cursor.content_hash == [0; 32];
            if prefix_bytes != offset || (!empty_legacy && prefix_hash != cursor.content_hash) {
                return Err(ImportError::SourceGenerationChanged {
                    path: self.original.clone(),
                    expected_size: cursor.byte_offset,
                    actual_size: self.file_size,
                });
            }
        }
        let mut reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut byte_offset = offset;
        let mut ops_emitted = cursor.map_or(0, |value| value.ops_emitted);
        let partial = loop {
            self.cancellation.check(&self.original)?;
            let mut data = Vec::new();
            let count = (&mut reader)
                .take(self.limits.record_bytes.saturating_add(1))
                .read_until(b'\n', &mut data)?;
            check_limit(
                &self.original,
                "record bytes",
                u64::try_from(count).unwrap_or(u64::MAX),
                self.limits.record_bytes,
            )?;
            if data.last() != Some(&b'\n') {
                break (!data.is_empty()).then(|| data.iter().all(u8::is_ascii_whitespace));
            }
            let _: &mut blake3::Hasher = hasher.update(&data);
            byte_offset = byte_offset
                .checked_add(u64::try_from(count).map_err(std::io::Error::other)?)
                .ok_or_else(|| ImportError::CursorStore("source offset exhausted".into()))?;
            ops_emitted = ops_emitted
                .checked_add(1)
                .ok_or_else(|| ImportError::CursorStore("source ordinal exhausted".into()))?;
            check_limit(
                &self.original,
                "source records",
                ops_emitted,
                self.limits.records,
            )?;
            lines.push(LineWithHash {
                hash: hash_raw(&data),
                data,
            });
        };
        Ok(CapturedRecords {
            lines,
            partial,
            checkpoint: CursorValue {
                accepted_generation: cursor.and_then(|value| value.accepted_generation),
                file_size: self.file_size,
                byte_offset,
                ops_emitted,
                content_hash: *hasher.finalize().as_bytes(),
                content_hash_version: 1,
                source_node: cursor.and_then(|value| value.source_node),
                normalization_version: cursor.map_or(0, |value| value.normalization_version),
                materialization: cursor.and_then(|value| value.materialization.clone()),
                session_title_hash: cursor.and_then(|value| value.session_title_hash),
            },
        })
    }
}

#[derive(Debug)]
struct CapturedRecords {
    lines: Vec<LineWithHash>,
    checkpoint: CursorValue,
    // None: no partial record. Some(true): partial whitespace only.
    partial: Option<bool>,
}

/// Immutable input and proposed checkpoint for one source import.
///
/// Capture stops at the size observed on the opened file. Growth after that
/// point is deferred. Prefix checking, complete records, partial-line status,
/// metadata replay, and helper projection all use this same private copy.
/// Constructing a plan never mutates a cursor store or accepts operations.
#[derive(Debug)]
pub struct SourceReadPlan {
    source: CapturedSource,
    records: CapturedRecords,
    state: SourceReadState,
    generation: u32,
    start_seq: u64,
}

impl SourceReadPlan {
    /// Capture a source and propose its continuity, generation, and cursor.
    ///
    /// # Errors
    ///
    /// Returns source IO/limit errors or a cursor error when a rewritten
    /// source would exhaust the generation counter.
    pub fn capture(
        path: &Path,
        cursor: Option<&CursorValue>,
        generation: u32,
        limits: SourceReadLimits,
    ) -> Result<Self, ImportError> {
        Self::capture_reserved(path, cursor, generation, None, limits)
    }

    /// Capture with any source prefix reserved before an interrupted append.
    /// The reservation constrains ID reuse while the accepted cursor continues
    /// to determine which records must be replayed.
    ///
    /// # Errors
    ///
    /// Returns source IO/limit errors, invalid reservation state, or generation
    /// exhaustion. A reserved prefix that changed starts another generation.
    pub fn capture_reserved(
        path: &Path,
        cursor: Option<&CursorValue>,
        generation: u32,
        reservation: Option<&CursorValue>,
        limits: SourceReadLimits,
    ) -> Result<Self, ImportError> {
        Self::capture_controlled(
            path,
            cursor,
            generation,
            reservation,
            &SourceReadControl {
                limits,
                cancellation: ImportCancellation::default(),
            },
        )
    }

    /// Capture a reserved source with cooperative cancellation during copying,
    /// prefix verification, and record reads, including later metadata replay.
    ///
    /// # Errors
    ///
    /// Returns the same errors as `capture_reserved`, or cancellation.
    pub fn capture_controlled(
        path: &Path,
        cursor: Option<&CursorValue>,
        generation: u32,
        reservation: Option<&CursorValue>,
        control: &SourceReadControl,
    ) -> Result<Self, ImportError> {
        let accepted_generation = cursor
            .and_then(|value| value.accepted_generation)
            .unwrap_or(generation);
        let highest_generation = generation.max(accepted_generation);
        let source = CapturedSource::capture(path, control.limits, &control.cancellation)?;
        let Some(reservation) = reservation else {
            return Self::from_source(source, cursor, accepted_generation, highest_generation);
        };
        let reserved_generation = reservation.accepted_generation.ok_or_else(|| {
            ImportError::CursorStore("source reservation has no generation".into())
        })?;
        match source.read(Some(reservation)) {
            Ok(_) => {
                if cursor.is_some() && reserved_generation != accepted_generation {
                    let mut plan =
                        Self::from_source(source, None, reserved_generation, highest_generation)?;
                    plan.state = SourceReadState::Rewritten;
                    return Ok(plan);
                }
                Self::from_source(source, cursor, reserved_generation, highest_generation)
            }
            Err(ImportError::SourceGenerationChanged { .. }) => {
                let next = highest_generation
                    .max(reserved_generation)
                    .checked_add(1)
                    .ok_or_else(|| {
                        ImportError::CursorStore(format!(
                            "source generation exhausted for {}",
                            path.display()
                        ))
                    })?;
                let mut plan = Self::from_source(source, None, next, next)?;
                plan.state = SourceReadState::Rewritten;
                Ok(plan)
            }
            Err(error) => Err(error),
        }
    }

    fn from_source(
        source: CapturedSource,
        cursor: Option<&CursorValue>,
        generation: u32,
        generation_floor: u32,
    ) -> Result<Self, ImportError> {
        let (mut records, state, generation, start_seq) = match source.read(cursor) {
            Ok(records) => {
                let state = match cursor {
                    None => SourceReadState::Fresh,
                    Some(_) if records.lines.is_empty() => SourceReadState::Unchanged,
                    Some(_) => SourceReadState::Append,
                };
                (
                    records,
                    state,
                    generation,
                    cursor.map_or(0, |value| value.ops_emitted),
                )
            }
            Err(ImportError::SourceGenerationChanged { .. }) => {
                let generation =
                    generation
                        .max(generation_floor)
                        .checked_add(1)
                        .ok_or_else(|| {
                            ImportError::CursorStore(format!(
                                "source generation exhausted for {}",
                                source.original.display()
                            ))
                        })?;
                (
                    source.read(None)?,
                    SourceReadState::Rewritten,
                    generation,
                    0,
                )
            }
            Err(error) => return Err(error),
        };
        records.checkpoint.accepted_generation = Some(generation);
        Ok(Self {
            source,
            records,
            state,
            generation,
            start_seq,
        })
    }

    /// Source continuity established by byte-exact accepted-prefix validation.
    #[must_use]
    pub const fn state(&self) -> SourceReadState {
        self.state
    }

    /// Proposed generation, which must be staged only after successful emission.
    #[must_use]
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Number of already accepted physical records in this generation.
    #[must_use]
    pub const fn start_seq(&self) -> u64 {
        self.start_seq
    }

    /// Newly complete records, in physical source order.
    #[must_use]
    pub fn lines(&self) -> &[LineWithHash] {
        &self.records.lines
    }

    /// Checkpoint proposed for successful acceptance of every new record.
    #[must_use]
    pub const fn checkpoint(&self) -> &CursorValue {
        &self.records.checkpoint
    }

    /// Path for a helper to read the exact captured bytes. The path exists for
    /// this plan's lifetime and retains the source's original filename.
    #[must_use]
    pub fn captured_path(&self) -> &Path {
        &self.source.path
    }

    /// Whether a trailing partial record exists, and whether it is blank.
    #[must_use]
    pub const fn partial(&self) -> Option<bool> {
        self.records.partial
    }

    /// Read complete historical evidence for a metadata-only version upgrade.
    ///
    /// # Errors
    ///
    /// Returns IO/limit errors reading the captured copy.
    pub fn all_lines(&self) -> Result<Vec<LineWithHash>, ImportError> {
        Ok(self.source.read(None)?.lines)
    }

    pub(crate) fn check_cancellation(&self) -> Result<(), ImportError> {
        self.source.cancellation.check(&self.source.original)
    }
}

/// Compatibility reader for callers that handle generation changes themselves.
///
/// # Errors
///
/// Returns IO/limit errors or `SourceGenerationChanged` for a mismatched cursor.
pub fn read_session_file(
    path: &Path,
    cursor: Option<&CursorValue>,
) -> Result<(Vec<LineWithHash>, u64, CursorValue), ImportError> {
    let source = CapturedSource::capture(
        path,
        SourceReadLimits::default(),
        &ImportCancellation::default(),
    )?;
    let records = source.read(cursor)?;
    let bytes = records
        .checkpoint
        .byte_offset
        .saturating_sub(cursor.map_or(0, |c| c.byte_offset));
    Ok((records.lines, bytes, records.checkpoint))
}

fn copy_checked(
    source: &mut impl Read,
    target: &mut impl Write,
    path: &Path,
    cancellation: &ImportCancellation,
) -> Result<u64, ImportError> {
    let mut buffer = vec![0; 64 * 1024].into_boxed_slice();
    let mut copied = 0_u64;
    loop {
        cancellation.check(path)?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok(copied);
        }
        if let Some(bytes) = buffer.get(..count) {
            target.write_all(bytes)?;
        }
        copied = copied
            .checked_add(u64::try_from(count).map_err(std::io::Error::other)?)
            .ok_or_else(|| std::io::Error::other("source copy length exhausted"))?;
    }
}

fn check_limit(
    path: &Path,
    resource: &'static str,
    size: u64,
    limit: u64,
) -> Result<(), ImportError> {
    if size > limit {
        return Err(ImportError::ResourceLimit {
            path: path.to_path_buf(),
            resource,
            limit,
        });
    }
    Ok(())
}
