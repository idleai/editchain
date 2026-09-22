//! Retry only rows with unresolved display content, including across restarts.

use super::{LiveChanges, LiveRow, LiveWorkspace, Result};
use editchain_core::BlobRef;
use editchain_index::Map;
use editchain_store::{BlobPreviewResolution, BlobReader};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
struct Waiting {
    input: LiveRow,
    blobs: Vec<BlobRef>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct PendingContent {
    rows: Map<String, Waiting>,
}

impl PendingContent {
    pub(super) fn observe(&mut self, input: &LiveRow, blobs: Vec<BlobRef>) {
        if blobs.is_empty() {
            self.remove(&input.key);
        } else {
            drop(self.rows.insert(
                input.key.clone(),
                Waiting {
                    input: input.clone(),
                    blobs,
                },
            ));
        }
    }

    pub(super) fn remove(&mut self, key: &str) {
        drop(self.rows.remove(key));
    }

    pub(super) fn ready(&self, reader: &BlobReader) -> BTreeMap<String, LiveRow> {
        self.rows
            .iter()
            .filter(|(_, waiting)| {
                // Check only unresolved references, without reading their payloads.
                // Presentation does its normal bounded reads once a dependency arrives.
                waiting
                    .blobs
                    .iter()
                    .any(|blob| matches!(reader.preview(blob, 0), BlobPreviewResolution::Found(_)))
            })
            .map(|(key, waiting)| (key.clone(), waiting.input.clone()))
            .collect()
    }
}

impl LiveWorkspace {
    /// Older checkpoints did not retain content dependencies. Revisit only
    /// blob-backed inputs once, repairing already available summaries as well.
    pub(super) fn restore_content_rows(&mut self) -> Result<()> {
        self.poisoned = true;
        let upserts = self
            .inputs
            .iter()
            .filter(|(_, input)| {
                input
                    .operations
                    .iter()
                    .any(|op| super::super::payloads::uses_blob_preview(&op.kind))
            })
            .map(|(key, input)| (key.clone(), input.clone()))
            .collect();
        let (removed, blocks) = self.apply_blocks(LiveChanges {
            upserts,
            ..Default::default()
        })?;
        drop(self.connect(&removed, blocks)?);
        Ok(())
    }

    pub(super) fn refresh_pending_content(&mut self) -> Result<bool> {
        let upserts = self.content.ready(&self.blobs);
        if upserts.is_empty() {
            return Ok(false);
        }
        self.poisoned = true;
        let (removed, blocks) = self.apply_blocks(LiveChanges {
            upserts,
            ..Default::default()
        })?;
        drop(self.connect(&removed, blocks)?);
        Ok(true)
    }
}

pub(super) fn include_ready(changes: &mut LiveChanges, ready: BTreeMap<String, LiveRow>) {
    for (key, input) in ready {
        // A concurrent operation change owns the row's newest content or removal.
        if !changes.removed.contains(&key) && !changes.upserts.contains_key(&key) {
            changes.work.presentation_ops = changes
                .work
                .presentation_ops
                .saturating_add(input.operations.len());
            drop(changes.upserts.insert(key, input));
        }
    }
}
