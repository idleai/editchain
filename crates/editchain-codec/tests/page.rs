#![doc = "Page format round-trip tests."]

use crc as _;
use editchain_core as _;
use postcard as _;
use proptest as _;
use serde as _;

use editchain_codec::page::{decode_page, encode_page, Page};
use editchain_codec::scan::{PageScanner, ScanErrorKind, ScanItem, MAX_RECORD_BYTES};

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "test assertions on known-length vec"
)]
fn round_trip_page() {
    let mut page = Page::new(0);
    page.add_record(0x01, vec![1, 2, 3]);
    page.add_record(0x02, vec![4, 5, 6, 7]);

    let encoded = encode_page(&page).unwrap();
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

    let mut encoded = encode_page(&page).unwrap();
    // Truncate to cut into the second record's data (after first record + len prefix of second)
    // First record: len(4) + flags(1) + data(3) = 8 bytes after header
    // Second record len prefix: 4 bytes
    // Header: magic(4) + page_seq(4) = 8 bytes
    // Truncate after first record + len prefix of second = 8 + 8 + 4 = 20 bytes
    encoded.truncate(20);

    let decoded = decode_page(&encoded).unwrap();
    assert_eq!(decoded.records.len(), 1); // first record survived
}

// EC02 fixture pinned from the existing fixed-u32 format. The first payload
// contains the page marker itself: it is data, not another page boundary.
const TWO_PAGES: &[u8] =
    b"EC02\x07\x00\x00\x00\x04\x00\x00\x00\x81EC02EC02\x08\x00\x00\x00\x02\x00\x00\x00\x02\xaa\xbb";

#[test]
fn scans_frozen_concatenated_pages_with_exact_offsets_and_flags() {
    let mut scanner = PageScanner::new(TWO_PAGES);
    let items = scanner.by_ref().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(scanner.consumed(), TWO_PAGES.len());
    assert_eq!(
        items,
        vec![
            ScanItem::Page {
                sequence: 7,
                offset: 0
            },
            ScanItem::Record(editchain_codec::scan::RecordRef {
                page_sequence: 7,
                offset: 8,
                data_offset: 13,
                flags: 0x81,
                data: b"EC02",
            }),
            ScanItem::Page {
                sequence: 8,
                offset: 17
            },
            ScanItem::Record(editchain_codec::scan::RecordRef {
                page_sequence: 8,
                offset: 25,
                data_offset: 30,
                flags: 2,
                data: &[0xaa, 0xbb],
            }),
        ]
    );
    let mut first = Page::new(7);
    first.add_record(0x81, b"EC02".to_vec());
    let mut second = Page::new(8);
    second.add_record(2, vec![0xaa, 0xbb]);
    let mut encoded = encode_page(&first).unwrap();
    encoded.extend(encode_page(&second).unwrap());
    assert_eq!(encoded, TWO_PAGES);
    assert_eq!(decode_page(TWO_PAGES).unwrap().records.len(), 1);
}

#[test]
fn every_truncation_preserves_only_complete_records() {
    for length in 0..=TWO_PAGES.len() {
        let bytes = TWO_PAGES.get(..length).unwrap();
        let mut scanner = PageScanner::new(bytes);
        let items: Vec<_> = scanner.by_ref().collect();
        let records = items
            .iter()
            .filter(|item| matches!(item, Ok(ScanItem::Record(_))))
            .count();
        assert_eq!(
            records,
            usize::from(length >= 17) + usize::from(length == 32)
        );
        let at_boundary = matches!(length, 0 | 8 | 17 | 25 | 32);
        assert_eq!(items.iter().any(Result::is_err), !at_boundary);
        for error in items.into_iter().filter_map(Result::err) {
            assert_eq!(error.kind, ScanErrorKind::IncompleteTail);
            assert_eq!(scanner.consumed(), error.offset);
        }
        assert!(scanner.next().is_none());
    }
}

#[test]
fn distinguishes_invalid_unsupported_and_oversized_input() {
    let invalid = PageScanner::new(b"bad!").next().unwrap().unwrap_err();
    assert_eq!(invalid.kind, ScanErrorKind::InvalidMagic);
    let unsupported = PageScanner::new(b"EC03").next().unwrap().unwrap_err();
    assert_eq!(unsupported.kind, ScanErrorKind::UnsupportedFormat(*b"EC03"));
    let mut bytes = b"EC02\x00\x00\x00\x00".to_vec();
    let declared = MAX_RECORD_BYTES.saturating_add(1);
    bytes.extend_from_slice(&declared.to_le_bytes());
    let error = PageScanner::new(&bytes).nth(1).unwrap().unwrap_err();
    assert_eq!(error.kind, ScanErrorKind::RecordTooLarge(declared));
    assert_eq!(error.offset, 8);

    let mut page = Page::new(0);
    page.magic = *b"bad!";
    assert_eq!(
        encode_page(&page),
        Err(editchain_codec::page::PageEncodeError::InvalidMagic)
    );
}
