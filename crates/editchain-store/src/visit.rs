//! Bounded sequential reads for conversions and whole-history audits.

use crate::format::scan::{PageScanner, ScanErrorKind, ScanItem};
use crate::{LogReadStats, RecordVisitor};
use std::{fs, io, path::Path};

/// Visit exact records one segment at a time, validating complete EC03 frames.
/// The caller must hold the source writer lock when a frozen snapshot is required.
/// Incomplete trailing writes are counted; corruption is never skipped.
/// # Errors
/// Returns segment, checksum, or visitor errors.
pub fn visit_records(root: &Path, visitor: &mut RecordVisitor<'_>) -> io::Result<LogReadStats> {
    let mut stats = LogReadStats::default();
    for sequence in crate::segment::segment_sequences(root)? {
        let bytes = fs::read(root.join(format!("{sequence:06}.eclog")))?;
        for item in PageScanner::new(&bytes) {
            match item {
                Ok(ScanItem::Page { .. }) => {}
                Ok(ScanItem::Record(record)) => visitor(record.flags, record.data)?,
                Err(error) if error.kind == ScanErrorKind::IncompleteTail => {
                    stats.incomplete_tails = stats.incomplete_tails.saturating_add(1);
                    break;
                }
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
            }
        }
    }
    Ok(stats)
}
