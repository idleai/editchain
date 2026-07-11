use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use editchain_codec::page::{decode_page, encode_page, Page};

/// Directory layout for segment storage.
///
/// ```text
/// .editchain/<chain>/
///   000000.eclog
///   000001.eclog
///   blobs/<content-id>
/// ```
pub struct SegmentStore {
    chain_dir: PathBuf,
    next_seq: u32,
}

impl SegmentStore {
    /// Open or create a chain directory.
    pub fn open(chain_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let chain_dir = chain_dir.into();
        fs::create_dir_all(&chain_dir)?;

        // Determine the next segment sequence number.
        let next_seq = find_next_segment(&chain_dir)?;

        Ok(Self { chain_dir, next_seq })
    }

    /// Append a page of operations to the current segment.
    pub fn append_page(&mut self, page: &Page) -> io::Result<()> {
        let path = self.current_segment_path();
        let encoded = encode_page(page);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(&encoded)?;
        Ok(())
    }

    /// Read all pages from all segments in order.
    pub fn read_all(&self) -> io::Result<Vec<Page>> {
        let mut pages = Vec::new();
        let mut seq = 0u32;

        loop {
            let path = self.segment_path(seq);
            if !path.exists() {
                break;
            }
            let bytes = fs::read(&path)?;
            // A segment file may contain multiple concatenated pages.
            let mut offset = 0;
            while offset < bytes.len() {
                if let Some(page) = decode_page(&bytes[offset..]) {
                    let encoded_len = encoded_page_len(&bytes[offset..]);
                    pages.push(page);
                    offset += encoded_len;
                } else {
                    break; // partial trailing page (power-loss)
                }
            }
            seq += 1;
        }

        Ok(pages)
    }

    /// Rotate to a new segment file.
    pub fn rotate(&mut self) -> io::Result<()> {
        self.next_seq += 1;
        Ok(())
    }

    /// Path to the current segment file.
    fn current_segment_path(&self) -> PathBuf {
        self.segment_path(self.next_seq)
    }

    fn segment_path(&self, seq: u32) -> PathBuf {
        let filename = format!("{:06}.eclog", seq);
        self.chain_dir.join(filename)
    }
}

/// Compute the encoded length of a page from its bytes.
/// Reads the magic + page_seq (8 bytes) then scans records.
fn encoded_page_len(bytes: &[u8]) -> usize {
    if bytes.len() < 8 {
        return bytes.len();
    }
    let mut offset = 8;
    while offset + 4 <= bytes.len() {
        let len = u32::from_le_bytes(
            bytes[offset..offset + 4].try_into().unwrap_or([0; 4]),
        ) as usize;
        offset += 4;
        if offset + 1 + len > bytes.len() {
            break;
        }
        offset += 1 + len;
    }
    offset
}

/// Find the next available segment sequence number.
fn find_next_segment(dir: &Path) -> io::Result<u32> {
    let mut max_seq = 0u32;

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(name_str) = name.to_str() {
            if name_str.ends_with(".eclog") {
                if let Some(seq_str) = name_str.strip_suffix(".eclog") {
                    if let Ok(seq) = u32::from_str_radix(seq_str, 10) {
                        if seq >= max_seq {
                            max_seq = seq + 1;
                        }
                    }
                }
            }
        }
    }

    Ok(max_seq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_creates_directory() {
        let dir = tempdir().unwrap();
        let store = SegmentStore::open(dir.path().join("test-chain")).unwrap();
        assert!(store.chain_dir.exists());
    }

    #[test]
    fn append_and_read() {
        let dir = tempdir().unwrap();
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
        let dir = tempdir().unwrap();
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
}