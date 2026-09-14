//! Apply conditional native windows without discarding unchanged presentation.

use super::{
    host, ExpandedRow, HistoryAppState, RequestBody, SnapshotPhase, Step, Viewport, MAX_RENDER_ROWS,
};
use editchain_protocol::{
    ErrorCode, HistoryRow, ReconcileRowsRequest, ReconciledWindow, ServiceError,
};
use std::collections::HashSet;

impl HistoryAppState {
    pub(super) fn request_disclosure(&mut self, abs: i64, task: bool, step: &mut Step) {
        let Some(row) = self.cache.get_by_index(abs) else {
            return;
        };
        let key = row.continuity_key().to_owned();
        self.request_disclosure_key(key, task, step);
    }

    /// Native disclosure targets the clicked identity even while its old
    /// coordinates are retired and the previous window is still on screen.
    pub(crate) fn request_disclosure_key(&mut self, key: String, task: bool, step: &mut Step) {
        let pending = self
            .pending_disclosures
            .entry((key.clone(), task))
            .or_default();
        *pending = pending.saturating_add(1);
        step.ops.push(super::DomOp::DisclosurePending {
            key: key.clone(),
            task,
            pending: true,
        });
        step.sends.push(super::Send::ToggleDisclosure { key, task });
    }

    pub(super) fn disclosure_done(&mut self, body: Option<serde_json::Value>, step: &mut Step) {
        let Some(body) = body else {
            return;
        };
        let Some(key) = body.get("key").and_then(serde_json::Value::as_str) else {
            return;
        };
        let task = body
            .get("task")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let identity = (key.to_owned(), task);
        let Some(count) = self.pending_disclosures.get_mut(&identity) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            let _removed = self.pending_disclosures.remove(&identity);
            step.ops.push(super::DomOp::DisclosurePending {
                key: key.to_owned(),
                task,
                pending: false,
            });
        }
        if let Some(error) = body.get("error").and_then(serde_json::Value::as_str) {
            Self::announce(&format!("Could not change disclosure: {error}"), step);
        }
    }

    pub(crate) fn reconciles_rows(&self) -> bool {
        self.remote
            .as_ref()
            .is_some_and(|remote| remote.reconcile_rows)
    }

    pub(super) fn handle_reconciled_window(
        &mut self,
        value: serde_json::Value,
        request: &ReconcileRowsRequest,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        let result = host::decode::<ReconciledWindow>(value).and_then(|window| {
            self.validate_reconciliation(&window, request)?;
            Ok(window)
        });
        let window = match result {
            Ok(window) => window,
            Err(error) => {
                self.fail_response(&RequestBody::ReconcileRows(request.clone()), &error, step);
                return;
            }
        };
        let entries = match self.cache.reconcile(window.rows) {
            Ok(entries) => entries,
            Err(error) => {
                self.fail_response(&RequestBody::ReconcileRows(request.clone()), &error, step);
                return;
            }
        };
        for (slot, entry) in entries.into_iter().enumerate() {
            let position = i64::try_from(
                window
                    .offset
                    .saturating_add(u64::try_from(slot).unwrap_or(0)),
            )
            .unwrap_or(0);
            if let Some(position) = ExpandedRow::new(position) {
                self.total_fetched = self.total_fetched.saturating_add(1);
                drop(self.cache.insert(position, entry));
            }
        }
        self.phase = SnapshotPhase::LayoutReady;
        self.max_lane = u32::try_from(window.max_lane).unwrap_or(0);
        if self.live.is_some() {
            if self
                .live
                .as_ref()
                .is_some_and(super::live::LiveUpdate::awaiting_open_or_locations)
            {
                self.handle_live_locations(
                    serde_json::json!({
                        "snapshot_id": window.snapshot_id, "rows": window.locations
                    }),
                    step,
                );
            }
            if self.live.is_some() {
                self.apply_live_window(step);
            }
        } else {
            self.finish_window_response(viewport, step, super::WindowPass::Content);
        }
    }

    fn validate_reconciliation(
        &self,
        window: &ReconciledWindow,
        request: &ReconcileRowsRequest,
    ) -> Result<(), ServiceError> {
        let count = u64::try_from(window.rows.len()).unwrap_or(u64::MAX);
        let expected = request
            .anchors
            .iter()
            .find_map(|key| {
                window
                    .locations
                    .iter()
                    .find(|location| &location.key == key)
                    .map(|location| location.row)
            })
            .unwrap_or(request.offset)
            .min(window.total.saturating_sub(1))
            .saturating_sub(u64::from(request.before));
        if window.snapshot_id != self.snapshot_id
            || window.snapshot_id != request.snapshot_id
            || i64::try_from(window.total).ok() != self.total
            || window.total > u64::try_from(MAX_RENDER_ROWS).unwrap_or(0)
            || u32::try_from(window.max_lane).is_err()
            || window.offset != expected
            || count > u64::from(request.limit)
            || window
                .offset
                .checked_add(count)
                .is_none_or(|end| end > window.total)
            || (count == 0 && window.offset < window.total)
        {
            return Err(invalid(
                "Conditional window exceeds its snapshot or requested bounds.",
            ));
        }
        let mut keys = HashSet::new();
        if window.locations.iter().any(|location| {
            location.row >= window.total
                || !request.keys.contains(&location.key)
                || !keys.insert(&location.key)
        }) {
            return Err(invalid("Conditional window contains invalid anchors."));
        }
        keys.clear();
        if window.rows.iter().any(|row| {
            !keys.insert(&row.cached.key)
                || row.cached.version.len() != 64
                || !row
                    .cached
                    .version
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || row
                    .content
                    .as_ref()
                    .is_some_and(|content| !bounded(content))
                || (row.content.is_none()
                    && !request.known.iter().any(|known| {
                        known.key == row.cached.key && known.version == row.cached.version
                    }))
        }) {
            return Err(invalid(
                "Conditional window contains unadvertised or invalid row content.",
            ));
        }
        Ok(())
    }
}

fn bounded(row: &HistoryRow) -> bool {
    row.content.as_ref().is_none_or(|content| {
        content.is_bounded()
            && row.summary.len() <= editchain_protocol::MAX_ROW_TEXT_BYTES
            && row
                .sub_ops
                .iter()
                .all(|child| child.summary.len() <= editchain_protocol::MAX_ROW_TEXT_BYTES)
    })
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, message)
}
