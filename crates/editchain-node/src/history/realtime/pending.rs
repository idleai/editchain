//! Keep raw peer imports out of the graph until their authored rows are ready.

use super::{LiveChanges, LiveRow, LiveWorkspace, Result};
use crate::receipts::Receipts;
use editchain_core::OpKind;

impl LiveWorkspace {
    pub(super) fn pending_imports<'a>(
        &self,
        rows: impl Iterator<Item = (&'a String, &'a LiveRow)>,
    ) -> Result<Vec<String>> {
        let mut pending = Vec::new();
        let mut receipts = None;
        for (key, row) in rows {
            let Some(source) = self.projection.operation(row.anchor) else {
                continue;
            };
            if !matches!(source.kind, OpKind::Import(_))
                || editchain_project::human::work_record(source).is_some()
                || self.projection.import_ready(row.anchor)
            {
                continue;
            }
            // Read provenance once per affected batch, and only when it has an
            // unverified import. Normal appends and completed items skip it.
            if receipts.is_none() {
                receipts = Some(Receipts::read(&self.chain)?);
            }
            if let Some(receipts) = &receipts {
                if receipts.foreign(self.tail.chain(), &self.chain, row.anchor)? {
                    pending.push(key.clone());
                }
            }
        }
        Ok(pending)
    }

    pub(super) fn remove_pending_imports(&mut self) -> Result<()> {
        self.poisoned = true;
        let removed = self
            .pending_imports(self.inputs.iter())?
            .into_iter()
            .collect();
        let (removed, blocks) = self.apply_blocks(LiveChanges {
            removed,
            ..LiveChanges::default()
        })?;
        drop(self.connect(&removed, blocks)?);
        Ok(())
    }
}
