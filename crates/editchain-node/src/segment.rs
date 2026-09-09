use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap as _;
use dirs as _;
use editchain_core as _;
use editchain_import as _;
use serde as _;
use serde_json as _;

use editchain_codec::page::{decode_page, encode_page, Page};

/// Directory layout for segment storage.
///
/// ```text
/// .editchain/<chain>/
///   000000.eclog
///   000001.eclog
///   blobs/<content-id>
/// ```
#[derive(Debug)]
pub struct SegmentStore {
    /// Path to the chain directory.
    pub chain_dir: PathBuf,
    /// Next segment sequence number.
    next_seq: u32,
}

impl SegmentStore {
    /// Open or create a chain directory.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the chain directory cannot be created or read.
    pub fn open(chain_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let chain_dir = chain_dir.into();
        fs::create_dir_all(&chain_dir)?;

        // Determine the next segment sequence number.
        let next_seq = find_next_segment(&chain_dir)?;

        Ok(Self {
            chain_dir,
            next_seq,
        })
    }

    /// Append a page of operations to the current segment.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the segment file cannot be opened or written.
    ///
    /// # Durability
    ///
    /// The appended bytes are flushed to stable storage (`sync_all`) before
    /// this returns. When the append creates a brand-new segment file, the
    /// chain directory is synced as well, so the new segment's directory
    /// entry is durable before this returns — callers may then safely advance
    /// durable cursors (the import command commits its staged cursors here).
    pub fn append_page(&mut self, page: &Page) -> io::Result<()> {
        let path = self.current_segment_path();
        let encoded = encode_page(page);
        // A new segment file needs its directory entry persisted, not just its
        // bytes: a crash could otherwise lose the entry while a cursor commit
        // that follows this append has already been made durable.
        let newly_created = !path.exists();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        if newly_created {
            sync_parent_dir(&self.chain_dir)?;
        }
        Ok(())
    }

    /// Read all pages from all segments in order.
    ///
    /// # Errors
    ///
    /// Returns an IO error if any segment file cannot be read.
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "Segment file reading; offsets bounded by buffer length checks"
    )]
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
    ///
    /// # Errors
    ///
    /// This operation is infallible but returns `io::Result` for future-proofing.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "next_seq is a bounded counter"
    )]
    pub const fn rotate(&mut self) -> io::Result<()> {
        self.next_seq += 1;
        Ok(())
    }

    /// Path to the current segment file.
    fn current_segment_path(&self) -> PathBuf {
        self.segment_path(self.next_seq)
    }

    fn segment_path(&self, seq: u32) -> PathBuf {
        let filename = format!("{seq:06}.eclog");
        self.chain_dir.join(filename)
    }
}

/// Compute the encoded length of a page from its bytes.
/// Reads the magic + `page_seq` (8 bytes) then scans records.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "Binary page parsing; offsets are bounded by buffer length checks"
)]
fn encoded_page_len(bytes: &[u8]) -> usize {
    if bytes.len() < 8 {
        return bytes.len();
    }
    let mut offset = 8;
    while offset + 4 <= bytes.len() {
        let len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4])) as usize;
        offset += 4;
        if offset + 1 + len > bytes.len() {
            break;
        }
        offset += 1 + len;
    }
    offset
}

/// Find the next available segment sequence number.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "seq is a bounded u32 counter from file parsing"
)]
fn find_next_segment(dir: &Path) -> io::Result<u32> {
    let mut max_seq = 0u32;

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(name_str) = name.to_str() {
            if name_str.to_lowercase().ends_with(".eclog") {
                if let Some(seq_str) = name_str.strip_suffix(".eclog") {
                    if let Ok(seq) = seq_str.parse::<u32>() {
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

/// Fsync a directory so a file created or renamed inside it survives a crash.
///
/// On Unix the directory is opened read-only and fsynced. On Windows opening a
/// directory requires `FILE_FLAG_BACKUP_SEMANTICS`. On other platforms
/// directory fsync is not available portably and the call degrades to a no-op
/// (best-effort durability).
///
/// # Errors
///
/// Returns an IO error if the directory cannot be opened or synced.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// Windows variant of [`sync_parent_dir`].
#[cfg(windows)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x0200_0000)
        .open(path)?
        .sync_all()
}

/// Fallback for platforms without directory fsync: best-effort no-op.
#[cfg(not(any(unix, windows)))]
fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_page_creates_and_persists_a_new_segment() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SegmentStore::open(dir.path().join("chain")).unwrap();

        let mut page = Page::new(0);
        page.add_record(0, vec![1, 2, 3]);
        store.append_page(&page).unwrap();

        // The new segment file exists on disk with a durable directory entry.
        let seg_path = dir.path().join("chain/000000.eclog");
        assert!(seg_path.is_file());
        let bytes = fs::read(&seg_path).unwrap();
        let decoded = decode_page(&bytes).unwrap();
        assert_eq!(decoded.records.len(), 1);
        assert_eq!(decoded.records.first().unwrap().data, vec![1, 2, 3]);

        let stored_pages = store.read_all().unwrap();
        assert_eq!(stored_pages.len(), 1);
        assert_eq!(
            stored_pages.first().unwrap().records.first().unwrap().data,
            vec![1, 2, 3]
        );
    }

    #[test]
    fn rotated_segments_read_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SegmentStore::open(dir.path().join("chain")).unwrap();

        let mut first_page = Page::new(0);
        first_page.add_record(0, vec![9]);
        store.append_page(&first_page).unwrap();
        store.rotate().unwrap();

        let mut second_page = Page::new(0);
        second_page.add_record(0, vec![8, 8]);
        store.append_page(&second_page).unwrap();

        // Two segments, both created through the new-segment sync path.
        assert!(dir.path().join("chain/000000.eclog").is_file());
        assert!(dir.path().join("chain/000001.eclog").is_file());

        let stored_pages = store.read_all().unwrap();
        assert_eq!(stored_pages.len(), 2);
        assert_eq!(
            stored_pages.first().unwrap().records.first().unwrap().data,
            vec![9]
        );
        assert_eq!(
            stored_pages.get(1).unwrap().records.first().unwrap().data,
            vec![8, 8]
        );

        // A fresh store over the same directory restores both segments in order.
        let reopened = SegmentStore::open(dir.path().join("chain")).unwrap();
        let restored_pages = reopened.read_all().unwrap();
        assert_eq!(restored_pages.len(), 2);
        assert_eq!(
            restored_pages
                .first()
                .unwrap()
                .records
                .first()
                .unwrap()
                .data,
            vec![9]
        );
        assert_eq!(
            restored_pages.get(1).unwrap().records.first().unwrap().data,
            vec![8, 8]
        );
    }

    #[test]
    fn sync_parent_dir_accepts_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        sync_parent_dir(dir.path()).unwrap();
    }
}
