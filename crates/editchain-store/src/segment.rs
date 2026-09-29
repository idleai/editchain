use std::fs;
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};

use crate::durable::{atomic_write, create_dir_all, sync_dir};
use crate::format::ec03::{encode_records, FRAME_OVERHEAD, RECORD_OVERHEAD};
use crate::format::scan::{PageScanner, ScanErrorKind, ScanItem};
use crate::format::Page;
use crate::{AppendLog, LogReadStats, RecordVisitor};

const PACKING_MARKER: &str = ".segment-layout";
const PACKING_VERSION: &[u8] = b"EC03-packed-v2\n";
const FRAME_TARGET_BYTES: u64 = 1024 * 1024;

/// Segment packing policy. Existing EC02 files remain readable unchanged.
#[derive(Debug, Clone, Copy)]
pub struct SegmentOptions {
    /// Target maximum bytes per segment. Whole pages stay together; one page
    /// may exceed this limit. Must be nonzero. Defaults to 32 MiB.
    pub max_segment_bytes: u64,
}

impl Default for SegmentOptions {
    fn default() -> Self {
        Self {
            max_segment_bytes: 32 * 1024 * 1024,
        }
    }
}

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
    options: SegmentOptions,
    length: u64,
    /// A failed write has an unknown outcome and must be rescanned before reuse.
    needs_recovery: bool,
    initialize_layout: bool,
}

impl SegmentStore {
    /// The directory protected by this writer's lifetime lock.
    #[must_use]
    pub fn chain_dir(&self) -> &Path {
        &self.chain_dir
    }

    /// Current segment sequence. Healthy final segments are reused across opens.
    /// Size-based rotation may advance it on the next append. Callers needing
    /// a persistent boundary must rotate and append a page (possibly empty).
    #[must_use]
    pub const fn segment_sequence(&self) -> u32 {
        self.next_seq
    }

    /// Open or create a chain directory.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the chain directory cannot be created or read,
    /// its sequence is invalid, or another writer holds its lock.
    pub fn open(chain_dir: impl Into<PathBuf>) -> io::Result<Self> {
        Self::open_with_options(chain_dir, SegmentOptions::default())
    }

    /// Open with a segment size policy, reusing only a complete final segment.
    /// Incomplete suffixes remain untouched and force a successor segment.
    ///
    /// # Errors
    /// Returns invalid-policy, framing, sequence, lock, or filesystem errors.
    pub fn open_with_options(
        chain_dir: impl Into<PathBuf>,
        options: SegmentOptions,
    ) -> io::Result<Self> {
        Self::open_inner(chain_dir.into(), options, false)
    }

    pub(crate) fn open_migration(chain_dir: PathBuf) -> io::Result<Self> {
        Self::open_inner(chain_dir, SegmentOptions::default(), true)
    }

    fn open_inner(
        chain_dir: PathBuf,
        options: SegmentOptions,
        migration: bool,
    ) -> io::Result<Self> {
        if !migration && chain_dir.join(".migration.json").try_exists()? {
            return Err(io::Error::other(
                "migration staging chain is not yet published",
            ));
        }
        if options.max_segment_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero segment size",
            ));
        }
        create_dir_all(&chain_dir)?;
        let writer_lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(chain_dir.join(".writer.lock"))?;
        writer_lock.try_lock().map_err(io::Error::from)?;
        require_current_format(&chain_dir)?;
        let initialize_layout = packing_marker_missing(&chain_dir)?;
        let next_seq = segment_sequences(&chain_dir)?.last().copied().unwrap_or(0);
        let mut store = Self {
            chain_dir,
            next_seq,
            writer_lock,
            options,
            length: 0,
            needs_recovery: true,
            initialize_layout,
        };
        store.recover_frontier()?;
        if initialize_layout {
            // Legacy writers opened a fresh segment for every transaction.
            // Preserve that boundary once on upgrade: existing sharing cutoffs
            // may point at the next, not-yet-created segment.
            store.rotate()?;
        }
        Ok(store)
    }

    /// Wait for a short competing transaction before opening a writer.
    ///
    /// Sleeps without holding the writer lock. Callers must still keep their
    /// resulting transaction short and release it before any network I/O.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::open`], including `WouldBlock` when
    /// the timeout expires while another writer still owns the chain.
    pub fn open_wait(
        chain_dir: impl Into<PathBuf>,
        timeout: std::time::Duration,
    ) -> io::Result<Self> {
        let chain_dir = chain_dir.into();
        let started = std::time::Instant::now();
        loop {
            match Self::open(&chain_dir) {
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && started.elapsed() < timeout =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                result => return result,
            }
        }
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
        self.append_with(page, |file, encoded, path| {
            file.write_all(encoded)?;
            file.sync_all()?;
            // Always sync publication: the file may exist after a failed sync.
            crate::durable::sync_parent_dir(path)
        })
    }

    fn append_with(
        &mut self,
        page: &Page,
        persist: impl FnOnce(&mut fs::File, &[u8], &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let encoded = encode_records(
            page.page_seq,
            page.records.iter().map(|r| (r.flags, r.data.as_slice())),
        )
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        if self.needs_recovery {
            self.recover_frontier()?;
        }
        let length = u64::try_from(encoded.len()).map_err(io::Error::other)?;
        if self.length > 0 && self.length.saturating_add(length) > self.options.max_segment_bytes {
            self.rotate()?;
        }
        let path = self.current_segment_path();
        self.needs_recovery = true;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        persist(&mut file, &encoded, &path)?;
        if self.initialize_layout {
            // Publish only after a segment beyond legacy history is durable.
            // An interrupted marker publication can cause another safe boundary,
            // never reuse below a pre-existing cutoff or change earlier bytes.
            atomic_write(&self.chain_dir.join(PACKING_MARKER), PACKING_VERSION)?;
            self.initialize_layout = false;
        }
        self.length = file.metadata()?.len();
        self.needs_recovery = false;
        Ok(())
    }

    fn recover_frontier(&mut self) -> io::Result<()> {
        let bytes = match fs::read(self.current_segment_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.length = 0;
                self.needs_recovery = false;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
        let mut incomplete = false;
        for item in PageScanner::new(&bytes) {
            match item {
                Ok(_) => {}
                Err(error) if error.kind == ScanErrorKind::IncompleteTail => {
                    incomplete = true;
                    break;
                }
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
            }
        }
        if incomplete || self.length >= self.options.max_segment_bytes {
            self.rotate()?;
        }
        self.needs_recovery = false;
        Ok(())
    }

    /// Re-establish durability of retained segments before acknowledging a replay.
    ///
    /// An earlier failed append may have written complete bytes but failed to
    /// sync them or their directory entry. Merely reading matching bytes does
    /// not prove they are durable. This method holds this store's writer lock
    /// while syncing all retained segments and their directory.
    ///
    /// # Errors
    /// Returns an error if segment enumeration, opening, or synchronization fails.
    pub fn sync_all(&self) -> io::Result<()> {
        for sequence in segment_sequences(&self.chain_dir)? {
            fs::OpenOptions::new()
                .write(true)
                .open(self.segment_path(sequence))?
                .sync_all()?;
        }
        sync_dir(&self.chain_dir)
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
        self.length = 0;
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

fn migration_required() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "EC02 chain is read-only; run editchain --chain SOURCE migrate --destination NEW_CHAIN before writing",
    )
}

fn require_current_format(root: &Path) -> io::Result<()> {
    // An interrupted first append can leave an empty segment before an EC02
    // segment. Check every retained segment so no legacy suffix is overlooked.
    for sequence in segment_sequences(root)? {
        let mut file = fs::File::open(root.join(format!("{sequence:06}.eclog")))?;
        let mut magic = [0; 4];
        match file.read_exact(&mut magic) {
            Ok(()) if &magic == b"EC02" => return Err(migration_required()),
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn packing_marker_missing(root: &Path) -> io::Result<bool> {
    match fs::read(root.join(PACKING_MARKER)) {
        Ok(bytes) if bytes == PACKING_VERSION => Ok(false),
        Ok(bytes) if bytes == b"EC02-packed-v1\n" => Err(migration_required()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported segment layout marker",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

impl AppendLog for SegmentStore {
    fn visit_records(&self, visitor: &mut RecordVisitor<'_>) -> io::Result<LogReadStats> {
        let mut stats = LogReadStats::default();
        for sequence in segment_sequences(&self.chain_dir)? {
            let bytes = fs::read(self.segment_path(sequence))?;
            for item in PageScanner::new(&bytes) {
                match item {
                    Ok(ScanItem::Page { .. }) => {}
                    Ok(ScanItem::Record(record)) => visitor(record.flags, record.data)?,
                    Err(error) if error.kind == ScanErrorKind::IncompleteTail => {
                        stats.incomplete_tails = stats.incomplete_tails.saturating_add(1);
                        break;
                    }
                    Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
                }
            }
        }
        Ok(stats)
    }

    fn append_record(&mut self, flags: u8, encoded: &[u8]) -> io::Result<()> {
        self.append_records(&[(flags, encoded)])
    }

    fn append_records(&mut self, records: &[(u8, &[u8])]) -> io::Result<()> {
        let mut page = Page::new(self.next_seq);
        let mut length = u64::try_from(FRAME_OVERHEAD).map_err(io::Error::other)?;
        for (flags, encoded) in records {
            let added = u64::try_from(encoded.len())
                .map_err(io::Error::other)?
                .saturating_add(u64::try_from(RECORD_OVERHEAD).map_err(io::Error::other)?);
            if !page.records.is_empty()
                && length.saturating_add(added)
                    > FRAME_TARGET_BYTES.min(self.options.max_segment_bytes)
            {
                self.append_page(&page)?;
                page = Page::new(self.next_seq);
                length = u64::try_from(FRAME_OVERHEAD).map_err(io::Error::other)?;
            }
            page.add_record(*flags, encoded.to_vec());
            length = length.saturating_add(added);
        }
        if !page.records.is_empty() {
            self.append_page(&page)?;
        }
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        self.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::decode_page;

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
        sync_dir(dir.path()).unwrap();
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

    #[test]
    fn competing_transactions_wait_boundedly_without_stealing_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let held = SegmentStore::open(dir.path()).unwrap();
        assert_eq!(
            SegmentStore::open_wait(dir.path(), std::time::Duration::from_millis(20))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            drop(held);
        });
        let next = SegmentStore::open_wait(dir.path(), std::time::Duration::from_secs(2)).unwrap();
        assert!(SegmentStore::open(dir.path()).is_err());
        release.join().unwrap();
        drop(next);
        assert!(SegmentStore::open(dir.path()).is_ok());
    }
    #[test]
    fn same_writer_recovers_after_partial_write_or_failed_sync() {
        for complete in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = SegmentStore::open(dir.path()).unwrap();
            let mut first = Page::new(17);
            first.add_record(0, b"complete original".to_vec());
            store.append_page(&first).unwrap();
            let mut failed = Page::new(18);
            failed.add_record(37, b"unacknowledged".to_vec());
            let error = store.append_with(&failed, |file, bytes, _path| {
                let cut = if complete {
                    bytes.len()
                } else {
                    bytes.len().saturating_sub(1)
                };
                file.write_all(bytes.get(..cut).unwrap())?;
                Err(io::Error::other("injected write/sync failure"))
            });
            assert!(error.is_err(), "failed persistence cannot be acknowledged");
            let path = dir.path().join("000000.eclog");
            let retained = fs::read(&path).unwrap();
            store.append_record(91, b"after failure").unwrap();
            assert!(
                fs::read(&path).unwrap().starts_with(&retained),
                "failure bytes remain untouched"
            );
            let mut records = Vec::new();
            let stats = store
                .visit_records(&mut |flags, bytes| {
                    records.push((flags, bytes.to_vec()));
                    Ok(())
                })
                .unwrap();
            assert_eq!(records.last(), Some(&(91, b"after failure".to_vec())));
            assert_eq!(stats.incomplete_tails, usize::from(!complete));
            assert_eq!(records.len(), if complete { 3 } else { 2 });
            drop(store);
            assert_eq!(
                SegmentStore::open(dir.path())
                    .unwrap()
                    .read_all()
                    .unwrap()
                    .iter()
                    .map(|page| page.records.len())
                    .sum::<usize>(),
                records.len()
            );
        }
    }
}
