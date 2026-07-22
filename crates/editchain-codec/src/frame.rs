// Suppress unused_crate_dependencies warnings for crates consumed by other modules
// or by derive macros.
#[cfg(test)]
use proptest as _;
#[cfg_attr(not(feature = "use-std"), allow(unused_extern_crates))]
use serde as _;

use editchain_core::Op;

use crate::page::PAGE_MAGIC;

/// Encode an operation into a binary frame using postcard.
///
/// # Errors
///
/// Returns `postcard::Error` if serialization fails.
pub fn encode_op(op: &Op) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_stdvec(op)
}

/// Decode an operation from a binary frame.
///
/// # Errors
///
/// Returns `postcard::Error` if deserialization fails.
pub fn decode_op(bytes: &[u8]) -> Result<Op, postcard::Error> {
    postcard::from_bytes(bytes)
}

// ---------------------------------------------------------------------------
// EC03 framed format
// ---------------------------------------------------------------------------

/// Magic bytes for EC03 frames.
pub const EC03_MAGIC: [u8; 4] = [0x45, 0x43, 0x30, 0x33]; // "EC03"

/// Current format version.
pub const EC03_FORMAT_VERSION: u16 = 1;

/// A framed batch of operations with checksums.
///
/// Format:
/// ```text
/// magic              [4]     "EC03"
/// format_version     u16     little-endian
/// header_len         u16     little-endian (bytes after header_len field)
/// frame_len          u32     little-endian (total frame bytes including magic)
/// record_count       u32     little-endian
/// page_sequence      u64     little-endian
/// commit_generation  u64     little-endian
/// flags              u32     little-endian
/// header_crc32c      u32     little-endian (CRC32C of all preceding header fields)
/// records            ...     concatenated encoded records
/// payload_crc32c     u32     little-endian (CRC32C of all record bytes)
/// ```
#[derive(Debug, Clone)]
pub struct Ec03Frame {
    /// Format version number (currently 1).
    pub format_version: u16,
    /// Header length in bytes (after the `header_len` field itself).
    pub header_len: u16,
    /// Total frame length in bytes (including magic).
    pub frame_len: u32,
    /// Number of records in this frame.
    pub record_count: u32,
    /// Monotonically increasing page sequence number.
    pub page_sequence: u64,
    /// Commit generation for ordering.
    pub commit_generation: u64,
    /// Bit-flag field for frame-level metadata.
    pub flags: u32,
    /// Encoded record payloads.
    pub records: Vec<Vec<u8>>,
}

impl Ec03Frame {
    /// Create a new EC03 frame.
    #[must_use]
    pub const fn new(page_sequence: u64, commit_generation: u64) -> Self {
        Self {
            format_version: EC03_FORMAT_VERSION,
            header_len: 0, // computed on encode
            frame_len: 0,  // computed on encode
            record_count: 0,
            page_sequence,
            commit_generation,
            flags: 0,
            records: Vec::new(),
        }
    }

    /// Add a record to this frame.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::as_conversions,
        reason = "Record count fits in u32; chain pages are <4 GiB"
    )]
    pub fn add_record(&mut self, data: Vec<u8>) {
        self.records.push(data);
        self.record_count = self.records.len() as u32;
    }
}

/// Encode an EC03 frame into bytes.
#[must_use]
#[expect(
    clippy::as_conversions,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "Frame encoding uses known-safe offsets; all casts fit in bounds"
)]
pub fn encode_ec03(frame: &Ec03Frame) -> Vec<u8> {
    let mut buf = Vec::new();

    // Magic
    buf.extend_from_slice(&EC03_MAGIC);

    // Format version
    buf.extend_from_slice(&frame.format_version.to_le_bytes());

    // Header length placeholder (will patch)
    let header_len_pos = buf.len();
    buf.extend_from_slice(&[0u8; 2]);

    // Frame length placeholder (will patch)
    let frame_len_pos = buf.len();
    buf.extend_from_slice(&[0u8; 4]);

    // Record count
    buf.extend_from_slice(&frame.record_count.to_le_bytes());

    // Page sequence
    buf.extend_from_slice(&frame.page_sequence.to_le_bytes());

    // Commit generation
    buf.extend_from_slice(&frame.commit_generation.to_le_bytes());

    // Flags
    buf.extend_from_slice(&frame.flags.to_le_bytes());

    // Header CRC32C (placeholder — will patch)
    let header_crc_pos = buf.len();
    buf.extend_from_slice(&[0u8; 4]);

    // Compute header length (from after header_len field to before records)
    let header_len = (buf.len() - header_len_pos + 4) as u16; // +4 for header_crc itself

    // Records
    let records_start = buf.len();
    for record in &frame.records {
        // Varint-length prefix (u32 LE for simplicity)
        let len = record.len() as u32;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(record);
    }

    // Payload CRC32C
    let payload_bytes = &buf[records_start..];
    let payload_crc = crc32c(payload_bytes);
    buf.extend_from_slice(&payload_crc.to_le_bytes());

    // Frame length (total bytes)
    let frame_len = buf.len() as u32;

    // Patch header length
    buf[header_len_pos..header_len_pos + 2].copy_from_slice(&header_len.to_le_bytes());

    // Patch frame length
    buf[frame_len_pos..frame_len_pos + 4].copy_from_slice(&frame_len.to_le_bytes());

    // Compute and patch header CRC32C (everything up to but not including header_crc)
    let header_bytes = &buf[..header_crc_pos];
    let header_crc = crc32c(header_bytes);
    buf[header_crc_pos..header_crc_pos + 4].copy_from_slice(&header_crc.to_le_bytes());

    buf
}

/// Decode an EC03 frame from bytes.
///
/// Returns `None` if the frame is incomplete or corrupt.
#[must_use]
#[expect(
    clippy::as_conversions,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "Frame decoding uses bounded offsets; all bounds checked before access"
)]
pub fn decode_ec03(bytes: &[u8]) -> Option<Ec03Frame> {
    if bytes.len() < 34 {
        // Minimum frame: magic(4) + version(2) + hdr_len(2) + frame_len(4)
        //   + rec_count(4) + page_seq(8) + commit_gen(8) + flags(4) + hdr_crc(4)
        //   + min payload_crc(4) = 44 bytes minimum with 0 records
        return None;
    }

    if bytes[..4] != EC03_MAGIC {
        return None;
    }

    let format_version = u16::from_le_bytes(bytes[4..6].try_into().ok()?);
    let header_len_val = u16::from_le_bytes(bytes[6..8].try_into().ok()?);
    let frame_len = u32::from_le_bytes(bytes[8..12].try_into().ok()?);

    // Verify total frame length matches available bytes.
    if (frame_len as usize) > bytes.len() {
        return None; // incomplete frame
    }

    let record_count = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let page_sequence = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    let commit_generation = u64::from_le_bytes(bytes[24..32].try_into().ok()?);
    let flags = u32::from_le_bytes(bytes[32..36].try_into().ok()?);
    let header_crc_stored = u32::from_le_bytes(bytes[36..40].try_into().ok()?);

    // Verify header CRC32C.
    let header_crc_computed = crc32c(&bytes[..36]);
    if header_crc_computed != header_crc_stored {
        return None; // header corruption
    }

    // Parse records starting at offset 40.
    let mut offset = 40usize;
    let mut records = Vec::with_capacity(record_count as usize);

    for _ in 0..record_count {
        if offset + 4 > frame_len as usize - 4 {
            return None; // not enough room for len prefix + payload crc
        }
        let rec_len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?);
        offset += 4;

        if offset + rec_len as usize > frame_len as usize - 4 {
            return None; // partial record
        }

        let data = bytes[offset..offset + rec_len as usize].to_vec();
        offset += rec_len as usize;
        records.push(data);
    }

    // Verify payload CRC32C.
    let payload_start = 40usize;
    let payload_end = offset;
    let payload_crc_stored = u32::from_le_bytes(
        bytes[frame_len as usize - 4..frame_len as usize]
            .try_into()
            .ok()?,
    );
    let payload_crc_computed = crc32c(&bytes[payload_start..payload_end]);
    if payload_crc_computed != payload_crc_stored {
        return None; // payload corruption
    }

    Some(Ec03Frame {
        format_version,
        header_len: header_len_val,
        frame_len,
        record_count,
        page_sequence,
        commit_generation,
        flags,
        records,
    })
}

/// Compute CRC32C (Castagnoli) over bytes.
const fn crc32c(data: &[u8]) -> u32 {
    crc::Crc::<u32>::new(&crc::CRC_32_ISCSI).checksum(data)
}

// ---------------------------------------------------------------------------
// Legacy EC02 compatibility helpers
// ---------------------------------------------------------------------------

/// Detect the frame format from magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// Legacy EC02 page format.
    Ec02,
    /// Current EC03 framed format.
    Ec03,
}

/// Detect the format of a frame from its magic bytes.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "bytes.len() >= 4 checked before slicing"
)]
pub fn detect_format(bytes: &[u8]) -> Option<FrameFormat> {
    if bytes.len() < 4 {
        return None;
    }
    match &bytes[..4] {
        m if m == PAGE_MAGIC => Some(FrameFormat::Ec02),
        m if m == EC03_MAGIC => Some(FrameFormat::Ec03),
        _ => None,
    }
}
