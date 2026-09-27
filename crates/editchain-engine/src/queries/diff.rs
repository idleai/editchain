//! Deterministic byte comparison, retaining both original sides and recorded edits.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{ByteRange, OpId, OpKind};

use super::{ChainQueries, ContentField, ContentQuery, ContentResult, HistoryEntry, Lookup};

/// A lossless single-replacement diff, independent of text encoding or UI geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ByteComparison {
    /// At least one complete side is unavailable; no comparison was fabricated.
    Unavailable,
    /// Both complete byte sequences are equal.
    Identical,
    /// Replacing `before` with the bytes in `after` reproduces the exact result.
    /// Common byte prefixes and suffixes are excluded; equal interior spans may
    /// remain. These are byte ranges, not line hunks or UTF-8 character boundaries.
    Changed {
        /// Half-open range in the before content.
        before: ByteRange,
        /// Half-open range in the after content.
        after: ByteRange,
    },
}

/// Compare any two recorded fields, retaining missing/conflicted operation status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDiff {
    /// Requested before-side field, also retained when the operation is missing.
    pub before_query: ContentQuery,
    /// Requested after-side field.
    pub after_query: ContentQuery,
    /// Exact before bytes or missing evidence.
    pub before: Lookup<ContentResult>,
    /// Exact after bytes or missing evidence.
    pub after: Lookup<ContentResult>,
    /// Computed only when both sides are completely available.
    pub comparison: ByteComparison,
}

/// One file revision's recorded snapshots, edit, and exact comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionDiff {
    /// Original revision, including path, stage, edit kind, and author.
    pub record: HistoryEntry,
    /// Explicit base content; absent base IDs are not assumed to mean an empty file.
    pub before: ContentResult,
    /// Explicit resulting content; deletions do not invent an empty snapshot.
    pub after: ContentResult,
    /// Recorded patch/replacement/full blob, retained even if snapshots are missing.
    pub edit: ContentResult,
    /// Comparison of complete snapshots only; no patch is silently applied.
    pub comparison: ByteComparison,
}

impl ChainQueries {
    /// Compare two exact recorded fields, including binary contents and empty files.
    ///
    /// # Errors
    /// Returns index or content IO errors.
    pub fn compare(
        &self,
        before_query: ContentQuery,
        after_query: ContentQuery,
    ) -> io::Result<ContentDiff> {
        let before = self.content(before_query)?;
        let after = self.content(after_query)?;
        let comparison = match (&before, &after) {
            (Lookup::Found(before), Lookup::Found(after)) => {
                compare_bytes(before.value.bytes(), after.value.bytes())?
            }
            _ => ByteComparison::Unavailable,
        };
        Ok(ContentDiff {
            before_query,
            after_query,
            before,
            after,
            comparison,
        })
    }

    /// Query one recorded file revision without deriving snapshots from partial edits.
    ///
    /// # Errors
    /// Returns `InvalidInput` for accepted non-file operations, or storage/index errors.
    pub fn diff(&self, revision: OpId) -> io::Result<Lookup<RevisionDiff>> {
        self.operation(revision)?.try_map(|record| {
            if !matches!(record.operation.kind, OpKind::File(_)) {
                return Err(crate::invalid_input(
                    "diff requires a recorded file revision",
                ));
            }
            let before = self.field_content(&record, ContentField::FileBase)?;
            let after = self.field_content(&record, ContentField::FileAfter)?;
            let edit = self.field_content(&record, ContentField::FileEdit)?;
            let comparison = compare_bytes(before.value.bytes(), after.value.bytes())?;
            Ok(RevisionDiff {
                record,
                before,
                after,
                edit,
                comparison,
            })
        })
    }
}

fn compare_bytes(before: Option<&[u8]>, after: Option<&[u8]>) -> io::Result<ByteComparison> {
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(ByteComparison::Unavailable);
    };
    if before == after {
        return Ok(ByteComparison::Identical);
    }
    let prefix = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    let suffix = before
        .iter()
        .rev()
        .take(before.len().saturating_sub(prefix))
        .zip(after.iter().rev().take(after.len().saturating_sub(prefix)))
        .take_while(|(a, b)| a == b)
        .count();
    let start = u64::try_from(prefix).map_err(crate::invalid_input)?;
    Ok(ByteComparison::Changed {
        before: ByteRange {
            start,
            end: u64::try_from(before.len().saturating_sub(suffix))
                .map_err(crate::invalid_input)?,
        },
        after: ByteRange {
            start,
            end: u64::try_from(after.len().saturating_sub(suffix)).map_err(crate::invalid_input)?,
        },
    })
}
