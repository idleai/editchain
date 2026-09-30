//! Capture results and the ordered operation/checkpoint persistence handoff.

use std::collections::BTreeMap;

use editchain_core::Op;

use crate::error::ImportError;
use crate::model::ImportReport;
use crate::sink::{BatchLimits, CursorStore, CursorValue, MemoryOpSink, OpSink};

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

impl<L: editchain_store::AppendLog> DurableOpSink for editchain_store::LogStore<L> {
    fn append_durable(&mut self, ops: &[Op]) -> Result<DurableAdmission, ImportError> {
        let mut result = DurableAdmission::default();
        let mut records = Vec::new();
        let mut bytes = 0_usize;
        for op in ops {
            let encoded = editchain_store::format::encode_op(op).map_err(std::io::Error::other)?;
            bytes = bytes.saturating_add(encoded.len());
            records.push(encoded);
            if records.len() >= 1024 || bytes >= 4 * 1024 * 1024 {
                persist_records(self, &records, &mut result)?;
                records.clear();
                bytes = 0;
            }
        }
        persist_records(self, &records, &mut result)?;
        Ok(result)
    }
}

fn persist_records<L: editchain_store::AppendLog>(
    writer: &mut editchain_store::LogStore<L>,
    records: &[Vec<u8>],
    result: &mut DurableAdmission,
) -> Result<(), ImportError> {
    let borrowed: Vec<_> = records.iter().map(Vec::as_slice).collect();
    for admission in writer.append_encoded_batch(&borrowed)? {
        match admission {
            editchain_core::Admission::Accepted => {
                result.written = result.written.saturating_add(1);
            }
            editchain_core::Admission::Duplicate => {
                result.duplicates = result.duplicates.saturating_add(1);
            }
            editchain_core::Admission::Conflict => {
                result.written = result.written.saturating_add(1);
                result.conflicts = result.conflicts.saturating_add(1);
            }
        }
    }
    Ok(())
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
    ops: MemoryOpSink,
    checkpoints: CheckpointChanges,
}

impl ImportBatch {
    /// Convert this private capture to schema three before durable acceptance.
    /// Checkpoints stay private until the converted records have been synced.
    /// # Errors
    /// Returns conversion, validation, or batch-limit errors.
    pub fn into_schema3(mut self, blobs: &mut dyn crate::BlobSink) -> Result<Self, ImportError> {
        self = self.convert_schema3(blobs, false)?;
        Ok(self)
    }

    /// Convert new input for a previously migrated chain, using its fixed ID namespace.
    /// # Errors
    /// Returns conversion, validation, or batch-limit errors.
    pub fn into_migrated_schema3(
        self,
        blobs: &mut dyn crate::BlobSink,
    ) -> Result<Self, ImportError> {
        self.convert_schema3(blobs, true)
    }

    /// Retain only exact source records from an already converted private batch.
    /// Use an independent cursor namespace so later normalization can backfill.
    /// # Errors
    /// Returns admission or size-limit errors.
    pub fn originals_only(mut self) -> Result<Self, ImportError> {
        let mut originals = MemoryOpSink::with_limits(self.ops.limits());
        for op in self.ops.ops {
            if matches!(&op.kind, editchain_core::OpKind::Activity(record) if matches!(record.kind, editchain_core::activity::Kind::Original(_)))
            {
                let _admission = originals.accept_op(&op)?;
            }
        }
        self.ops = originals;
        self.report.normalized_ops = 0;
        Ok(self)
    }

    fn convert_schema3(
        mut self,
        blobs: &mut dyn crate::BlobSink,
        migration: bool,
    ) -> Result<Self, ImportError> {
        let mut converter = if migration {
            crate::activity::Converter::for_migration()
        } else {
            crate::activity::Converter::default()
        };
        converter.protect_conflicts(crate::activity::conflicts(
            self.ops.source_context.values().chain(&self.ops.ops),
        )?);
        for op in self.ops.source_context.values().chain(&self.ops.ops) {
            let stored = crate::activity::resolve_original(op, blobs)?;
            if stored.is_none()
                && matches!(&op.kind, editchain_core::OpKind::Import(raw) if matches!(raw.raw_ref, editchain_core::Payload::Blob(_)))
            {
                return Err(ImportError::BlobSink(
                    "schema-three capture requires readable source blobs; source checkpoints were not advanced".into(),
                ));
            }
            converter.observe(op, stored.as_deref())?;
        }
        converter.finish_observations();
        let mut records = Vec::new();
        for op in &self.ops.ops {
            records.extend(converter.convert(op, blobs)?);
        }
        let mut ops = MemoryOpSink::with_limits(self.ops.limits());
        self.report.raw_ops = 0;
        self.report.normalized_ops = 0;
        self.report.evidence_ops = 0;
        for op in records {
            let _admission = ops.accept_op(&op)?;
            if matches!(&op.kind, editchain_core::OpKind::Activity(record) if matches!(record.kind, editchain_core::activity::Kind::Original(_)))
            {
                self.report.raw_ops = self.report.raw_ops.saturating_add(1);
            } else {
                self.report.normalized_ops = self.report.normalized_ops.saturating_add(1);
            }
        }
        self.ops = ops;
        Ok(self)
    }

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
        Self::capture_bounded(cursors, BatchLimits::default(), run)
    }

    /// Capture with explicit aggregate bounds, including reconciliation added
    /// later through [`Self::extend_operations`]. Exact duplicates need no
    /// additional capacity; conflicting variants consume their full budget.
    ///
    /// # Errors
    ///
    /// Returns capture/admission errors without staging any base checkpoint.
    pub fn capture_bounded(
        cursors: &dyn CursorStore,
        limits: BatchLimits,
        run: impl FnOnce(&mut dyn OpSink, &mut dyn CursorStore) -> Result<ImportReport, ImportError>,
    ) -> Result<Self, ImportError> {
        let mut ops = MemoryOpSink::with_limits(limits);
        let mut pending = PendingCursors {
            base: cursors,
            changes: CheckpointChanges::default(),
        };
        let report = run(&mut ops, &mut pending)?;
        Ok(Self {
            report,
            ops,
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
        &self.ops.ops
    }

    /// Proposed resumable source positions, for inspection before acceptance.
    /// These become accepted only when [`Self::persist`] succeeds; saving them
    /// earlier can skip evidence after a failed operation or blob write.
    pub fn proposed_cursors(&self) -> impl Iterator<Item = (&str, &CursorValue)> {
        self.checkpoints
            .cursors
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    /// Add exact reconciliation evidence to the same durable batch.
    ///
    /// # Errors
    ///
    /// Returns an admission or limit error, consuming the failed batch so a
    /// partial reconciliation cannot advance its checkpoints.
    pub fn extend_operations(
        mut self,
        operations: impl IntoIterator<Item = Op>,
    ) -> Result<Self, ImportError> {
        for op in operations {
            crate::sink::emit_op(
                &op,
                &mut self.ops,
                &mut self.report,
                crate::sink::EmissionKind::Derived,
            )?;
        }
        Ok(self)
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
        let operations = self.ops.into_operations();
        // Reserve physical IDs before a possibly partial append. A later
        // source rewrite must not reuse an ID already written by this attempt.
        for (key, cursor) in &self.checkpoints.cursors {
            cursors.reserve_checkpoint(key, cursor)?;
        }
        let admission = writer.append_durable(&operations)?;
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
