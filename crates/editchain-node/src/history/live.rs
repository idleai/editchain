//! Snapshot-bound identity lookup for live viewport reconciliation.

use std::collections::HashSet;

use editchain_protocol::{LocateRowsResponse, RowLocation};

use super::{HistoryWindowOptions, Workspace};

impl Workspace {
    /// Resolve a bounded set of presentation anchors in the fixed opened view.
    /// This first live implementation scans bounded pages, including cached views;
    /// it never builds the lexical index or hydrates detail payloads.
    ///
    /// # Errors
    ///
    /// Returns a row-read error without publishing partial coordinates.
    pub fn locate_rows(
        &mut self,
        keys: &[String],
    ) -> Result<LocateRowsResponse, Box<dyn std::error::Error>> {
        let mut remaining: HashSet<&str> = keys.iter().map(String::as_str).collect();
        let mut rows = Vec::new();
        let mut offset = 0u64;
        while !remaining.is_empty() {
            let page = self.history_window(HistoryWindowOptions {
                offset,
                limit: 256,
                include_layout: false,
            })?;
            for (index, row) in page.rows.iter().enumerate() {
                let key = if row.continuity_key.is_empty() {
                    &row.node_key
                } else {
                    &row.continuity_key
                };
                if remaining.remove(key.as_str()) {
                    rows.push(RowLocation {
                        key: key.clone(),
                        node_key: row.node_key.clone(),
                        row: offset.saturating_add(u64::try_from(index)?),
                    });
                }
            }
            offset = offset.saturating_add(u64::try_from(page.rows.len())?);
            if offset >= page.total || page.rows.is_empty() {
                break;
            }
        }
        Ok(LocateRowsResponse {
            snapshot_id: self.snapshot_id().clone(),
            rows,
        })
    }
}
