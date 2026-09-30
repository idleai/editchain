//! Borrowed EC02/EC03 scanning shared by all segment readers.

use super::ec03::{self, FrameError};
use super::page::PAGE_MAGIC;

/// Largest record written or accepted by the EC02/EC03 readers and writer.
///
/// Large source payloads belong in the blob store. This bound also keeps a
/// record length distinct from the reserved `EC02` page marker.
pub const MAX_RECORD_BYTES: u32 = 64 * 1024 * 1024;

/// A complete page header or record borrowed from a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanItem<'a> {
    /// A page begins at this byte offset in the supplied segment.
    Page {
        /// Page sequence encoded in the header.
        sequence: u32,
        /// Offset of the page magic.
        offset: usize,
    },
    /// A complete record; no payload copy or operation decoding is performed.
    Record(RecordRef<'a>),
}

/// A complete record and its exact source location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordRef<'a> {
    /// Page sequence owning this record.
    pub(crate) page_sequence: u32,
    /// Offset of the record length prefix.
    pub(crate) offset: usize,
    /// Offset of the encoded operation, after length and flags.
    pub(crate) data_offset: usize,
    /// Record metadata flags, preserved independently of the payload.
    pub(crate) flags: u8,
    /// Complete encoded operation bytes.
    pub(crate) data: &'a [u8],
    /// Independent EC03 record checksum, absent in legacy EC02.
    pub(crate) checksum: Option<u32>,
}

/// Why a scanner stopped before clean EOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanErrorKind {
    /// A final page header or record was not fully written.
    IncompleteTail,
    /// Bytes where the first page header was expected have an invalid magic.
    InvalidMagic,
    /// A recognizable `EditChain` page marker names another format.
    UnsupportedFormat([u8; 4]),
    /// A record's declared length exceeds the supported bound.
    RecordTooLarge(u32),
    /// A complete EC03 frame failed validation.
    Frame(FrameError),
}

/// The first unread item and the reason it could not be consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanError {
    /// Offset at which the incomplete, invalid, or unsupported item begins.
    pub(crate) offset: usize,
    /// Structured scanner outcome.
    pub(crate) kind: ScanErrorKind,
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "segment scan at byte {}: {:?}", self.offset, self.kind)
    }
}

impl std::error::Error for ScanError {}

/// Scan EC02 pages or EC03 frames, yielding borrowed records and their offsets.
///
/// Clean EOF ends iteration. An incomplete tail, invalid header, unsupported
/// format, or oversized record yields one error and then ends iteration.
/// Callers may retain previously yielded complete records after an incomplete
/// final write; corruption and unsupported formats are separate outcomes.
#[derive(Debug)]
pub(crate) struct PageScanner<'a> {
    bytes: &'a [u8],
    offset: usize,
    page_sequence: Option<u32>,
    frame_end: Option<usize>,
    finished: bool,
}

impl<'a> PageScanner<'a> {
    /// Start scanning at the first page header in a segment.
    #[must_use]
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            offset: 0,
            page_sequence: None,
            frame_end: None,
            finished: false,
        }
    }

    /// Bytes consumed through the last complete header or record.
    #[must_use]
    pub(crate) const fn consumed(&self) -> usize {
        self.offset
    }

    /// Resume at a complete-record boundary inside an already opened page.
    pub(crate) const fn resume(bytes: &'a [u8], page_sequence: Option<u32>) -> Self {
        Self {
            bytes,
            offset: 0,
            page_sequence,
            frame_end: None,
            finished: false,
        }
    }

    pub(crate) const fn page_sequence(&self) -> Option<u32> {
        self.page_sequence
    }

    const fn error(&self, kind: ScanErrorKind) -> ScanError {
        ScanError {
            offset: self.offset,
            kind,
        }
    }

    fn next_item(&mut self) -> Result<ScanItem<'a>, ScanError> {
        let remaining = self.bytes.get(self.offset..).unwrap_or_default();
        if let Some(end) = self.frame_end {
            let record = ec03::scan_record(remaining)
                .map_err(|error| self.error(ScanErrorKind::Frame(error)))?;
            let item = RecordRef {
                page_sequence: self.page_sequence.unwrap_or_default(),
                offset: self.offset,
                data_offset: self.offset.saturating_add(5),
                flags: record.flags,
                data: record.data,
                checksum: Some(record.checksum),
            };
            self.offset = self.offset.saturating_add(record.length);
            if self.offset == end.saturating_sub(4) {
                self.offset = end;
                self.frame_end = None;
                self.page_sequence = None;
            }
            return Ok(ScanItem::Record(item));
        }
        let prefix: [u8; 4] = remaining
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| self.error(ScanErrorKind::IncompleteTail))?;
        if prefix == ec03::MAGIC {
            let frame = ec03::scan_frame(remaining).map_err(|error| {
                self.error(if error == FrameError::Incomplete {
                    ScanErrorKind::IncompleteTail
                } else {
                    ScanErrorKind::Frame(error)
                })
            })?;
            let offset = self.offset;
            let end = offset.saturating_add(frame.length);
            if frame.records.is_empty() {
                self.offset = end;
                self.page_sequence = None;
            } else {
                self.offset = offset.saturating_add(ec03::HEADER_BYTES);
                self.page_sequence = Some(frame.sequence);
                self.frame_end = Some(end);
            }
            return Ok(ScanItem::Page {
                sequence: frame.sequence,
                offset,
            });
        }
        if prefix == PAGE_MAGIC {
            let sequence = remaining
                .get(4..8)
                .and_then(|bytes| bytes.try_into().ok())
                .map(u32::from_le_bytes)
                .ok_or_else(|| self.error(ScanErrorKind::IncompleteTail))?;
            let offset = self.offset;
            self.offset = self.offset.saturating_add(8);
            self.page_sequence = Some(sequence);
            return Ok(ScanItem::Page { sequence, offset });
        }
        if matches!(prefix, [b'E', b'C', b'0'..=b'9', b'0'..=b'9']) {
            return Err(self.error(ScanErrorKind::UnsupportedFormat(prefix)));
        }
        let page_sequence = self
            .page_sequence
            .ok_or_else(|| self.error(ScanErrorKind::InvalidMagic))?;
        let length = u32::from_le_bytes(prefix);
        if length > MAX_RECORD_BYTES {
            return Err(self.error(ScanErrorKind::RecordTooLarge(length)));
        }
        let length = usize::try_from(length)
            .map_err(|_error| self.error(ScanErrorKind::RecordTooLarge(length)))?;
        let flags = remaining
            .get(4)
            .copied()
            .ok_or_else(|| self.error(ScanErrorKind::IncompleteTail))?;
        let end = 5usize.saturating_add(length);
        let data = remaining
            .get(5..end)
            .ok_or_else(|| self.error(ScanErrorKind::IncompleteTail))?;
        let record = RecordRef {
            page_sequence,
            offset: self.offset,
            data_offset: self.offset.saturating_add(5),
            flags,
            data,
            checksum: None,
        };
        self.offset = self.offset.saturating_add(end);
        Ok(ScanItem::Record(record))
    }
}

impl<'a> Iterator for PageScanner<'a> {
    type Item = Result<ScanItem<'a>, ScanError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished || self.offset == self.bytes.len() {
            return None;
        }
        let result = self.next_item();
        if result.is_err() {
            self.finished = true;
        }
        Some(result)
    }
}

impl std::iter::FusedIterator for PageScanner<'_> {}
