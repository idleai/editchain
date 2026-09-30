//! EC03 version 2: bounded frames with independent record checksums.

use super::page::Record;
use super::scan::MAX_RECORD_BYTES;

pub(crate) const MAGIC: [u8; 4] = *b"EC03";
/// Version 1 was an unwired prototype. Version 2 is the durable wire contract.
pub const EC03_FORMAT_VERSION: u16 = 2;
/// Maximum frame size, independent of the segment rollover target.
pub const MAX_FRAME_BYTES: u32 = 128 * 1024 * 1024;
pub(crate) const HEADER_BYTES: usize = 32;
pub(crate) const FRAME_OVERHEAD: usize = HEADER_BYTES + 4;
pub(crate) const RECORD_OVERHEAD: usize = 9;

/// An owned EC03 frame. Lengths and counts are derived by the encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ec03Frame {
    /// Producer-supplied page sequence; not a durability acknowledgement.
    pub page_sequence: u32,
    /// Exact record bytes and opaque record flags.
    pub records: Vec<Record>,
}

impl Ec03Frame {
    /// Create an empty frame.
    #[must_use]
    pub const fn new(page_sequence: u32) -> Self {
        Self {
            page_sequence,
            records: Vec::new(),
        }
    }

    /// Append exact record evidence, preserving its flags.
    pub fn add_record(&mut self, flags: u8, data: Vec<u8>) {
        self.records.push(Record { flags, data });
    }
}

/// Structured framing failure, distinct from an interrupted final append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The input ends before the complete declared frame.
    Incomplete,
    /// The magic does not identify an EC03 frame.
    Magic,
    /// Unsupported wire version, header size, or mandatory feature flags.
    Unsupported,
    /// Invalid or excessive frame/record length or record count.
    Length,
    /// Header, record, or frame checksum mismatch.
    Checksum,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EC03 frame: {self:?}")
    }
}

impl std::error::Error for FrameError {}

pub(crate) fn checksum(bytes: &[u8]) -> u32 {
    crc::Crc::<u32>::new(&crc::CRC_32_ISCSI).checksum(bytes)
}

fn array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], FrameError> {
    bytes
        .get(offset..offset.saturating_add(N))
        .and_then(|part| part.try_into().ok())
        .ok_or(FrameError::Incomplete)
}

/// Encode a frame, checking all sizes before allocating its output buffer.
///
/// # Errors
/// Rejects records over 64 MiB and frames over [`MAX_FRAME_BYTES`].
pub fn encode_ec03(frame: &Ec03Frame) -> Result<Vec<u8>, FrameError> {
    encode_records(
        frame.page_sequence,
        frame.records.iter().map(|r| (r.flags, r.data.as_slice())),
    )
}

pub(crate) fn encode_records<'a>(
    sequence: u32,
    records: impl Iterator<Item = (u8, &'a [u8])> + Clone,
) -> Result<Vec<u8>, FrameError> {
    let mut length = FRAME_OVERHEAD;
    let mut count = 0_u32;
    for (_, data) in records.clone() {
        if u32::try_from(data.len()).map_err(|_invalid_length| FrameError::Length)?
            > MAX_RECORD_BYTES
        {
            return Err(FrameError::Length);
        }
        length = length
            .checked_add(RECORD_OVERHEAD)
            .and_then(|n| n.checked_add(data.len()))
            .ok_or(FrameError::Length)?;
        count = count.checked_add(1).ok_or(FrameError::Length)?;
    }
    let frame_length = u32::try_from(length).map_err(|_invalid_length| FrameError::Length)?;
    if frame_length > MAX_FRAME_BYTES {
        return Err(FrameError::Length);
    }
    let mut bytes = Vec::with_capacity(length);
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&EC03_FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&32_u16.to_le_bytes());
    bytes.extend_from_slice(&frame_length.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&u64::from(sequence).to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes()); // Mandatory feature bits.
    bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
    for (flags, data) in records {
        bytes.extend_from_slice(
            &u32::try_from(data.len())
                .map_err(|_invalid_length| FrameError::Length)?
                .to_le_bytes(),
        );
        bytes.push(flags);
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(&checksum(data).to_le_bytes());
    }
    bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
    Ok(bytes)
}

/// A validated frame borrowing the input, with no per-record allocation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameRef<'a> {
    pub(crate) sequence: u32,
    pub(crate) length: usize,
    pub(crate) records: &'a [u8],
}

/// Verify the entire frame before making any record visible.
pub(crate) fn scan_frame(bytes: &[u8]) -> Result<FrameRef<'_>, FrameError> {
    if array::<4>(bytes, 0)? != MAGIC {
        return Err(FrameError::Magic);
    }
    let header = bytes.get(..HEADER_BYTES).ok_or(FrameError::Incomplete)?;
    if checksum(header.get(..28).ok_or(FrameError::Length)?)
        != u32::from_le_bytes(array(bytes, 28)?)
    {
        return Err(FrameError::Checksum);
    }
    if u16::from_le_bytes(array(bytes, 4)?) != EC03_FORMAT_VERSION
        || u16::from_le_bytes(array(bytes, 6)?) != 32
        || u32::from_le_bytes(array(bytes, 24)?) != 0
    {
        return Err(FrameError::Unsupported);
    }
    let declared = u32::from_le_bytes(array(bytes, 8)?);
    let length = usize::try_from(declared).map_err(|_invalid_length| FrameError::Length)?;
    if declared > MAX_FRAME_BYTES || length < FRAME_OVERHEAD {
        return Err(FrameError::Length);
    }
    let frame = bytes.get(..length).ok_or(FrameError::Incomplete)?;
    let end = length.saturating_sub(4);
    if checksum(frame.get(..end).ok_or(FrameError::Length)?)
        != u32::from_le_bytes(array(frame, end)?)
    {
        return Err(FrameError::Checksum);
    }
    let records = frame.get(HEADER_BYTES..end).ok_or(FrameError::Length)?;
    let count = usize::try_from(u32::from_le_bytes(array(bytes, 12)?))
        .map_err(|_invalid_length| FrameError::Length)?;
    if count > records.len() / RECORD_OVERHEAD {
        return Err(FrameError::Length);
    }
    let mut remaining = records;
    for _ in 0..count {
        let record = scan_record(remaining)?;
        remaining = remaining.get(record.length..).ok_or(FrameError::Length)?;
    }
    if !remaining.is_empty() {
        return Err(FrameError::Length);
    }
    Ok(FrameRef {
        sequence: u32::try_from(u64::from_le_bytes(array(bytes, 16)?))
            .map_err(|_invalid_length| FrameError::Length)?,
        length,
        records,
    })
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckedRecord<'a> {
    pub(crate) flags: u8,
    pub(crate) data: &'a [u8],
    pub(crate) checksum: u32,
    pub(crate) length: usize,
}

pub(crate) fn scan_record(bytes: &[u8]) -> Result<CheckedRecord<'_>, FrameError> {
    let length = u32::from_le_bytes(array(bytes, 0)?);
    if length > MAX_RECORD_BYTES {
        return Err(FrameError::Length);
    }
    let end = usize::try_from(length)
        .map_err(|_invalid_length| FrameError::Length)?
        .saturating_add(5);
    let data = bytes.get(5..end).ok_or(FrameError::Length)?;
    let flags = bytes.get(4).copied().ok_or(FrameError::Length)?;
    let stored =
        u32::from_le_bytes(array(bytes, end).map_err(|_invalid_length| FrameError::Length)?);
    if checksum(data) != stored {
        return Err(FrameError::Checksum);
    }
    Ok(CheckedRecord {
        flags,
        data,
        checksum: stored,
        length: end.saturating_add(4),
    })
}

/// Decode one complete frame. Any following frame remains the caller's input.
///
/// # Errors
/// Rejects incomplete, unsupported, corrupt, oversized, or inconsistent frames.
pub fn decode_ec03(bytes: &[u8]) -> Result<Ec03Frame, FrameError> {
    let frame = scan_frame(bytes)?;
    let mut result = Ec03Frame::new(frame.sequence);
    let mut remaining = frame.records;
    while !remaining.is_empty() {
        let record = scan_record(remaining)?;
        result.add_record(record.flags, record.data.to_vec());
        remaining = remaining.get(record.length..).ok_or(FrameError::Length)?;
    }
    Ok(result)
}
