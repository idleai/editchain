//! Portable append-log contracts and exact-byte operation admission.

use std::io;

use editchain_core::{Admission, OpSet};

use crate::format::{decode_op, MAX_RECORD_BYTES};
use crate::CanonicalChain;

/// A fallible visitor receiving record flags and borrowed, unmodified bytes.
pub type RecordVisitor<'a> = dyn FnMut(u8, &[u8]) -> io::Result<()> + 'a;

/// Diagnostics from replaying the complete records in a log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogReadStats {
    /// Retained incomplete suffixes, excluded from complete-record replay.
    pub incomplete_tails: usize,
}

/// An exclusively owned append transaction over immutable record evidence.
///
/// Adapters must serialize writers for this handle's lifetime, including the
/// read/classify/append sequence in [`LogStore`]. Readers may observe an
/// unacknowledged complete prefix after an interrupted append. A successful
/// append or sync must establish durable persistence, including publication
/// metadata. Errors have an unknown commit outcome: callers retry exact bytes.
///
/// Implementations must preserve record order, flags and bytes, retain partial
/// tails for diagnosis, and reject corrupt framing rather than skipping it.
/// There is no path, workspace, identity allocation or encoding normalization
/// requirement in this interface.
pub trait AppendLog {
    /// Visit complete records, including duplicates and conflicting variants.
    ///
    /// # Errors
    /// Returns replay, integrity or visitor errors. An incomplete trailing write
    /// is reported in the result and must not hide later complete segments.
    fn visit_records(&self, visitor: &mut RecordVisitor<'_>) -> io::Result<LogReadStats>;

    /// Durably append one exact record; flags are independent of its bytes.
    ///
    /// # Errors
    /// Returns size, integrity or persistence errors. Failure may retain bytes.
    fn append_record(&mut self, flags: u8, encoded: &[u8]) -> io::Result<()>;

    /// Durably append an ordered batch of exact records.
    ///
    /// Adapters may share a durability fence across records. Success makes the
    /// entire batch durable; failure may leave any complete prefix and an
    /// incomplete tail. The default preserves the single-record contract.
    ///
    /// # Errors
    /// Returns validation or persistence errors with an unknown commit outcome.
    fn append_records(&mut self, records: &[(u8, &[u8])]) -> io::Result<()> {
        for (flags, bytes) in records {
            self.append_record(*flags, bytes)?;
        }
        Ok(())
    }

    /// Re-establish durability of all readable evidence before a replay is acked.
    ///
    /// # Errors
    /// Returns an error if evidence or its publication cannot be persisted.
    fn sync(&self) -> io::Result<()>;
}

/// Exact operation admission over a caller-supplied append-log adapter.
///
/// This layer classifies against original bytes, retains conflicting variants,
/// and never acknowledges a duplicate based on readability alone. Admission
/// evidence is reused only while the adapter's exclusive writer ownership is
/// held. Any uncertain write discards that state before the next admission.
#[derive(Debug)]
pub struct LogStore<L> {
    log: L,
    evidence: Option<OpSet>,
}

impl<L: AppendLog> LogStore<L> {
    /// Use an adapter whose writer ownership covers this store's lifetime.
    pub const fn new(log: L) -> Self {
        Self {
            log,
            evidence: None,
        }
    }

    /// Read accepted operations, conflicts and incomplete-content diagnostics.
    ///
    /// # Errors
    /// Returns log replay and integrity errors.
    pub fn snapshot(&self) -> io::Result<CanonicalChain> {
        CanonicalChain::read_log(&self.log)
    }

    /// Append supported operation bytes unchanged, or acknowledge an exact replay.
    ///
    /// A conflict is durably retained before returning [`Admission::Conflict`].
    /// All variants of that identity are excluded from accepted history. A
    /// duplicate of a quarantined variant never revives the identity.
    ///
    /// # Errors
    /// Rejects oversized/undecodable input and log read/write/sync failures.
    /// Retry the same bytes after a persistence error; it may have committed.
    pub fn append_encoded(&mut self, encoded: &[u8]) -> io::Result<Admission> {
        self.append_encoded_batch(&[encoded])?
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("single-record admission returned no result"))
    }

    /// Admit an ordered batch with one replay per exclusive writer lifetime.
    ///
    /// All input is validated before writing. Intra-batch duplicates and
    /// conflicts use exact bytes and the same rules as separate appends. No
    /// result is returned until every new variant is durable. A later conflict
    /// can quarantine an earlier accepted identity in this batch.
    ///
    /// # Errors
    /// Returns encoding, framing or persistence errors. After an uncertain
    /// persistence result, the next call replays the log and fences duplicates
    /// before acknowledging them. Retrying the entire batch is safe.
    pub fn append_encoded_batch(&mut self, records: &[&[u8]]) -> io::Result<Vec<Admission>> {
        let ids = records
            .iter()
            .map(|bytes| validate_record(bytes))
            .collect::<io::Result<Vec<_>>>()?;
        if records.is_empty() {
            return Ok(Vec::new());
        }
        // Take ownership before any fallible IO. An error drops speculative
        // evidence, even if the adapter retained bytes before reporting failure.
        let mut evidence = match self.evidence.take() {
            Some(evidence) => evidence,
            None => read_evidence(&self.log)?,
        };
        let mut admissions = Vec::with_capacity(records.len());
        let mut pending = Vec::new();
        let mut fence = false;
        for (id, bytes) in ids.into_iter().zip(records) {
            let admission = evidence.insert(id, bytes.to_vec());
            fence |= admission != Admission::Accepted;
            if admission != Admission::Duplicate {
                pending.push((0, *bytes));
            }
            admissions.push(admission);
        }
        if fence {
            self.log.sync()?;
        }
        self.log.append_records(&pending)?;
        self.evidence = Some(evidence);
        Ok(admissions)
    }

    /// Return the adapter, retaining its ownership and any unacknowledged bytes.
    pub fn into_inner(self) -> L {
        self.log
    }
}

fn validate_record(bytes: &[u8]) -> io::Result<editchain_core::OpId> {
    if !u32::try_from(bytes.len()).is_ok_and(|len| len <= MAX_RECORD_BYTES) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "record too large",
        ));
    }
    decode_op(bytes)
        .map(|op| op.id)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn read_evidence(log: &impl AppendLog) -> io::Result<OpSet> {
    let mut evidence = OpSet::new();
    let _stats = log.visit_records(&mut |_flags, bytes| {
        if let Ok(op) = decode_op(bytes) {
            let _admission = evidence.insert(op.id, bytes.to_vec());
        }
        Ok(())
    })?;
    Ok(evidence)
}
