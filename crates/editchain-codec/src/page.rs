#[cfg(not(feature = "use-std"))]
extern crate alloc;

// Suppress unused_crate_dependencies warnings for crates consumed by other modules
// or by derive macros.
#[cfg_attr(not(feature = "use-std"), allow(unused_extern_crates))]
use crc as _;
#[cfg_attr(not(feature = "use-std"), allow(unused_extern_crates))]
use editchain_core as _;
#[cfg_attr(not(feature = "use-std"), allow(unused_extern_crates))]
use postcard as _;
#[cfg(test)]
use proptest as _;
#[cfg_attr(not(feature = "use-std"), allow(unused_extern_crates))]
use serde as _;

/// Page magic bytes — "EC" + version 02.
pub const PAGE_MAGIC: [u8; 4] = [0x45, 0x43, 0x30, 0x32]; // "EC02"

/// A framed page of operations.
///
/// Format: magic | `page_seq` (u32 LE) | records... | optional CRC32
/// Each record: `varint_len` | flags (u8) | `encoded_op` | optional CRC32
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
#[must_use]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "Record length fits in u32; chain pages are <4 GiB"
)]
pub fn encode_page(page: &Page) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&page.magic);
    buf.extend_from_slice(&page.page_seq.to_le_bytes());

    for record in &page.records {
        // Varint-length prefix (simplified — just u32 LE for now)
        let len = record.data.len() as u32;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.push(record.flags);
        buf.extend_from_slice(&record.data);
    }

    buf
}

/// Decode a page from bytes.
///
/// Power-loss rule: ignore partial trailing records.
#[must_use]
#[expect(
    clippy::as_conversions,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "Page decoding uses bounded offsets; all bounds checked before access"
)]
pub fn decode_page(bytes: &[u8]) -> Option<Page> {
    if bytes.len() < 8 {
        return None;
    }

    let magic = &bytes[..4];
    if magic != PAGE_MAGIC {
        return None;
    }

    let page_seq = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let mut page = Page::new(page_seq);
    let mut offset = 8;

    while offset + 4 <= bytes.len() {
        // Read length prefix
        let len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?);
        offset += 4;

        if offset + 1 + len as usize > bytes.len() {
            // Partial trailing record — stop (power-loss tolerance)
            break;
        }

        let flags = bytes[offset];
        offset += 1;

        let data = bytes[offset..offset + len as usize].to_vec();
        offset += len as usize;

        page.add_record(flags, data);
    }

    Some(page)
}
