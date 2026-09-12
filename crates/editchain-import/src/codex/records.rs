//! A bounded batch of complete physical records shared by offline and live import.

use crate::{sink::CursorValue, source_read::LineWithHash, ImportError};

pub(super) struct RecordBatch<'a> {
    pub(super) lines: &'a [LineWithHash],
    pub(super) start: u64,
    pub(super) checkpoint: &'a CursorValue,
    pub(super) check: &'a dyn Fn() -> Result<(), ImportError>,
}

impl RecordBatch<'_> {
    pub(super) fn check_cancellation(&self) -> Result<(), ImportError> {
        (self.check)()
    }
}
