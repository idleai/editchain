use editchain_codec::page::Page;
use editchain_node::segment::SegmentStore;

#[test]
fn open_creates_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store = SegmentStore::open(dir.path().join("test-chain")).unwrap();
    assert!(store.chain_dir.exists());
}

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