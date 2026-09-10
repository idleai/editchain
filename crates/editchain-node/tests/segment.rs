//! Segment store tests.

use clap as _;
use dirs as _;
use editchain_core as _;
use editchain_git as _;
use editchain_import as _;
use editchain_vscode_service as _;
use serde as _;
use serde_json as _;

use editchain_codec::page::Page;
use editchain_node::segment::SegmentStore;

#[test]
fn open_creates_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store = SegmentStore::open(dir.path().join("test-chain")).unwrap();
    assert!(store.chain_dir.exists());
}

#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-length vec"
)]
#[test]
fn append_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = SegmentStore::open(dir.path().join("test-chain")).unwrap();

    let mut page = Page::new(0);
    page.add_record(0x01, vec![1, 2, 3]);
    store.append_page(&page).unwrap();

    let pages = store.read_all().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].records.len(), 1);
}

#[expect(
    clippy::similar_names,
    reason = "page1/page2/pages are distinct test variables"
)]
#[test]
fn rotate_and_read_multiple() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = SegmentStore::open(dir.path().join("test-chain")).unwrap();

    let mut page1 = Page::new(0);
    page1.add_record(0x01, vec![1]);
    store.append_page(&page1).unwrap();
    store.rotate().unwrap();

    let mut page2 = Page::new(1);
    page2.add_record(0x02, vec![2]);
    store.append_page(&page2).unwrap();

    let pages = store.read_all().unwrap();
    assert_eq!(pages.len(), 2);
}

#[test]
fn concatenated_pages_are_read_without_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = SegmentStore::open(dir.path()).unwrap();
    for sequence in [7, 8] {
        let mut page = Page::new(sequence);
        page.add_record(0x81, b"EC02".to_vec());
        store.append_page(&page).unwrap();
    }
    let pages = store.read_all().unwrap();
    assert_eq!(
        pages.iter().map(|page| page.page_seq).collect::<Vec<_>>(),
        vec![7, 8]
    );
    for page in pages {
        assert_eq!(page.records.len(), 1);
        let record = page.records.first().unwrap();
        assert_eq!(record.flags, 0x81);
        assert_eq!(record.data, b"EC02");
    }
}

#[test]
fn incomplete_tail_is_tolerated_but_unsupported_format_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = SegmentStore::open(dir.path()).unwrap();
    let mut page = Page::new(0);
    page.add_record(0, b"one".to_vec());
    store.append_page(&page).unwrap();
    let path = dir.path().join("000000.eclog");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"EC02\x01");
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(store.read_all().unwrap().len(), 1);

    std::fs::write(path, b"EC03").unwrap();
    assert_eq!(
        store.read_all().unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}
