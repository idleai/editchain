use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use editchain_codec::page::{encode_page, Page};
use editchain_codec::scan::{PageScanner, ScanErrorKind, ScanItem};

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
    chain_dir: PathBuf,
    /// Next segment sequence number.
    next_seq: u32,
    /// Held for this writer's lifetime; readers do not acquire the lock.
    writer_lock: fs::File,
}

impl SegmentStore {
    /// The directory protected by this writer's lifetime lock.
    #[must_use]
    pub fn chain_dir(&self) -> &Path {
        &self.chain_dir
    }

    /// Open or create a chain directory.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the chain directory cannot be created or read,
    /// its sequence is invalid, or another writer holds its lock.
    pub fn open(chain_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let chain_dir = chain_dir.into();
        fs::create_dir_all(&chain_dir)?;
        let writer_lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(chain_dir.join(".writer.lock"))?;
        writer_lock.try_lock().map_err(io::Error::from)?;

        // Determine the next segment sequence number.
        let next_seq = segment_sequences(&chain_dir)?.last().map_or(Ok(0), |seq| {
            seq.checked_add(1).ok_or_else(sequence_exhausted)
        })?;

        Ok(Self {
            chain_dir,
            next_seq,
            writer_lock,
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
        let encoded = encode_page(page)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
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
    pub fn read_all(&self) -> io::Result<Vec<Page>> {
        let mut pages = Vec::new();
        for seq in segment_sequences(&self.chain_dir)? {
            let bytes = fs::read(self.segment_path(seq))?;
            for item in PageScanner::new(&bytes) {
                match item {
                    Ok(ScanItem::Page { sequence, .. }) => pages.push(Page::new(sequence)),
                    Ok(ScanItem::Record(record)) => {
                        let page = pages.last_mut().ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "record has no page")
                        })?;
                        page.add_record(record.flags, record.data.to_vec());
                    }
                    Err(error) if error.kind == ScanErrorKind::IncompleteTail => break,
                    Err(error) => {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, error));
                    }
                }
            }
        }

        Ok(pages)
    }

    /// Rotate after a written segment. An unwritten segment stays current so
    /// repeated rotations cannot create sequence gaps.
    ///
    /// # Errors
    ///
    /// Returns an error if the segment sequence space is exhausted.
    pub fn rotate(&mut self) -> io::Result<()> {
        match fs::metadata(self.current_segment_path()) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or_else(sequence_exhausted)?;
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

impl Drop for SegmentStore {
    fn drop(&mut self) {
        // Closing only this descriptor can leave the lock alive briefly in a
        // concurrently spawned child before exec closes its inherited copy.
        // End the lock at the writer's actual lifetime boundary.
        drop(self.writer_lock.unlock());
    }
}

/// Enumerate the contiguous, canonically named segment sequence.
/// Missing chain directories are empty; other I/O errors are preserved.
pub(crate) fn segment_sequences(dir: &Path) -> io::Result<Vec<u32>> {
    if dir.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut sequences = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(number) = name.strip_suffix(".eclog") else {
            continue;
        };
        let seq: u32 = number
            .parse()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if name != format!("{seq:06}.eclog") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "noncanonical segment filename",
            ));
        }
        sequences.push(seq);
    }
    sequences.sort_unstable();
    for (expected, actual) in sequences.iter().enumerate() {
        if u32::try_from(expected).ok() != Some(*actual) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "segment sequence has a gap",
            ));
        }
    }
    Ok(sequences)
}

fn sequence_exhausted() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "segment sequence exhausted")
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
    use editchain_codec::page::decode_page;

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
        drop(store);
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

    #[test]
    fn dropping_writer_releases_lock_even_with_an_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let store = SegmentStore::open(dir.path()).unwrap();
        let inherited = store.writer_lock.try_clone().unwrap();
        assert!(SegmentStore::open(dir.path()).is_err());
        drop(store);
        let reopened = SegmentStore::open(dir.path()).unwrap();
        drop(inherited);
        assert!(SegmentStore::open(dir.path()).is_err());
        drop(reopened);
        assert!(SegmentStore::open(dir.path()).is_ok());
    }
}
