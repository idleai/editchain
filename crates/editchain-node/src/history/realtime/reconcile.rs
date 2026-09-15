//! Bounded viewport reconciliation without resending unchanged row content.

use super::{LiveWorkspace, Result};
use editchain_protocol::{
    CachedRow, GetWindowRequest, ReconcileRowsRequest, ReconciledRow, ReconciledWindow,
};
use std::collections::HashMap;

impl LiveWorkspace {
    pub(super) fn reconcile_rows(
        &self,
        request: &ReconcileRowsRequest,
    ) -> Result<ReconciledWindow> {
        if !self.paged() {
            return Err("native window reconciliation was not negotiated".into());
        }
        let locations = self.locate(&request.keys)?.rows;
        let anchor = request
            .anchors
            .iter()
            .find_map(|key| {
                locations
                    .iter()
                    .find(|location| &location.key == key)
                    .map(|location| location.row)
            })
            .unwrap_or(request.offset);
        let offset = anchor
            .min(self.blocks.measure().visible.saturating_sub(1))
            .saturating_sub(u64::from(request.before));
        let window = self.paged_window(&GetWindowRequest {
            snapshot_id: request.snapshot_id.clone(),
            offset,
            limit: u64::from(request.limit),
            include_layout: true,
        })?;
        let known: HashMap<_, _> = request
            .known
            .iter()
            .map(|row| (row.key.as_str(), row.version.as_str()))
            .collect();
        let rows = window
            .rows
            .into_iter()
            .map(|row| {
                let key = if row.continuity_key.is_empty() {
                    row.node_key.clone()
                } else {
                    row.continuity_key.clone()
                };
                // Include content, source identity, disclosure, and decorated graph
                // geometry. A matching identity alone cannot establish safe reuse.
                let version = blake3::hash(&serde_json::to_vec(&row)?)
                    .to_hex()
                    .to_string();
                let unchanged = known
                    .get(key.as_str())
                    .is_some_and(|known| *known == version);
                Ok(ReconciledRow {
                    cached: CachedRow { key, version },
                    content: (!unchanged).then_some(row),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ReconciledWindow {
            snapshot_id: window.snapshot_id,
            locations,
            offset,
            rows,
            total: window.total,
            chain_generation: window.chain_generation,
            max_lane: window.max_lane,
        })
    }
}
