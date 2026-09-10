//! Capture results and the ordered operation/checkpoint persistence handoff.

use std::collections::BTreeMap;

use editchain_core::Op;

use crate::error::ImportError;
use crate::model::ImportReport;
use crate::sink::{CursorStore, CursorValue, MemoryOpSink, OpSink};

/// Admission outcomes confirmed by a durable operation writer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DurableAdmission {
    /// Distinct operation variants appended and synced, including conflicts.
    pub written: usize,
    /// Exact variants already retained, requiring no further append.
    pub duplicates: usize,
    /// New conflicting variants retained as evidence and excluded from history.
    pub conflicts: usize,
}

/// Operation persistence required before source checkpoints can advance.
pub trait DurableOpSink {
    /// Retain every distinct variant and sync its bytes and directory entries.
    /// Exact duplicates may be skipped; conflicting variants must be retained.
    ///
    /// # Errors
    ///
    /// Returns an error if encoding, admission, append, or sync fails. A partial
    /// append is safe: the batch will be replayed with the same checkpoints.
    fn append_durable(&mut self, ops: &[Op]) -> Result<DurableAdmission, ImportError>;
}

/// Successfully persisted operations and their corresponding checkpoints.
#[derive(Debug)]
pub struct DurableImport {
    /// Provider capture counts for this batch.
    pub report: ImportReport,
    /// Actual durable operation admission counts.
    pub admission: DurableAdmission,
}

/// A complete capture whose checkpoints have not yet reached a cursor store.
///
/// Blob sinks must retain payloads before accepting their references. This
/// batch then orders durable operations before durable source checkpoints.
/// Dropping it without persistence leaves source cursors unchanged.
#[derive(Debug)]
pub struct ImportBatch {
    report: ImportReport,
    ops: Vec<Op>,
    checkpoints: CheckpointChanges,
}

impl ImportBatch {
    /// Run a provider capture with a private cursor overlay. Any capture error
    /// discards all proposed cursor and generation changes for the invocation.
    ///
    /// # Errors
    ///
    /// Returns the provider's capture error, preserving the base cursor store.
    pub fn capture(
        cursors: &dyn CursorStore,
        run: impl FnOnce(&mut dyn OpSink, &mut dyn CursorStore) -> Result<ImportReport, ImportError>,
    ) -> Result<Self, ImportError> {
        let mut ops = MemoryOpSink::new();
        let mut pending = PendingCursors {
            base: cursors,
            changes: CheckpointChanges::default(),
        };
        let report = run(&mut ops, &mut pending)?;
        Ok(Self {
            report,
            ops: ops.ops,
            checkpoints: pending.changes,
        })
    }

    /// Provider capture counts, before persistence.
    #[must_use]
    pub const fn report(&self) -> &ImportReport {
        &self.report
    }

    /// Captured source operations and derived evidence in emission order.
    #[must_use]
    pub fn operations(&self) -> &[Op] {
        &self.ops
    }

    /// Add exact reconciliation evidence to the same durable batch.
    pub fn extend_operations(&mut self, operations: impl IntoIterator<Item = Op>) {
        self.ops.extend(operations);
    }

    /// Persist operations, then the source checkpoints covered by this batch.
    ///
    /// A crash after append and before checkpoint intentionally replays the
    /// same immutable operations; canonical admission collapses exact repeats.
    /// Empty captures still persist their checkpoints and generation changes.
    ///
    /// # Errors
    ///
    /// Returns reservation, append, or cursor errors. Physical source IDs are
    /// reserved before append; accepted checkpoints advance only after the
    /// operation writer confirms durable acceptance of the entire batch.
    pub fn persist(
        self,
        writer: &mut dyn DurableOpSink,
        cursors: &mut dyn CursorStore,
    ) -> Result<DurableImport, ImportError> {
        // Reserve physical IDs before a possibly partial append. A later
        // source rewrite must not reuse an ID already written by this attempt.
        for (key, cursor) in &self.checkpoints.cursors {
            cursors.reserve_checkpoint(key, cursor)?;
        }
        let admission = writer.append_durable(&self.ops)?;
        self.checkpoints.stage(cursors)?;
        cursors.commit()?;
        Ok(DurableImport {
            report: self.report,
            admission,
        })
    }
}

#[derive(Debug, Default)]
struct CheckpointChanges {
    cursors: BTreeMap<String, CursorValue>,
    generations: BTreeMap<String, u32>,
}

impl CheckpointChanges {
    fn stage(self, store: &mut dyn CursorStore) -> Result<(), ImportError> {
        for (key, generation) in self.generations {
            store.set_generation(&key, generation)?;
        }
        for (key, cursor) in self.cursors {
            store.set_cursor(&key, &cursor)?;
        }
        Ok(())
    }
}

struct PendingCursors<'a> {
    base: &'a dyn CursorStore,
    changes: CheckpointChanges,
}

impl CursorStore for PendingCursors<'_> {
    fn get_reservation(&self, key: &str) -> Result<Option<CursorValue>, ImportError> {
        self.base.get_reservation(key)
    }

    fn reserve_checkpoint(&mut self, _key: &str, _cursor: &CursorValue) -> Result<(), ImportError> {
        Err(ImportError::CursorStore(
            "capture cannot persist source reservations".into(),
        ))
    }

    fn get_cursor(&self, key: &str) -> Result<Option<CursorValue>, ImportError> {
        match self.changes.cursors.get(key) {
            Some(cursor) => Ok(Some(cursor.clone())),
            None => self.base.get_cursor(key),
        }
    }

    fn set_cursor(&mut self, key: &str, cursor: &CursorValue) -> Result<(), ImportError> {
        drop(self.changes.cursors.insert(key.to_string(), cursor.clone()));
        Ok(())
    }

    fn get_generation(&self, key: &str) -> Result<u32, ImportError> {
        match self.changes.generations.get(key) {
            Some(generation) => Ok(*generation),
            None => self.base.get_generation(key),
        }
    }

    fn set_generation(&mut self, key: &str, generation: u32) -> Result<(), ImportError> {
        let _: Option<u32> = self.changes.generations.insert(key.to_string(), generation);
        Ok(())
    }
}
