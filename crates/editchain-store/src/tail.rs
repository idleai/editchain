//! Retained canonical admission over the append-only segment frontier.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editchain_core::{Admission, Op, OpId};

use crate::format::decode_op;
use crate::format::scan::{PageScanner, ScanErrorKind, ScanItem, MAX_RECORD_BYTES};
use crate::segment::segment_sequences;
use crate::{CanonicalChain, OpRecordLocation};

/// Work performed by one frontier read, excluding initial history loading.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct TailWork {
    /// Bytes read from the frontier, never including sealed history.
    pub bytes_read: u64,
    /// Complete encoded operations decoded in this read.
    pub records_decoded: u64,
    /// Complete records with unsupported operation encodings.
    pub undecodable: u64,
}

/// Canonical additions and retractions caused by newly encountered bytes.
#[derive(Debug, Default)]
pub struct ChainDelta {
    /// Newly accepted operations, after conflicts within this batch are removed.
    pub added: BTreeMap<OpId, (Arc<Op>, OpRecordLocation)>,
    /// Identities quarantined by newly encountered conflicting evidence.
    pub removed: BTreeSet<OpId>,
    /// Observable work used by scaling tests and live diagnostics.
    pub work: TailWork,
}

/// Admission storage used by an append frontier.
///
/// Implementations must retain every distinct byte representation and keep
/// conflicted identities permanently inert for this frontier's lifetime.
pub trait TailCorpus: std::fmt::Debug {
    /// Create an empty corpus backed by the specified chain directory.
    fn empty(root: &Path) -> Self;
    /// Admit the bytes at a durable record location.
    ///
    /// # Errors
    /// Returns errors if retained evidence cannot be read.
    fn admit_record(
        &mut self,
        op: Arc<Op>,
        encoded: &[u8],
        location: OpRecordLocation,
    ) -> io::Result<Admission>;
    /// Count an undecodable complete record.
    fn record_undecodable(&mut self);
    /// Track whether the active frontier ends with an incomplete record.
    fn record_tail_change(&mut self, incomplete: bool);
}

impl TailCorpus for CanonicalChain {
    fn empty(_root: &Path) -> Self {
        Self::default()
    }
    fn admit_record(
        &mut self,
        op: Arc<Op>,
        encoded: &[u8],
        location: OpRecordLocation,
    ) -> io::Result<Admission> {
        Ok(self.admit(op.as_ref().clone(), encoded.to_vec(), Some(location)))
    }
    fn record_undecodable(&mut self) {
        self.record_undecodable();
    }
    fn record_tail_change(&mut self, incomplete: bool) {
        self.record_tail_change(incomplete);
    }
}

/// Append frontier retaining a portable in-memory evidence set.
pub type CanonicalTail = Tail<CanonicalChain>;
/// Append frontier retaining exact evidence in its immutable segment records.
pub type IndexedTail = Tail<crate::IndexedChain>;

/// A canonical chain retained across append reads.
///
/// Segment writers append only to their current segment and leave earlier
/// segments immutable. This reader checks the current file and its successor;
/// it does not inventory or decode sealed history on each poll. A caller that
/// observes an external replacement of sealed history must reopen the reader.
/// Active-file replacement, truncation and same-size modification are rejected.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Tail<C: TailCorpus> {
    root: PathBuf,
    chain: C,
    segment: u32,
    offset: u64,
    page: Option<u32>,
    observed: Option<FileStamp>,
    sealed: editchain_index::OrderedMap<u32, FileStamp>,
    incomplete: bool,
}

impl<C: TailCorpus> Tail<C> {
    /// Load the existing chain once and retain its admission state and frontier.
    ///
    /// # Errors
    ///
    /// Rejects gaps, unreadable records and corrupt framing as the full reader does.
    pub fn open(root: &Path) -> io::Result<Self> {
        let _sequences = segment_sequences(root)?;
        let mut tail = Self::empty(root);
        loop {
            let before = (tail.segment, tail.offset);
            drop(tail.poll()?);
            if before == (tail.segment, tail.offset) {
                break;
            }
        }
        Ok(tail)
    }

    /// Create an empty admission frontier, ready to read from the first record.
    #[must_use]
    pub fn empty(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            chain: C::empty(root),
            segment: 0,
            offset: 0,
            page: None,
            observed: None,
            sealed: editchain_index::OrderedMap::new(),
            incomplete: false,
        }
    }

    /// The retained byte-admission corpus.
    #[must_use]
    pub const fn chain(&self) -> &C {
        &self.chain
    }

    /// Drain the currently available append frontier, stopping at an incomplete
    /// record without retrying its unchanged prefix in a busy loop.
    ///
    /// # Errors
    /// Returns the same framing and continuity errors as [`Self::poll`].
    pub fn drain(&mut self) -> io::Result<ChainDelta> {
        let mut result = ChainDelta::default();
        loop {
            let before = (self.segment, self.offset);
            let delta = self.poll()?;
            result.work.bytes_read = result.work.bytes_read.saturating_add(delta.work.bytes_read);
            result.work.records_decoded = result
                .work
                .records_decoded
                .saturating_add(delta.work.records_decoded);
            result.work.undecodable = result
                .work
                .undecodable
                .saturating_add(delta.work.undecodable);
            result.added.extend(delta.added);
            for id in delta.removed {
                drop(result.added.remove(&id));
                let _: bool = result.removed.insert(id);
            }
            if before == (self.segment, self.offset) {
                return Ok(result);
            }
        }
    }

    /// Read one bounded portion of newly appended records.
    ///
    /// A pending partial record is reread on a later poll and is never admitted
    /// early. Exact replays produce no additions; conflicts retract accepted IDs.
    ///
    /// # Errors
    ///
    /// Returns an error for source replacement, truncation, invalid framing or IO.
    pub fn poll(&mut self) -> io::Result<ChainDelta> {
        let mut delta = ChainDelta::default();
        loop {
            let path = self.root.join(format!("{:06}.eclog", self.segment));
            let mut file = match File::open(&path) {
                Ok(file) => file,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && self.observed.is_none() =>
                {
                    return Ok(delta)
                }
                Err(error) => return Err(error),
            };
            let metadata = file.metadata()?;
            self.validate_frontier(&metadata)?;
            let _: u64 = file.seek(SeekFrom::Start(self.offset))?;
            let mut bytes = Vec::new();
            let limit = u64::from(MAX_RECORD_BYTES).saturating_add(13);
            let _read = file.take(limit).read_to_end(&mut bytes)?;
            delta.work.bytes_read = delta
                .work
                .bytes_read
                .saturating_add(u64::try_from(bytes.len()).map_err(io::Error::other)?);
            let previous_offset = self.offset;
            let incomplete = self.scan(&bytes, &mut delta)?
                && previous_offset
                    .saturating_add(u64::try_from(bytes.len()).map_err(io::Error::other)?)
                    >= metadata.len();
            if self.incomplete != incomplete {
                self.chain.record_tail_change(incomplete);
            }
            self.incomplete = incomplete;
            self.observed = Some(FileStamp::read(&metadata)?);
            if self.offset > previous_offset {
                return Ok(delta);
            }
            let next = self
                .segment
                .checked_add(1)
                .ok_or_else(|| invalid("segment sequence exhausted"))?;
            if !self.root.join(format!("{next:06}.eclog")).try_exists()? {
                return Ok(delta);
            }
            if let Some(stamp) = self.observed.take() {
                let _previous = self.sealed.insert(self.segment, stamp);
            }
            self.segment = next;
            self.offset = 0;
            self.page = None;
            self.observed = None;
            self.incomplete = false;
        }
    }

    /// Validate a saved frontier without decoding sealed records. Older segments
    /// must still be identical; only the active segment may have grown.
    /// # Errors
    /// Rejects source replacement, truncation, missing segments or IO errors.
    pub fn resume(&self, root: &Path) -> io::Result<()> {
        if self.root != root {
            return Err(invalid("checkpoint belongs to another chain"));
        }
        for (index, stamp) in self.sealed.iter() {
            let metadata = std::fs::metadata(root.join(format!("{index:06}.eclog")))?;
            if &FileStamp::read(&metadata)? != stamp {
                return Err(invalid("sealed checkpoint segment changed"));
            }
        }
        if self.observed.is_some() {
            self.validate_frontier(&std::fs::metadata(
                root.join(format!("{:06}.eclog", self.segment)),
            )?)?;
        }
        Ok(())
    }

    fn validate_frontier(&self, metadata: &Metadata) -> io::Result<()> {
        if let Some(previous) = &self.observed {
            let current = FileStamp::read(metadata)?;
            if current.identity != previous.identity
                || current.length < previous.length
                || (current.length == previous.length && current.modified != previous.modified)
            {
                return Err(invalid(
                    "chain frontier changed non-monotonically; reload required",
                ));
            }
        }
        Ok(())
    }

    fn scan(&mut self, bytes: &[u8], delta: &mut ChainDelta) -> io::Result<bool> {
        let mut scanner = PageScanner::resume(bytes, self.page);
        let mut records = Vec::new();
        // Validate the bounded slice before mutating canonical admission.
        for item in scanner.by_ref() {
            match item {
                Ok(ScanItem::Page { .. }) => {}
                Ok(ScanItem::Record(record)) => records.push(record),
                Err(error) if error.kind == ScanErrorKind::IncompleteTail => break,
                Err(error) => return Err(invalid(error.to_string())),
            }
        }
        for record in records {
            let Ok(op) = decode_op(record.data) else {
                delta.work.undecodable = delta.work.undecodable.saturating_add(1);
                self.chain.record_undecodable();
                continue;
            };
            delta.work.records_decoded = delta.work.records_decoded.saturating_add(1);
            let location = OpRecordLocation {
                segment_seq: self.segment,
                data_offset: self
                    .offset
                    .checked_add(u64::try_from(record.data_offset).map_err(io::Error::other)?)
                    .ok_or_else(|| invalid("record offset exhausted"))?,
                data_len: u32::try_from(record.data.len()).map_err(io::Error::other)?,
            };
            let op = Arc::new(op);
            match self
                .chain
                .admit_record(Arc::clone(&op), record.data, location)?
            {
                Admission::Accepted => {
                    drop(delta.added.insert(op.id, (op, location)));
                }
                Admission::Duplicate => {}
                Admission::Conflict => {
                    drop(delta.added.remove(&op.id));
                    let _: bool = delta.removed.insert(op.id);
                }
            }
        }
        self.offset = self
            .offset
            .checked_add(u64::try_from(scanner.consumed()).map_err(io::Error::other)?)
            .ok_or_else(|| invalid("segment offset exhausted"))?;
        self.page = scanner.page_sequence();
        Ok(scanner.consumed() < bytes.len())
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FileStamp {
    identity: (u64, u64),
    length: u64,
    modified: std::time::SystemTime,
}
impl FileStamp {
    fn read(metadata: &Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt as _;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let identity = {
            let created = metadata
                .created()?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?;
            (created.as_secs(), u64::from(created.subsec_nanos()))
        };
        Ok(Self {
            identity,
            length: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}
