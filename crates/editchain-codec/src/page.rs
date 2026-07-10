extern crate alloc;
use alloc::vec::Vec;

/// Page magic bytes — "EC" + version 02.
pub const PAGE_MAGIC: [u8; 4] = [0x45, 0x43, 0x30, 0x32]; // "EC02"

/// A framed page of operations.
///
/// Format: magic | page_seq (u32 LE) | records... | optional CRC32
/// Each record: varint_len | flags (u8) | encoded_op | optional CRC32
#[derive(Debug, Clone)]
pub struct Page {
    pub magic: [u8; 4],
    pub page_seq: u32,
    pub records: Vec<Record>,
}

/// A single record within a page.
#[derive(Debug, Clone)]
pub struct Record {
    pub flags: u8,
    pub data: Vec<u8>,
}

impl Page {
    pub fn new(page_seq: u32) -> Self {
        Self {
            magic: PAGE_MAGIC,
            page_seq,
            records: Vec::new(),
        }
    }

    pub fn add_record(&mut self, flags: u8, data: Vec<u8>) {
        self.records.push(Record { flags, data });
    }
}

/// Encode a page into bytes.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_page() {
        let mut page = Page::new(0);
        page.add_record(0x01, vec![1, 2, 3]);
        page.add_record(0x02, vec![4, 5, 6, 7]);

        let encoded = encode_page(&page);
        let decoded = decode_page(&encoded).unwrap();

        assert_eq!(decoded.page_seq, 0);
        assert_eq!(decoded.records.len(), 2);
        assert_eq!(decoded.records[0].flags, 0x01);
        assert_eq!(decoded.records[0].data, vec![1, 2, 3]);
        assert_eq!(decoded.records[1].flags, 0x02);
        assert_eq!(decoded.records[1].data, vec![4, 5, 6, 7]);
    }

    #[test]
    fn power_loss_partial_record() {
        let mut page = Page::new(0);
        page.add_record(0x01, vec![1, 2, 3]);
        page.add_record(0x02, vec![4, 5, 6, 7]);

        let mut encoded = encode_page(&page);
        // Truncate to cut into the second record's data (after first record + len prefix of second)
        // First record: len(4) + flags(1) + data(3) = 8 bytes after header
        // Second record len prefix: 4 bytes
        // Header: magic(4) + page_seq(4) = 8 bytes
        // Truncate after first record + len prefix of second = 8 + 8 + 4 = 20 bytes
        encoded.truncate(20);

        let decoded = decode_page(&encoded).unwrap();
        assert_eq!(decoded.records.len(), 1); // first record survived
    }
}