// Suppress unused_crate_dependencies warnings for crates consumed by other modules
// or by derive macros.
use crc as _;
use editchain_core as _;
use postcard as _;
#[cfg(test)]
use proptest as _;
use serde as _;

use crate::scan::{PageScanner, ScanErrorKind, ScanItem, MAX_RECORD_BYTES};

/// Page magic bytes — "EC" + version 02.
pub const PAGE_MAGIC: [u8; 4] = [0x45, 0x43, 0x30, 0x32]; // "EC02"

/// A framed page of operations.
///
/// Format: magic | `page_seq` (u32 LE) | records...
/// Each record: payload length (u32 LE) | flags (u8) | encoded operation.
/// EC02 contains no checksum or record count. The next page marker or EOF
/// terminates the page; record lengths are bounded below the reserved marker.
#[derive(Debug, Clone)]
pub struct Page {
    /// Magic bytes identifying the page format ("EC02").
    pub magic: [u8; 4],
    /// Monotonically increasing page sequence number.
    pub page_seq: u32,
    /// Records contained in this page.
    pub records: Vec<Record>,
}

/// A single record within a page.
#[derive(Debug, Clone)]
pub struct Record {
    /// Bit-flag field for record-level metadata.
    pub flags: u8,
    /// Encoded record payload.
    pub data: Vec<u8>,
}

impl Page {
    /// Create a new page with the given sequence number.
    #[must_use]
    pub const fn new(page_seq: u32) -> Self {
        Self {
            magic: PAGE_MAGIC,
            page_seq,
            records: Vec::new(),
        }
    }

    /// Add a record to this page.
    pub fn add_record(&mut self, flags: u8, data: Vec<u8>) {
        self.records.push(Record { flags, data });
    }
}

/// Encode a page into bytes.
///
/// # Errors
///
/// Rejects invalid magic or a record larger than [`MAX_RECORD_BYTES`].
pub fn encode_page(page: &Page) -> Result<Vec<u8>, PageEncodeError> {
    if page.magic != PAGE_MAGIC {
        return Err(PageEncodeError::InvalidMagic);
    }
    let mut buf = Vec::new();
    buf.extend_from_slice(&page.magic);
    buf.extend_from_slice(&page.page_seq.to_le_bytes());

    for record in &page.records {
        let len = u32::try_from(record.data.len())
            .ok()
            .filter(|length| *length <= MAX_RECORD_BYTES)
            .ok_or(PageEncodeError::RecordTooLarge(record.data.len()))?;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.push(record.flags);
        buf.extend_from_slice(&record.data);
    }

    Ok(buf)
}

/// A page cannot be represented by the supported EC02 writer/reader contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageEncodeError {
    /// Page magic must be the EC02 marker.
    InvalidMagic,
    /// Payload length exceeds the supported record limit.
    RecordTooLarge(usize),
}

impl std::fmt::Display for PageEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot encode EC02 page: {self:?}")
    }
}

impl std::error::Error for PageEncodeError {}

/// Decode a page from bytes.
///
/// Returns the first page, retaining complete records before an incomplete
/// trailing write. Invalid and unsupported input returns `None`. Use
/// [`PageScanner`] when all pages, locations, or diagnostics are needed.
#[must_use]
pub fn decode_page(bytes: &[u8]) -> Option<Page> {
    let mut scanner = PageScanner::new(bytes);
    let ScanItem::Page { sequence, .. } = scanner.next()?.ok()? else {
        return None;
    };
    let mut page = Page::new(sequence);
    for item in scanner {
        match item {
            Ok(ScanItem::Record(record)) => page.add_record(record.flags, record.data.to_vec()),
            Ok(ScanItem::Page { .. }) => break,
            Err(error) if error.kind == ScanErrorKind::IncompleteTail => break,
            Err(_) => return None,
        }
    }
    Some(page)
}
