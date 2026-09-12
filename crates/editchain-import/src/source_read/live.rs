//! Append-only provider observation after one strict accepted-prefix bootstrap.
//!
//! Codex owns its active rollout as an append log. Replacement, truncation and
//! same-size writes force strict recapture; arbitrary rewrite-and-append cannot
//! be proved absent without rereading the prefix and is outside this contract.

use super::{LineWithHash, SourceReadControl, SourceReadPlan};
use crate::{sink::CursorValue, ImportError};
use std::{
    fs::{File, Metadata},
    io::{self, BufRead as _, BufReader, Read as _, Seek as _, SeekFrom},
    path::Path,
};

const BATCH_RECORDS: usize = 512;
const BATCH_BYTES: u64 = 4 * 1024 * 1024;

/// Retained hash and physical cursor for one append-only provider generation.
#[derive(Debug, Clone)]
pub struct LiveRead {
    checkpoint: CursorValue,
    hasher: blake3::Hasher,
    observed: Metadata,
    at_eof: bool,
}

/// A speculative source advance, adopted only after the durable transaction.
#[derive(Debug)]
pub struct LiveReadBatch {
    /// Proposed retained cursor and hasher.
    pub next: LiveRead,
    /// Newly complete physical records, bounded by count and payload bytes.
    pub lines: Vec<LineWithHash>,
    /// Actual bytes read, including any retried incomplete tail.
    pub bytes_read: u64,
}

impl LiveRead {
    /// Retain the strict bootstrap's byte hash. Historical records are read once
    /// here and also seed the provider reducer; ordinary polls never revisit them.
    ///
    /// # Errors
    /// Returns IO errors or a detected replacement during bootstrap.
    pub fn bootstrap(
        path: &Path,
        plan: &SourceReadPlan,
    ) -> Result<(Self, Vec<LineWithHash>), ImportError> {
        let observed = std::fs::metadata(path)?;
        if observed.len() < plan.checkpoint().file_size {
            return Err(io::Error::other("source shrank during live bootstrap").into());
        }
        let lines = plan.all_lines()?;
        let mut hasher = blake3::Hasher::new();
        for line in &lines {
            let _: &mut blake3::Hasher = hasher.update(&line.data);
        }
        if hasher.finalize().as_bytes() != &plan.checkpoint().content_hash {
            return Err(io::Error::other("live bootstrap hash mismatch").into());
        }
        Ok((
            Self {
                checkpoint: plan.checkpoint().clone(),
                hasher,
                at_eof: observed.len() == plan.checkpoint().file_size,
                observed,
            },
            lines,
        ))
    }

    /// The latest committed physical source cursor.
    #[must_use]
    pub const fn checkpoint(&self) -> &CursorValue {
        &self.checkpoint
    }

    /// A bounded read stopped before reaching the observed end of the source.
    #[must_use]
    pub fn has_more(&self) -> bool {
        !self.at_eof && self.checkpoint.byte_offset < self.observed.len()
    }

    /// Inspect only appended bytes. Invalid continuity requires strict recapture.
    ///
    /// # Errors
    /// Returns source IO, framing, resource limit or continuity errors.
    pub fn poll(
        &self,
        path: &Path,
        control: &SourceReadControl,
    ) -> Result<LiveReadBatch, ImportError> {
        control.cancellation.check(path)?;
        let mut file = File::open(path)?;
        let observed = file.metadata()?;
        check_continuity(&self.observed, &observed)?;
        if observed.len() > control.limits.source_bytes {
            return Err(ImportError::ResourceLimit {
                path: path.into(),
                resource: "source bytes",
                limit: control.limits.source_bytes,
            });
        }
        let unchanged = observed.len() == self.observed.len()
            && observed.modified()? == self.observed.modified()?;
        let mut next = self.clone();
        next.observed = observed;
        let mut batch = LiveReadBatch {
            next,
            lines: Vec::new(),
            bytes_read: 0,
        };
        if unchanged && self.at_eof {
            return Ok(batch);
        }
        let _offset = file.seek(SeekFrom::Start(self.checkpoint.byte_offset))?;
        let mut input = BufReader::new(
            file.take(
                batch
                    .next
                    .observed
                    .len()
                    .saturating_sub(self.checkpoint.byte_offset),
            ),
        );
        batch.next.at_eof = false;
        while batch.lines.len() < BATCH_RECORDS && batch.bytes_read < BATCH_BYTES {
            control.cancellation.check(path)?;
            let mut data = Vec::new();
            let count = (&mut input)
                .take(control.limits.record_bytes.saturating_add(1))
                .read_until(b'\n', &mut data)?;
            let count = u64::try_from(count).map_err(io::Error::other)?;
            if count > control.limits.record_bytes {
                return Err(ImportError::ResourceLimit {
                    path: path.into(),
                    resource: "record bytes",
                    limit: control.limits.record_bytes,
                });
            }
            batch.bytes_read = batch.bytes_read.saturating_add(count);
            if data.last() != Some(&b'\n') {
                batch.next.at_eof = true;
                break;
            }
            batch.next.accept(&data, control, path)?;
            batch.lines.push(LineWithHash {
                hash: *blake3::hash(&data).as_bytes(),
                data,
            });
        }
        batch.next.checkpoint.file_size = batch.next.observed.len();
        batch.next.checkpoint.content_hash = *batch.next.hasher.finalize().as_bytes();
        Ok(batch)
    }

    fn accept(
        &mut self,
        data: &[u8],
        control: &SourceReadControl,
        path: &Path,
    ) -> Result<(), ImportError> {
        let cursor = &mut self.checkpoint;
        cursor.byte_offset = cursor
            .byte_offset
            .checked_add(u64::try_from(data.len()).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("live byte offset exhausted"))?;
        cursor.ops_emitted = cursor
            .ops_emitted
            .checked_add(1)
            .ok_or_else(|| io::Error::other("live ordinal exhausted"))?;
        if cursor.ops_emitted > control.limits.records {
            return Err(ImportError::ResourceLimit {
                path: path.into(),
                resource: "source records",
                limit: control.limits.records,
            });
        }
        let _: &mut blake3::Hasher = self.hasher.update(data);
        Ok(())
    }
}

fn check_continuity(previous: &Metadata, current: &Metadata) -> io::Result<()> {
    #[cfg(unix)]
    let replaced = {
        use std::os::unix::fs::MetadataExt as _;
        previous.dev() != current.dev() || previous.ino() != current.ino()
    };
    #[cfg(not(unix))]
    let replaced = previous.created().ok() != current.created().ok();
    if replaced
        || current.len() < previous.len()
        || (current.len() == previous.len() && current.modified()? != previous.modified()?)
    {
        Err(io::Error::other(
            "provider source changed; strict bootstrap required",
        ))
    } else {
        Ok(())
    }
}
