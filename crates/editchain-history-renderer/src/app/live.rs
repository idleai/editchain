//! Live snapshot handover. Old DOM stays visible until the anchored replacement
//! viewport is validated and cached; service coordinates never mix generations.

use std::collections::{BTreeSet, HashSet};

use editchain_protocol::{LocateRowsRequest, LocateRowsResponse, RowLocation};

use super::{
    get_window, host, DomOp, ExpandedRow, FindCounterState, HistoryAppState, OpenResponse,
    RequestBody, Send, SnapshotPhase, Step, Unwrapped, Value, Viewport, MAX_RENDER_ROWS, PAGE,
    ROW_H,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    WaitingOpen,
    Locating,
    Loading,
}

#[derive(Debug, Clone)]
pub(in crate::app) struct LiveUpdate {
    stage: Stage,
    anchors: Vec<String>,
    selected: Option<String>,
    focused: Option<String>,
    expanded: BTreeSet<String>,
    previous_viewport: Viewport,
    locations: Vec<RowLocation>,
    pub(super) viewport: Option<Viewport>,
}

impl LiveUpdate {
    pub(in crate::app) fn for_delta(viewport: Viewport) -> Self {
        Self {
            stage: Stage::Loading,
            anchors: Vec::new(),
            selected: None,
            focused: None,
            expanded: BTreeSet::new(),
            previous_viewport: viewport,
            locations: Vec::new(),
            viewport: Some(viewport),
        }
    }
    pub(super) fn awaiting_open_or_locations(&self) -> bool {
        self.stage != Stage::Loading
    }

    fn capture(state: &HistoryAppState, viewport: &Viewport) -> Self {
        let top = HistoryAppState::viewport_visible_top(viewport);
        let anchor = state
            .abs_index_for_visible(top)
            .and_then(|abs| state.cache.get_by_index(abs));
        let mut anchors = Vec::new();
        if viewport.scroll_top.get() > 0 {
            if let Some(row) = anchor {
                anchors.push(row.continuity_key().to_owned());
                if let Some(parent) = row
                    .source
                    .parent_row
                    .and_then(|abs| i64::try_from(abs).ok())
                    .and_then(|abs| state.cache.get_by_index(abs))
                {
                    anchors.push(parent.continuity_key().to_owned());
                }
            }
            for visible in [top.saturating_add(1), top.saturating_sub(1)] {
                if let Some(row) = state
                    .abs_index_for_visible(visible)
                    .and_then(|abs| state.cache.get_by_index(abs))
                {
                    anchors.push(row.continuity_key().to_owned());
                }
            }
        }
        Self {
            stage: Stage::WaitingOpen,
            anchors,
            selected: state.selection.continuity_key().map(str::to_owned),
            focused: state
                .cache
                .get_by_index(state.roving_abs())
                .map(|row| row.continuity_key().to_owned()),
            expanded: state.expanded_keys.clone(),
            previous_viewport: *viewport,
            locations: Vec::new(),
            viewport: None,
        }
    }

    fn keys(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.anchors
            .iter()
            .chain(self.selected.iter())
            .chain(self.focused.iter())
            .chain(self.expanded.iter())
            .filter(|key| seen.insert((*key).clone()))
            .take(2000)
            .cloned()
            .collect()
    }

    fn location(&self, key: &str) -> Option<&RowLocation> {
        self.locations.iter().find(|row| row.key == key)
    }
}

impl HistoryAppState {
    pub(super) fn locate_remote_anchors(&mut self, step: &mut Step) {
        let Some(live) = &mut self.live else {
            return;
        };
        live.stage = Stage::Locating;
        live.locations.clear();
        live.viewport = None;
        let request = RequestBody::LocateRows(LocateRowsRequest {
            snapshot_id: self.snapshot_id.clone(),
            keys: live.keys(),
        });
        let _issued = self.issue_request(&request, None, step);
    }
    pub(super) fn pause_live(&mut self, viewport: &Viewport, step: &mut Step) {
        if self.live.is_some() {
            return;
        }
        self.live = Some(LiveUpdate::capture(self, viewport));
        self.requests.clear();
        step.ops.push(DomOp::ProgressiveLoader(false));
    }

    pub(super) fn handle_live_open(
        &mut self,
        body: Option<Value>,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        let opened = match host::unwrap(body) {
            Unwrapped::Ok(value) => host::decode::<OpenResponse>(value).and_then(|opened| {
                opened.validate()?;
                Ok(opened)
            }),
            Unwrapped::Err(error) => Err(error),
        };
        let opened = match opened {
            Ok(opened) if opened.nodes <= u64::try_from(MAX_RENDER_ROWS).unwrap_or(0) => opened,
            Ok(_) => {
                self.fail_snapshot(step, "History coordinates exceed the exact pixel range.");
                return;
            }
            Err(error) => {
                self.fail_snapshot(step, &error.message);
                return;
            }
        };
        // Capture the latest reading position after the potentially slow Open.
        let mut live = self
            .live
            .as_ref()
            .filter(|live| live.stage != Stage::WaitingOpen)
            .cloned()
            .unwrap_or_else(|| LiveUpdate::capture(self, viewport));
        live.stage = Stage::Locating;
        live.locations.clear();
        live.viewport = None;
        let keys = live.keys();
        self.live = Some(live);
        self.snapshot_id = opened.snapshot_id;
        self.view_gen = self.view_gen.saturating_add(1);
        self.requests.clear();
        self.reset_find_state();
        self.cache.clear();
        self.expansion = None;
        if let Some(baseline) = opened.live.as_ref().filter(|baseline| baseline.paged) {
            if let Err(error) = self.open_remote(baseline) {
                self.fail_snapshot(step, &error.message);
                return;
            }
        }
        self.total = Some(i64::try_from(opened.nodes).unwrap_or(0));
        self.total_fetched = 0;
        self.phase = SnapshotPhase::Opening;
        self.open_warnings = opened.warnings;
        step.ops.push(DomOp::FindCounter(FindCounterState::Hidden));
        let request = RequestBody::LocateRows(LocateRowsRequest {
            snapshot_id: self.snapshot_id.clone(),
            keys,
        });
        let _: Option<u64> = self.issue_request(&request, None, step);
    }

    pub(super) fn handle_live_locations(&mut self, value: Value, step: &mut Step) {
        let result = host::decode::<LocateRowsResponse>(value);
        let response = match result {
            Ok(response) if response.snapshot_id == self.snapshot_id => response,
            Ok(_) => {
                self.fail_snapshot(step, "Live anchors belong to a different snapshot.");
                return;
            }
            Err(error) => {
                self.fail_snapshot(step, &error.message);
                return;
            }
        };
        let Some(live) = &mut self.live else {
            return;
        };
        let keys = live.keys();
        let mut seen = HashSet::new();
        if response.rows.iter().any(|row| {
            !keys.contains(&row.key)
                || !seen.insert(&row.key)
                || row.row > u64::try_from(MAX_RENDER_ROWS).unwrap_or(0)
        }) {
            self.fail_snapshot(step, "Invalid live anchor coordinates.");
            return;
        }
        live.locations = response.rows;
        live.stage = Stage::Loading;
        if self.remote.is_some() {
            let state = live.clone();
            let viewport = self.restore_live_anchors(&state);
            if let Some(live) = &mut self.live {
                live.viewport = Some(viewport);
            }
            self.fetch_window(&viewport, step);
            return;
        }
        let request = get_window(
            &self.snapshot_id,
            0,
            u64::try_from(PAGE).unwrap_or(500),
            true,
        );
        let _: Option<u64> = self.issue_request(&request, None, step);
    }

    fn restore_live_anchors(&mut self, live: &LiveUpdate) -> Viewport {
        self.expanded_keys.clear();
        if let Some(index) = self.expansion.as_mut().filter(|_| self.remote.is_none()) {
            for location in &live.locations {
                if live.expanded.contains(&location.key) {
                    if let Some(row) = i64::try_from(location.row).ok().and_then(ExpandedRow::new) {
                        if index.toggle(row) {
                            let _: bool = self.expanded_keys.insert(location.key.clone());
                        }
                    }
                }
            }
        }
        self.clear_selection();
        if let Some(selected) = live.selected.as_deref().and_then(|key| live.location(key)) {
            self.selection
                .select_with_continuity(&selected.node_key, &selected.key);
        }
        let focused = live
            .focused
            .as_deref()
            .and_then(|key| live.location(key))
            .and_then(|row| i64::try_from(row.row).ok())
            .and_then(ExpandedRow::new);
        self.selection.set_roving(focused);
        let anchor = live
            .anchors
            .iter()
            .filter_map(|key| live.location(key))
            .filter_map(|location| i64::try_from(location.row).ok())
            .find_map(|row| self.visible_index_for_abs(row));
        let previous = live.previous_viewport;
        let top = if previous.scroll_top.get() == 0 {
            0
        } else {
            anchor
                .unwrap_or_else(|| Self::viewport_visible_top(&previous))
                .min(self.visible_total().saturating_sub(1))
                .saturating_mul(ROW_H)
                .saturating_add(previous.scroll_top.get().rem_euclid(ROW_H))
        };
        Viewport::new(top, previous.client_height.get())
    }

    pub(super) fn apply_live_window(&mut self, step: &mut Step) {
        // The normal two-pass handler may have scheduled a reanchor. Publish
        // only once all visible replacement rows exist under the new topology.
        step.ops.clear();
        let Some(live) = self.live.clone() else {
            return;
        };
        if live
            .locations
            .iter()
            .any(|row| row.row >= u64::try_from(self.total.unwrap_or(0)).unwrap_or(0))
        {
            self.fail_snapshot(step, "Live anchors exceed the snapshot total.");
            return;
        }
        let viewport = live
            .viewport
            .unwrap_or_else(|| self.restore_live_anchors(&live));
        if let Some(live) = &mut self.live {
            live.viewport = Some(viewport);
        }
        if !self.evict_far_windows(&viewport) {
            self.fail_snapshot(
                step,
                "Visible history rows exceed the retained content budget.",
            );
            return;
        }
        let first = Self::viewport_visible_top(&viewport);
        let last = self.viewport_visible_bottom(&viewport);
        let ready = self.total == Some(0)
            || (first..=last).all(|visible| {
                self.abs_index_for_visible(visible)
                    .is_some_and(|abs| self.cache.get_by_index(abs).is_some())
            });
        if !ready {
            self.fetch_window(&viewport, step);
            return;
        }
        self.live = None;
        let (top, bottom) = self.desired_visible_range(&viewport);
        self.render_top = top;
        self.render_bottom = bottom;
        self.phase = SnapshotPhase::LayoutReady;
        step.ops.push(DomOp::RefreshHeader);
        if self.total == Some(0) {
            Self::show_view_message(step, "No history found in this workspace", false);
        } else {
            step.ops.push(DomOp::ReanchorLive {
                top,
                bottom,
                scroll_top: viewport.scroll_top.get(),
            });
        }
        step.ops.push(DomOp::ProgressiveLoader(true));
        step.save_state = Some(Self::persisted_state(&viewport));
        step.sends.push(Send::LiveSettled {
            snapshot_id: self.snapshot_id.as_str().to_owned(),
            error: None,
        });
        Self::announce("History updated", step);
        self.report_status(&viewport, step);
        self.finish_remote_find(&viewport, step);
    }

    pub(super) fn fail_live(&mut self, message: &str, step: &mut Step) {
        if self.live.take().is_some() {
            step.sends.push(Send::LiveSettled {
                snapshot_id: self.snapshot_id.as_str().to_owned(),
                error: Some(message.to_owned()),
            });
        }
    }
}
