use editchain_codec::page::{decode_page, encode_page, Page};

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