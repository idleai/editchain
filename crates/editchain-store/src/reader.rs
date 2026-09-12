use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use crate::format::scan::{PageScanner, ScanErrorKind, ScanItem, MAX_RECORD_BYTES};
use crate::format::{decode_op, encode_op};
use editchain_core::{Admission, Op, OpId, OpSet};

use crate::segment::segment_sequences;

/// Canonicalization and integrity outcomes for a chain read.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct ChainReadStats {
    /// Successfully decoded record occurrences, including exact replays.
    pub records: usize,
    /// Unconflicted operation identities accepted into the view.
    pub accepted: usize,
    /// Repeated occurrences of already retained exact bytes.
    pub duplicates: usize,
    /// Distinct record variants belonging to conflicted IDs, including the first.
    pub quarantined: usize,
    /// Complete records that cannot be decoded by the supported operation schema.
    #[serde(default)]
    pub undecodable: usize,
    /// Segments ending with an incomplete page header or record.
    #[serde(default)]
    pub incomplete_tails: usize,
}

/// Exact location of one encoded operation inside an append-only segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpRecordLocation {
    /// Numeric sequence from `<sequence>.eclog`.
    pub segment_seq: u32,
    /// Byte offset of the encoded operation, after length and flags.
    pub data_offset: u64,
    /// Encoded operation length in bytes.
    pub data_len: u32,
}

/// Retained evidence and decoded accepted operations from one read.
///
/// The accepted corpus always follows the core byte-admission rule. In-memory
/// candidates can be added for reconciliation using the same encoding and rule.
/// Neither reading nor admitting candidates writes to the filesystem.
#[derive(Debug, Default)]
pub struct CanonicalChain {
    evidence: OpSet,
    accepted: BTreeMap<OpId, (Op, Option<OpRecordLocation>)>,
    stats: ChainReadStats,
}

impl CanonicalChain {
    /// Read all complete records without creating or modifying the chain.
    ///
    /// A missing chain is empty. Incomplete tails and undecodable records are
    /// counted; complete source bytes remain in their original segment files.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable files, sequence gaps, corrupt framing,
    /// unsupported page formats, or record lengths beyond the codec bound.
    pub fn read(chain_dir: &Path) -> io::Result<Self> {
        let mut chain = Self::default();
        for segment_seq in segment_sequences(chain_dir)? {
            let bytes = fs::read(chain_dir.join(format!("{segment_seq:06}.eclog")))?;
            chain.scan(segment_seq, &bytes)?;
        }
        Ok(chain)
    }

    fn scan(&mut self, segment_seq: u32, bytes: &[u8]) -> io::Result<()> {
        for item in PageScanner::new(bytes) {
            match item {
                Ok(ScanItem::Page { .. }) => {}
                Ok(ScanItem::Record(record)) => {
                    let location = OpRecordLocation {
                        segment_seq,
                        data_offset: u64::try_from(record.data_offset).map_err(io::Error::other)?,
                        data_len: u32::try_from(record.data.len()).map_err(io::Error::other)?,
                    };
                    match decode_op(record.data) {
                        Ok(op) => {
                            let _: Admission = self.admit(op, record.data.to_vec(), Some(location));
                        }
                        Err(_) => {
                            self.stats.undecodable = self.stats.undecodable.saturating_add(1);
                        }
                    }
                }
                Err(error) if error.kind == ScanErrorKind::IncompleteTail => {
                    self.stats.incomplete_tails = self.stats.incomplete_tails.saturating_add(1);
                    break;
                }
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
            }
        }
        Ok(())
    }

    /// Admit a proposed operation using exactly the persisted encoder.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation cannot be encoded within the record bound.
    pub fn insert(&mut self, op: Op) -> io::Result<Admission> {
        let encoded = encode_op(&op).map_err(io::Error::other)?;
        if encoded.len() > usize::try_from(MAX_RECORD_BYTES).map_err(io::Error::other)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "operation is too large",
            ));
        }
        Ok(self.admit(op, encoded, None))
    }

    pub(crate) fn admit(
        &mut self,
        op: Op,
        encoded: Vec<u8>,
        location: Option<OpRecordLocation>,
    ) -> Admission {
        self.stats.records = self.stats.records.saturating_add(1);
        let was_accepted = self.evidence.contains(&op.id);
        let result = self.evidence.insert(op.id, encoded);
        match result {
            Admission::Accepted => {
                drop(self.accepted.insert(op.id, (op, location)));
            }
            Admission::Duplicate => {
                self.stats.duplicates = self.stats.duplicates.saturating_add(1);
            }
            Admission::Conflict => {
                drop(self.accepted.remove(&op.id));
                self.stats.quarantined =
                    self.stats
                        .quarantined
                        .saturating_add(if was_accepted { 2 } else { 1 });
            }
        }
        result
    }

    /// Final counts for the currently accepted corpus and all retained evidence.
    #[must_use]
    pub fn stats(&self) -> ChainReadStats {
        ChainReadStats {
            accepted: self.accepted.len(),
            ..self.stats
        }
    }

    /// All distinct encoded evidence, including every conflicted variant.
    #[must_use]
    pub const fn evidence(&self) -> &OpSet {
        &self.evidence
    }

    /// Read one accepted identity without scanning the retained corpus.
    #[must_use]
    pub fn get(&self, id: OpId) -> Option<&Op> {
        self.accepted.get(&id).map(|(op, _)| op)
    }

    /// Iterate retained accepted operations without consuming the admission index.
    pub fn located_ops(&self) -> impl Iterator<Item = (&Op, Option<OpRecordLocation>)> {
        self.accepted.values().map(|(op, location)| (op, *location))
    }

    pub(crate) fn record_undecodable(&mut self) {
        self.stats.undecodable = self.stats.undecodable.saturating_add(1);
    }

    pub(crate) fn record_tail_change(&mut self, is_incomplete: bool) {
        if is_incomplete {
            self.stats.incomplete_tails = self.stats.incomplete_tails.saturating_add(1);
        } else {
            self.stats.incomplete_tails = self.stats.incomplete_tails.saturating_sub(1);
        }
    }

    /// Consume accepted operations and their first durable locations in ID order.
    /// In-memory candidates have no durable location until appended and read back.
    pub fn into_located_ops(self) -> impl Iterator<Item = (Op, Option<OpRecordLocation>)> {
        self.accepted.into_values()
    }
}

/// Decode one operation directly from its indexed segment-record location.
///
/// # Errors
///
/// Returns an error if the location is oversized, unreadable, or undecodable.
pub fn read_op_at(chain_dir: &Path, location: OpRecordLocation) -> io::Result<Op> {
    if location.data_len > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "record location is too large",
        ));
    }
    let path = chain_dir.join(format!("{:06}.eclog", location.segment_seq));
    let mut file = File::open(path)?;
    let _: u64 = file.seek(SeekFrom::Start(location.data_offset))?;
    let mut encoded = vec![0u8; usize::try_from(location.data_len).map_err(io::Error::other)?];
    file.read_exact(&mut encoded)?;
    decode_op(&encoded).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
