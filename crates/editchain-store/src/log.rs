//! Portable append-log contracts and exact-byte operation admission.

use std::io;

use editchain_core::Admission;

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

    /// Re-establish durability of all readable evidence before a replay is acked.
    ///
    /// # Errors
    /// Returns an error if evidence or its publication cannot be persisted.
    fn sync(&self) -> io::Result<()>;
}

/// Exact operation admission over a caller-supplied append-log adapter.
///
/// This layer classifies against original bytes, retains conflicting variants,
/// and never acknowledges a duplicate based on readability alone. It replays
/// on each call so a failed write cannot leave an optimistic admission cache.
/// Derived/incremental indexes can be layered over the same evidence later.
#[derive(Debug)]
pub struct LogStore<L> {
    log: L,
}

impl<L: AppendLog> LogStore<L> {
    /// Use an adapter whose writer ownership covers this store's lifetime.
    pub const fn new(log: L) -> Self {
        Self { log }
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
        if !u32::try_from(encoded.len()).is_ok_and(|len| len <= MAX_RECORD_BYTES) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "record too large",
            ));
        }
        let op = decode_op(encoded)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let admission = self.snapshot()?.evidence().classify(op.id, encoded);
        if admission != Admission::Accepted {
            // A new conflicting variant also depends on retained evidence that
            // may have come from an earlier append with an uncertain outcome.
            self.log.sync()?;
        }
        if admission != Admission::Duplicate {
            self.log.append_record(0, encoded)?;
        }
        Ok(admission)
    }

    /// Return the adapter, retaining its ownership and any unacknowledged bytes.
    pub fn into_inner(self) -> L {
        self.log
    }
}
