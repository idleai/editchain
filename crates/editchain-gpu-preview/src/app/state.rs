//! The Rust `HistoryApp` state machine.
//!
//! This is a faithful native port of the legacy JS controller's view-state
//! logic: view/search generations, request correlation (including
//! synchronous fixture-response reentrancy), the sparse window cache, virtual
//! paging decisions (`PAGE=500`, `BUFFER=400`, `ROW_H=34`), profile
//! switching, find/search sessions, expansion (sub-op reveal), persistence,
//! and the DOM operation plans the webview shell executes.
//!
//! The state machine is pure: it never touches the DOM or the VS Code API.
//! Every transition produces a [`Step`] describing what to send to the host,
//! what to render, and whether to persist state, so native tests can drive
//! the exact production interactions (profile switches, scroll paging, find
//! navigation, stale-response races) deterministically.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::host::{
    self, find_in_history, get_window, ChainFilter, HostMessage, Id, LoggedRequest, Send, Unwrapped,
};

/// Fixed row height in CSS pixels (`ROW_H = 34` — contract value).
pub(crate) const ROW_H: i64 = 34;

/// Rows fetched per request (`PAGE`).
pub(crate) const PAGE: i64 = 500;
/// Rows kept rendered past each edge of the viewport (`BUFFER`).
pub(crate) const BUFFER: i64 = 400;
/// Candidate cap for find-in-chain (`FIND_TOP_K`).
pub(crate) const FIND_TOP_K: i64 = 50;
/// Fixed submodule hiding for the current view (`FIXED_HIDE_SUBMODULES`).
pub(crate) const FIXED_HIDE_SUBMODULES: bool = true;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Profile {
    Activity,
    Raw,
}

impl Profile {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Profile::Activity => "Activity",
            Profile::Raw => "Raw",
        }
    }

    pub(crate) fn hide_trace(self) -> bool {
        self == Profile::Activity
    }
}

/// How a profile switch is applied (mirrors the production `setProfile` opts:
/// the user action resets coherently; open/reveal restores in memory only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileAction {
    /// Coherent reset: bump the view generation, drop the cache/snapshot, and
    /// refetch offset 0 under the new profile, persisting `{profile, topRow: 0}`
    /// immediately.
    Reset,
    /// Restore path (open/reveal): apply the profile in memory only; the caller
    /// persists the profile once the restored viewport is applied.
    Restore,
}

/// Viewport measurements the renderer observes (CSS pixels).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Viewport {
    pub(crate) scroll_top: i64,
    pub(crate) client_height: i64,
}

impl Viewport {
    pub(crate) fn new(scroll_top: i64, client_height: i64) -> Viewport {
        Viewport {
            scroll_top,
            client_height,
        }
    }
}

/// An in-flight service request (correlated by id).
#[derive(Debug, Clone)]
pub(crate) struct InFlight {
    pub(crate) body: Value,
    pub(crate) gen_tag: u64,
    pub(crate) search_epoch: Option<u64>,
}

/// A normalized find-in-chain match.
#[derive(Debug, Clone)]
pub(crate) struct FindMatch {
    pub(crate) row: i64,
    #[cfg(test)]
    pub(crate) node_key: String,
    #[cfg(test)]
    pub(crate) summary: String,
}

/// A find jump awaiting its target window in cache.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FindTarget {
    pub(crate) abs: i64,
    pub(crate) index: usize,
}

/// What the DOM shell should do after a state transition. The shell owns DOM
/// node mutation; the state machine owns the index/pixel decisions.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DomOp {
    /// Replace `#rows` with a full-pane message.
    ShowMessage { text: String, error: bool },
    /// Replace `#rows` with a request error + Retry button; `retry` names the
    /// recovery the shell must re-run.
    ShowRequestError { text: String, retry: RetryAction },
    /// Rebuild the whole rendered window `[top, bottom]` (visible indices).
    Reanchor { top: i64, bottom: i64 },
    /// Append contiguous visible rows `[from, to]` below the window.
    AppendBelow { from: i64, to: i64 },
    /// Prepend contiguous visible rows `[from, to]` above the window.
    PrependAbove { from: i64, to: i64 },
    /// Trim rows above `keep_top` (visible index).
    TrimTop { keep_top: i64 },
    /// Trim rows below `keep_bottom` (visible index).
    TrimBottom { keep_bottom: i64 },
    /// Replace placeholder rows whose data arrived (shell-local).
    FillPlaceholders,
    /// Rebuild the sticky header (maxLane changed).
    RefreshHeader,
    /// Set `#rows.scrollTop` to a raw pixel offset (whole CSS pixels; the
    /// shell applies them to the element, which accepts fractional values).
    SetScrollTop(i64),
    /// Set `#rows.scrollTop` from a persisted visible top row index (the
    /// shell clamps it against the real scroll range).
    RestoreScrollTop { row_index: i64 },
    /// Mark a cached absolute row as the current find match + inline selection.
    SetFindHighlight { abs: i64 },
    /// Clear the find highlight + inline selection.
    ClearFindHighlight,
    /// Reveal the row element for `abs` with the minimal header-aware scroll.
    RevealRow { abs: i64 },
    /// Update the find counter UI.
    FindCounter(FindCounterState),
    /// Start or stop the progressive loader timer (shell concern).
    ProgressiveLoader(bool),
}

/// Recovery action for a terminal request error (the explicit Retry button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryAction {
    /// `resetHistory()` — reload the full history from the top.
    ResetHistory,
    /// Re-issue the current legacy Search query with a fresh epoch.
    ResubmitSearch,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FindCounterState {
    /// A find query is in flight (searching state).
    Pending,
    Zero,
    Error(String),
    Settled {
        index: usize,
        total: usize,
        more: bool,
    },
    Hidden,
}

/// One state transition: host sends, ordered DOM ops, and optional persisted
/// webview state (`vscode.setState`). Renderer readiness is owned solely by
/// [`ViewFlags::data_ready`] (the shell reads the authoritative flag, so no
/// per-step copy exists here).
#[derive(Debug, Clone, Default)]
pub(crate) struct Step {
    pub(crate) sends: Vec<Send>,
    pub(crate) ops: Vec<DomOp>,
    pub(crate) save_state: Option<Value>,
}

impl Step {
    pub(crate) fn new() -> Step {
        Step::default()
    }
}

/// Read-only view-mode flags (grouped so the struct stays under the
/// `struct_excessive_bools` gate and reads as one unit).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ViewFlags {
    /// Legacy flat-list search mode; while active the virtual-scroll
    /// machinery is suspended.
    pub(crate) search_mode: bool,
    /// Harness/parity readiness: true once a terminal event correlated with
    /// actual content has been processed.
    pub(crate) data_ready: bool,
}

/// Find-in-chain session flags.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FindFlags {
    /// A find session has settled.
    pub(crate) active: bool,
    /// The response was truncated by the candidate cap (`more`).
    pub(crate) more: bool,
}

/// Snapshot/layout readiness flags for the current view generation.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SessionFlags {
    /// The offset-0 expansion snapshot has been received for this view.
    pub(crate) snapshot_established: bool,
    /// Global lane geometry has been received (two-pass hydration done).
    pub(crate) layout_ready: bool,
    /// The initial "Loaded N history rows" announcement was made.
    pub(crate) announced_initial_load: bool,
}

/// The full view state machine.
#[derive(Debug, Clone)]
pub(crate) struct HistoryAppState {
    // --- request correlation ----------------------------------------------
    pub(crate) next_req_id: u64,
    pub(crate) in_flight: BTreeMap<u64, InFlight>,
    pub(crate) pending_window_req_id: Option<u64>,
    pub(crate) view_gen: u64,
    // --- search epoch (find/search correlation) ---------------------------
    pub(crate) search_epoch: u64,
    pub(crate) current_search_epoch: Option<u64>,
    // --- view state -------------------------------------------------------
    pub(crate) profile: Profile,
    /// Authoritative total; `None` = unknown (fresh view / after a reset).
    pub(crate) total: Option<i64>,
    pub(crate) cache: BTreeMap<i64, Value>,
    pub(crate) total_fetched: u64,
    pub(crate) max_lane: u32,
    pub(crate) open_warnings: Vec<String>,
    /// Persisted webview state from `vscode.getState()` (profile/topRow).
    pub(crate) persisted: Option<Value>,
    // --- expansion snapshot -----------------------------------------------
    pub(crate) session_flags: SessionFlags,
    pub(crate) sub_op_counts: Vec<u32>,
    pub(crate) block_starts: Vec<i64>,
    /// Absolute expanded-row index to contiguous descendant count.
    pub(crate) expansion_spans: BTreeMap<i64, u32>,
    /// Absolute expandable rows whose direct children are currently visible.
    pub(crate) expanded_rows: BTreeSet<i64>,
    /// Visible-index to absolute-index map after applying nested disclosure.
    pub(crate) visible_abs: Vec<i64>,
    // --- render window (visible indices) ----------------------------------
    pub(crate) render_top: i64,
    pub(crate) render_bottom: i64,
    // --- selection / roving focus -----------------------------------------
    /// The selected row's stable `node_key` (`selectedRowKey`), if any.
    pub(crate) selected_key: Option<String>,
    /// The roving-tabindex row's absolute index (`rovingAbs`); `-1` = unset.
    pub(crate) roving_abs: i64,
    // --- find-in-chain ------------------------------------------------------
    pub(crate) find_flags: FindFlags,
    pub(crate) find_matches: Vec<FindMatch>,
    pub(crate) find_total: usize,
    pub(crate) find_index: usize,
    pub(crate) pending_find_target: Option<FindTarget>,
    // --- legacy flat-list search --------------------------------------------
    pub(crate) view_flags: ViewFlags,
    pub(crate) search_query: String,
    // --- debug / harness parity ----------------------------------------------
    pub(crate) request_log: Vec<LoggedRequest>,
}

impl Default for HistoryAppState {
    fn default() -> Self {
        let mut state = HistoryAppState {
            next_req_id: 1,
            in_flight: BTreeMap::new(),
            pending_window_req_id: None,
            view_gen: 0,
            session_flags: SessionFlags::default(),
            search_epoch: 0,
            current_search_epoch: None,
            profile: Profile::Activity,
            total: None,
            cache: BTreeMap::new(),
            total_fetched: 0,
            max_lane: 2,
            open_warnings: Vec::new(),
            persisted: None,
            sub_op_counts: Vec::new(),
            block_starts: Vec::new(),
            expansion_spans: BTreeMap::new(),
            expanded_rows: BTreeSet::new(),
            visible_abs: Vec::new(),
            render_top: 0,
            render_bottom: -1,
            selected_key: None,
            roving_abs: -1,
            find_flags: FindFlags::default(),
            find_matches: Vec::new(),
            find_total: 0,
            find_index: 0,
            pending_find_target: None,
            view_flags: ViewFlags::default(),
            search_query: String::new(),
            request_log: Vec::new(),
        };
        state.recompute_expansion();
        state
    }
}

impl HistoryAppState {
    // --- filter payloads -----------------------------------------------------

    pub(crate) fn filter_payload(&self) -> ChainFilter {
        ChainFilter {
            summary_pattern: String::new(),
            kind_pattern: String::new(),
            include_kind_pattern: String::new(),
            hide_undated: true,
            splice: true,
            hide_trace: self.profile.hide_trace(),
        }
    }

    pub(crate) fn hide_submodules() -> bool {
        FIXED_HIDE_SUBMODULES
    }

    // --- expansion mapping ---------------------------------------------------

    fn clear_expansion_state(&mut self) {
        self.sub_op_counts.clear();
        self.block_starts.clear();
        self.expansion_spans.clear();
        self.expanded_rows.clear();
        self.visible_abs.clear();
    }

    pub(crate) fn recompute_expansion(&mut self) {
        let n = self.sub_op_counts.len();
        let mut starts = Vec::with_capacity(n);
        let mut acc: i64 = 0;
        for count in &self.sub_op_counts {
            starts.push(acc);
            acc = acc.saturating_add(1).saturating_add(i64::from(*count));
        }
        self.block_starts = starts;

        // Older services expose only one-level top-level counts. Derive the
        // equivalent generalized spans so the same walker handles both wire
        // versions. A current service supplies explicit spans, including inner
        // bundle rows nested under a work group.
        if self.expansion_spans.is_empty() {
            for (index, count) in self.sub_op_counts.iter().copied().enumerate() {
                if count > 0 {
                    if let Some(start) = self.block_starts.get(index).copied() {
                        let _: Option<u32> = self.expansion_spans.insert(start, count);
                    }
                }
            }
        }
        self.expanded_rows
            .retain(|row| self.expansion_spans.contains_key(row));

        self.visible_abs.clear();
        let expansion_index_ready = self.session_flags.snapshot_established
            || !self.sub_op_counts.is_empty()
            || !self.expansion_spans.is_empty();
        if !expansion_index_ready {
            return;
        }
        let total = self.total.unwrap_or(0).max(0);
        let mut abs = 0i64;
        while abs < total {
            self.visible_abs.push(abs);
            if let Some(descendants) = self.expansion_spans.get(&abs).copied() {
                if !self.expanded_rows.contains(&abs) {
                    abs = abs.saturating_add(1).saturating_add(i64::from(descendants));
                    continue;
                }
            }
            abs = abs.saturating_add(1);
        }
    }

    /// Total number of visible rows given the current reveal state.
    pub(crate) fn visible_total(&self) -> i64 {
        if self.session_flags.snapshot_established
            || !self.sub_op_counts.is_empty()
            || !self.expansion_spans.is_empty()
        {
            return i64::try_from(self.visible_abs.len())
                .unwrap_or(i64::MAX)
                .max(1);
        }
        self.total.unwrap_or(0).max(1)
    }

    /// Map a visible index back to its absolute slot index (or `None` for a
    /// hidden collapsed sub-op slot). Identity before the snapshot arrives.
    pub(crate) fn abs_index_for_visible(&self, vis: i64) -> Option<i64> {
        if vis < 0 {
            return None;
        }
        if self.session_flags.snapshot_established
            || !self.sub_op_counts.is_empty()
            || !self.expansion_spans.is_empty()
        {
            return usize::try_from(vis)
                .ok()
                .and_then(|index| self.visible_abs.get(index).copied());
        }
        Some(vis)
    }

    /// Map an absolute slot index back to its visible index (inverse of
    /// `abs_index_for_visible`); `None` for a hidden collapsed sub-op slot.
    pub(crate) fn visible_index_for_abs(&self, abs: i64) -> Option<i64> {
        if self.session_flags.snapshot_established
            || !self.sub_op_counts.is_empty()
            || !self.expansion_spans.is_empty()
        {
            return self
                .visible_abs
                .binary_search(&abs)
                .ok()
                .and_then(|index| i64::try_from(index).ok());
        }
        Some(abs)
    }

    /// Index of the top-level node whose block starts at `abs_parent_row`.
    #[cfg(test)]
    pub(crate) fn block_index_of_abs(&self, abs_parent_row: i64) -> Option<usize> {
        let mut lo = 0usize;
        let mut hi = self.block_starts.len();
        while lo < hi {
            let mid = lo.saturating_add(hi) >> 1;
            if self.block_starts.get(mid).copied().unwrap_or(0) <= abs_parent_row {
                lo = lo.saturating_add(1);
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return None;
        }
        let b = lo.saturating_sub(1);
        if b < self.block_starts.len()
            && self.block_starts.get(b).copied().unwrap_or(0) == abs_parent_row
        {
            Some(b)
        } else {
            None
        }
    }

    /// Toggle any expandable row by absolute expanded-history index.
    pub(crate) fn toggle_expanded(&mut self, abs_parent_row: i64) -> bool {
        if !self.expansion_spans.contains_key(&abs_parent_row) {
            return false;
        }
        if !self.expanded_rows.remove(&abs_parent_row) {
            let _: bool = self.expanded_rows.insert(abs_parent_row);
        }
        self.recompute_expansion();
        true
    }

    /// `toggleExpandFor` — toggle a row's reveal state and plan the
    /// full desired-window rebuild (`reanchorTo(desiredVisibleRange())` plus
    /// `ensureFilled()`), so every newly revealed sub-op slot fills the
    /// viewport instead of only the pre-expansion slice.
    pub(crate) fn toggle_expanded_ui(
        &mut self,
        abs_parent_row: i64,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        if !self.toggle_expanded(abs_parent_row) {
            return;
        }
        let (top, bottom) = self.desired_visible_range(viewport);
        // Reanchor replaces the DOM with exactly this visible range. Keep the
        // reducer's rendered-window bounds in lockstep so the next sync does
        // not append the newly exposed bottom row a second time.
        self.render_top = top;
        self.render_bottom = bottom;
        step.ops.push(DomOp::Reanchor { top, bottom });
        self.fetch_window(viewport, step);
    }

    // --- viewport math ---------------------------------------------------------

    /// Visible row index of the top of the viewport.
    pub(crate) fn viewport_visible_top(viewport: &Viewport) -> i64 {
        viewport.scroll_top.saturating_div(ROW_H).max(0)
    }

    /// Visible row index just below the bottom of the viewport.
    pub(crate) fn viewport_visible_bottom(&self, viewport: &Viewport) -> i64 {
        let top = Self::viewport_visible_top(viewport);
        let bottom = viewport
            .scroll_top
            .saturating_add(viewport.client_height)
            .saturating_div(ROW_H)
            .max(top);
        self.visible_total().saturating_sub(1).min(bottom)
    }

    /// The absolute index range we want cached: viewport ± BUFFER in visible
    /// space mapped to absolute slots.
    pub(crate) fn desired_cache_range(&self, viewport: &Viewport) -> (i64, i64) {
        let v_top = Self::viewport_visible_top(viewport)
            .saturating_sub(BUFFER)
            .max(0);
        let v_bottom = self
            .viewport_visible_bottom(viewport)
            .saturating_add(BUFFER)
            .min(self.visible_total().saturating_sub(1));
        let top_abs = self.abs_index_for_visible(v_top);
        let bottom_abs = self.abs_index_for_visible(v_bottom);
        (
            top_abs.unwrap_or(0),
            bottom_abs.unwrap_or(self.total.unwrap_or(0).max(0)),
        )
    }

    /// The visible index range we want rendered.
    pub(crate) fn desired_visible_range(&self, viewport: &Viewport) -> (i64, i64) {
        (
            Self::viewport_visible_top(viewport)
                .saturating_sub(BUFFER)
                .max(0),
            self.viewport_visible_bottom(viewport)
                .saturating_add(BUFFER)
                .min(self.visible_total().saturating_sub(1)),
        )
    }

    // --- request issuing ---------------------------------------------------------

    /// Register an in-flight request, record the envelope, and push the
    /// correlated `Send`. Window requests also claim the pending-window slot
    /// BEFORE the send is emitted, so a synchronous fixture response (which
    /// arrives inside `postMessage`) correlates correctly.
    fn issue_request(
        &mut self,
        body: Value,
        is_window: bool,
        search_epoch: Option<u64>,
        step: &mut Step,
    ) -> u64 {
        let id = self.next_req_id;
        self.next_req_id = self.next_req_id.saturating_add(1);
        drop(self.in_flight.insert(
            id,
            InFlight {
                body: body.clone(),
                gen_tag: self.view_gen,
                search_epoch,
            },
        ));
        if is_window {
            self.pending_window_req_id = Some(id);
        }
        self.request_log.push(LoggedRequest {
            id,
            body: body.clone(),
        });
        step.sends.push(Send::Request { id, body });
        id
    }

    /// `fetchWindow` — fetch a window of rows around the cacheable range.
    /// Returns the request id, or `None` when nothing should be fetched.
    pub(crate) fn fetch_window(&mut self, viewport: &Viewport, step: &mut Step) {
        if self.view_flags.search_mode
            || self.pending_window_req_id.is_some()
            || self.total == Some(0)
        {
            return;
        }
        let (top, bottom) = self.desired_cache_range(viewport);
        if top > bottom {
            return;
        }
        // The expansion snapshot ships only with the offset-0 window.
        let force_snapshot = !self.session_flags.snapshot_established;
        let range_top = if force_snapshot { 0 } else { top };
        let range_bottom = if force_snapshot {
            (PAGE - 1).min(bottom)
        } else {
            bottom
        };
        if range_top > range_bottom {
            return;
        }
        let start = (range_top..=range_bottom).find(|i| !self.cache.contains_key(i));
        let Some(start) = start else { return };
        let limit = PAGE.min(range_bottom.saturating_sub(start).saturating_add(1));
        let body = get_window(
            start,
            limit,
            &self.filter_payload(),
            self.session_flags.layout_ready,
        );
        let _: u64 = self.issue_request(body, true, None, step);
    }

    /// `fetchWindowAround` — fetch the sparse window around an absolute row.
    pub(crate) fn fetch_window_around(&mut self, abs_row: i64, step: &mut Step) {
        if self.pending_window_req_id.is_some() {
            return;
        }
        let total = self.total.unwrap_or(0);
        if total <= 0 {
            return;
        }
        let force_snapshot = !self.session_flags.snapshot_established;
        let top = abs_row.saturating_sub(BUFFER).max(0);
        let bottom = total.saturating_sub(1).min(abs_row.saturating_add(BUFFER));
        let range_top = if force_snapshot { 0 } else { top };
        let range_bottom = if force_snapshot {
            (PAGE - 1).min(bottom)
        } else {
            bottom
        };
        if range_top > range_bottom {
            return;
        }
        let start = (range_top..=range_bottom).find(|i| !self.cache.contains_key(i));
        let Some(start) = start else { return };
        let limit = PAGE.min(range_bottom.saturating_sub(start).saturating_add(1));
        let body = get_window(
            start,
            limit,
            &self.filter_payload(),
            self.session_flags.layout_ready,
        );
        let _: u64 = self.issue_request(body, true, None, step);
    }

    /// `evictFarWindows` — drop cached rows far outside the desired range.
    pub(crate) fn evict_far_windows(&mut self, viewport: &Viewport) {
        let (top, bottom) = self.desired_cache_range(viewport);
        let keep_min = top.saturating_sub(BUFFER);
        let keep_max = bottom.saturating_add(BUFFER);
        let keys: Vec<i64> = self.cache.keys().copied().collect();
        for (index, key) in keys.iter().enumerate() {
            if *key < keep_min || *key > keep_max {
                drop(self.cache.remove(key));
            }
            if index.saturating_add(1)
                > usize::try_from(PAGE.saturating_mul(4)).unwrap_or(usize::MAX)
            {
                break; // hard cap on retained rows
            }
        }
    }

    /// `syncWindow` — extend/trim the rendered window to the desired visible
    /// range, pushing DOM ops onto `step`. Mirrors the production additive
    /// virtual scroll.
    pub(crate) fn sync_window(&mut self, viewport: &Viewport, step: &mut Step) {
        if self.view_flags.search_mode || self.total.unwrap_or(0) <= 0 {
            return;
        }
        let (want_top, want_bottom) = self.desired_visible_range(viewport);
        let covers_viewport = self.render_top <= Self::viewport_visible_top(viewport)
            && self.render_bottom >= self.viewport_visible_bottom(viewport);
        if self.render_bottom < self.render_top || !covers_viewport {
            step.ops.push(DomOp::Reanchor {
                top: want_top,
                bottom: want_bottom,
            });
            self.render_top = want_top;
            self.render_bottom = want_bottom;
            self.fetch_window(viewport, step);
            return;
        }
        if self.render_bottom < want_bottom {
            let (from, to, added) =
                self.append_rows_below(want_bottom.saturating_sub(self.render_bottom));
            if added > 0 {
                step.ops.push(DomOp::AppendBelow { from, to });
            }
            self.fetch_window(viewport, step);
        }
        if self.render_top > want_top
            || self
                .abs_index_for_visible(self.render_top.saturating_sub(1))
                .is_some_and(|i| self.cache.contains_key(&i))
        {
            let (from, to, added) =
                self.prepend_rows_above(self.render_top.saturating_sub(want_top).max(1));
            if added > 0 {
                step.ops.push(DomOp::PrependAbove { from, to });
            }
            self.fetch_window(viewport, step);
        }
        self.trim_top(want_top, step);
        self.trim_bottom(want_bottom, step);
        step.ops.push(DomOp::FillPlaceholders);
    }

    /// `appendRowsBelow` — extend the window contiguously below. Returns the
    /// visible range actually added and the added count.
    fn append_rows_below(&mut self, n: i64) -> (i64, i64, i64) {
        if n <= 0 || self.render_bottom >= self.visible_total().saturating_sub(1) {
            return (0, -1, 0);
        }
        let mut from = -1;
        let mut to = -1;
        let mut added = 0i64;
        let mut vis = self.render_bottom.saturating_add(1);
        let limit =
            (self.render_bottom.saturating_add(n)).min(self.visible_total().saturating_sub(1));
        while vis <= limit {
            let Some(abs) = self.abs_index_for_visible(vis) else {
                vis = vis.saturating_add(1);
                continue;
            };
            if !self.cache.contains_key(&abs) {
                break; // stop at first gap — keep contiguous
            }
            if from < 0 {
                from = vis;
            }
            to = vis;
            added = added.saturating_add(1);
            self.render_bottom = self.render_bottom.saturating_add(1);
            vis = vis.saturating_add(1);
        }
        if added == 0 {
            return (0, -1, 0);
        }
        (from, to, added)
    }

    /// `prependRowsAbove` — extend the window contiguously above.
    fn prepend_rows_above(&mut self, n: i64) -> (i64, i64, i64) {
        if n <= 0 || self.render_top <= 0 {
            return (0, -1, 0);
        }
        let mut from = -1;
        let mut to = -1;
        let mut added = 0i64;
        let top_vis = self.render_top.saturating_sub(1);
        let mut vis = top_vis.saturating_sub(n).saturating_add(1).max(0);
        while vis <= top_vis {
            let Some(abs) = self.abs_index_for_visible(vis) else {
                vis = vis.saturating_add(1);
                continue;
            };
            if !self.cache.contains_key(&abs) {
                break;
            }
            if from < 0 {
                from = vis;
            }
            to = vis;
            added = added.saturating_add(1);
            self.render_top = self.render_top.saturating_sub(1);
            vis = vis.saturating_add(1);
        }
        if added == 0 {
            return (0, -1, 0);
        }
        (from, to, added)
    }

    /// `trimTop` — remove rows above `keep_top` (visible index).
    pub(crate) fn trim_top(&mut self, keep_top: i64, step: &mut Step) {
        if self.render_top >= keep_top {
            return;
        }
        let removed = keep_top
            .saturating_sub(self.render_top)
            .min(
                self.render_bottom
                    .saturating_sub(self.render_top)
                    .saturating_add(1),
            )
            .max(0);
        if removed <= 0 {
            return;
        }
        self.render_top = self.render_top.saturating_add(removed);
        step.ops.push(DomOp::TrimTop { keep_top });
    }

    /// `trimBottom` — remove rows below `keep_bottom` (visible index).
    pub(crate) fn trim_bottom(&mut self, keep_bottom: i64, step: &mut Step) {
        if self.render_bottom <= keep_bottom {
            return;
        }
        let removed = self
            .render_bottom
            .saturating_sub(keep_bottom)
            .min(
                self.render_bottom
                    .saturating_sub(self.render_top)
                    .saturating_add(1),
            )
            .max(0);
        if removed <= 0 {
            return;
        }
        self.render_bottom = self.render_bottom.saturating_sub(removed);
        step.ops.push(DomOp::TrimBottom { keep_bottom });
    }

    // --- persistence -----------------------------------------------------------

    /// `saveState` — persist only safe profile + visible top row index.
    pub(crate) fn persisted_state(&self, viewport: &Viewport) -> Value {
        json!({
            "profile": match self.profile {
                Profile::Activity => "activity",
                Profile::Raw => "raw",
            },
            "topRow": Self::viewport_visible_top(viewport),
        })
    }

    /// `restoreState` — read `{ topRow, profile }` from persisted state.
    pub(crate) fn restore_state(&self) -> (i64, Profile) {
        let persisted = self.persisted.as_ref();
        let top_row = persisted
            .and_then(|v| v.get("topRow"))
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .unwrap_or(-1);
        let profile = match persisted
            .and_then(|v| v.get("profile"))
            .and_then(Value::as_str)
        {
            Some("raw") => Profile::Raw,
            _ => Profile::Activity,
        };
        (top_row, profile)
    }

    // --- find counter / nav ------------------------------------------------------

    pub(crate) fn find_counter_text(state: &FindCounterState) -> String {
        match state {
            FindCounterState::Zero => "0 of 0".to_owned(),
            FindCounterState::Error(_) => "error".to_owned(),
            FindCounterState::Settled {
                index, total, more, ..
            } => {
                let mut text = format!("{} of {total}", index.saturating_add(1));
                if *more {
                    text.push('+');
                }
                text
            }
            FindCounterState::Pending | FindCounterState::Hidden => String::new(),
        }
    }

    /// `findNavigationEnabled` — exactly the production guard.
    pub(crate) fn find_navigation_enabled(&self, search_value: &str) -> bool {
        self.find_flags.active
            && !self.find_matches.is_empty()
            && self.current_search_epoch.is_none()
            && search_value.trim() == self.search_query
    }

    // --- read-only find/expansion accessors (shell-facing) -----------------------

    /// The settled find session is active (matches received, not cleared).
    pub(crate) fn find_active(&self) -> bool {
        self.find_flags.active
    }

    /// The current find match (the one the viewport is on), if any.
    pub(crate) fn current_find_match(&self) -> Option<&FindMatch> {
        self.find_matches.get(self.find_index)
    }

    /// The current find cursor index into the settled match list.
    pub(crate) fn find_index(&self) -> usize {
        self.find_index
    }

    /// Total matches in the settled session.
    pub(crate) fn find_total(&self) -> usize {
        self.find_total
    }

    /// Whether the backend truncated the candidate list (`more`).
    pub(crate) fn find_more(&self) -> bool {
        self.find_flags.more
    }

    /// The in-flight find/search epoch, if a query is still pending.
    pub(crate) fn current_search_epoch(&self) -> Option<u64> {
        self.current_search_epoch
    }

    /// Whether the top-level block at `block_index` is expanded.
    #[cfg(test)]
    pub(crate) fn is_block_expanded(&self, block_index: usize) -> bool {
        self.block_starts
            .get(block_index)
            .is_some_and(|row| self.expanded_rows.contains(row))
    }

    /// Whether an arbitrary expandable row is expanded.
    pub(crate) fn is_row_expanded(&self, abs: i64) -> bool {
        self.expanded_rows.contains(&abs)
    }

    // --- selection / roving focus --------------------------------------------------

    /// The selected row's stable `node_key`, if any.
    pub(crate) fn selected_key(&self) -> Option<&str> {
        self.selected_key.as_deref()
    }

    /// The roving-tabindex row's absolute index (`-1` = unset).
    pub(crate) fn roving_abs(&self) -> i64 {
        self.roving_abs
    }

    /// `selectRow` — mark the cached row at `abs` as the inline selection.
    /// Rows are rebuilt by virtual scroll, so every render re-applies the
    /// selected class from this key (via [`HistoryAppState::row_context`]).
    pub(crate) fn select_row(&mut self, abs: i64) {
        let Some(row) = self.cache.get(&abs) else {
            return;
        };
        self.selected_key = Some(host::row::owned_str(row, "node_key"));
    }

    /// `clearSelection` — drop the inline selection.
    pub(crate) fn clear_selection(&mut self) {
        self.selected_key = None;
    }

    /// `setRovingAbs` — re-pin the single tab stop to `abs`.
    pub(crate) fn set_roving_abs(&mut self, abs: i64) {
        self.roving_abs = abs;
    }

    /// Build the live shell-facing [`RowContext`] for one rendered row:
    /// selection, find-current, expansion, and roving state are fed from this
    /// machine so every rebuild re-applies them from the authoritative state.
    pub(crate) fn row_context(
        &self,
        view: super::rows::ViewMode,
        abs_index: i64,
        is_group_start: bool,
    ) -> super::rows::RowContext {
        let find_current = self
            .current_find_match()
            .is_some_and(|found| found.row == abs_index);
        let expanded = self.is_row_expanded(abs_index);
        super::rows::RowContext {
            view,
            abs_index,
            is_group_start,
            selected_key: self.selected_key().map(str::to_owned),
            find_current,
            expanded,
            roving_abs: (self.roving_abs() >= 0).then_some(self.roving_abs()),
        }
    }
}

impl HistoryAppState {
    // --- find-in-chain -----------------------------------------------------------

    /// `normalizeFindMatch` — keep only finite, whole, in-range coordinates.
    fn normalize_find_match(&self, m: &Value) -> Option<FindMatch> {
        let raw = m.get("row").cloned().unwrap_or(Value::Null);
        let row = match &raw {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse::<i64>().ok(),
            Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
        };
        let row = row?;
        let total = self.total.unwrap_or(0).max(0);
        if row >= total {
            return None;
        }
        Some(FindMatch {
            row,
            #[cfg(test)]
            node_key: host::row::owned_str(m, "node_key"),
            #[cfg(test)]
            summary: host::row::owned_str(m, "summary"),
        })
    }

    /// `resetFindState` — drop every piece of find state.
    pub(crate) fn reset_find_state(&mut self) {
        self.find_flags.active = false;
        self.find_matches.clear();
        self.find_total = 0;
        self.find_flags.more = false;
        self.find_index = 0;
        self.pending_find_target = None;
    }

    /// `clearFind` — end the session without reloading history.
    pub(crate) fn clear_find(&mut self, step: &mut Step) {
        self.reset_find_state();
        self.search_query.clear();
        self.current_search_epoch = None;
        step.ops.push(DomOp::ClearFindHighlight);
        step.ops.push(DomOp::FindCounter(FindCounterState::Hidden));
    }

    /// `submitFind` — issue a `FindInHistory` request for the trimmed query.
    pub(crate) fn submit_find(&mut self, query: &str, step: &mut Step) {
        query.clone_into(&mut self.search_query);
        self.reset_find_state();
        step.ops.push(DomOp::ClearFindHighlight);
        let epoch = self.search_epoch.saturating_add(1);
        self.search_epoch = epoch;
        self.current_search_epoch = Some(epoch);
        step.ops.push(DomOp::FindCounter(FindCounterState::Pending));
        step.sends
            .push(Send::StatusText(format!("Searching for \"{query}\"")));
        let body = find_in_history(
            query,
            FIND_TOP_K,
            &self.filter_payload(),
            Self::hide_submodules(),
        );
        let _: u64 = self.issue_request(body, false, Some(epoch), step);
    }

    /// `resubmitSearch` — re-issue the current legacy flat-list Search query
    /// with a fresh epoch (the explicit Retry recovery after a terminal
    /// `Search` error; production `showRequestError`'s retry closure).
    pub(crate) fn submit_legacy_search(&mut self, query: &str, step: &mut Step) {
        query.clone_into(&mut self.search_query);
        self.reset_find_state();
        let epoch = self.search_epoch.saturating_add(1);
        self.search_epoch = epoch;
        self.current_search_epoch = Some(epoch);
        step.ops.push(DomOp::FindCounter(FindCounterState::Pending));
        step.sends
            .push(Send::StatusText(format!("Searching for \"{query}\"")));
        let body = host::search(query, FIND_TOP_K);
        let _: u64 = self.issue_request(body, false, Some(epoch), step);
    }

    /// `applyFindResponse` — settle a `FindInHistory` response in place. The
    /// live viewport is used for the first-match jump so centering and the
    /// post-jump cache/sync decisions match the real scroll position.
    pub(crate) fn apply_find_response(
        &mut self,
        response: &Value,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        let matches = response
            .get("matches")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| self.normalize_find_match(&m))
            .collect::<Vec<_>>();
        let count = matches.len();
        self.find_matches = matches;
        self.find_total = count;
        self.find_flags.more = host::row::bool(response, "more");
        self.find_flags.active = true;
        self.find_index = 0;
        self.current_search_epoch = None;
        self.pending_find_target = None;
        step.ops.push(DomOp::ClearFindHighlight);
        if count == 0 {
            step.ops.push(DomOp::FindCounter(FindCounterState::Zero));
            step.sends.push(Send::StatusText(format!(
                "No matches for \"{}\"",
                self.search_query
            )));
            step.sends.push(Send::Log(format!(
                "find: 0 match(es) for \"{}\"",
                self.search_query
            )));
            return;
        }
        step.sends.push(Send::Log(format!(
            "find: {} match(es) for \"{}\"",
            self.find_total, self.search_query
        )));
        self.jump_to_find_match(0, viewport, step);
    }

    /// `showFindError` — compact, non-disruptive find error.
    pub(crate) fn show_find_error(&mut self, err_text: &str, step: &mut Step) {
        self.reset_find_state();
        self.current_search_epoch = None;
        step.ops.push(DomOp::ClearFindHighlight);
        step.ops.push(DomOp::FindCounter(FindCounterState::Error(
            err_text.to_owned(),
        )));
        step.sends
            .push(Send::StatusText(format!("Find failed: {err_text}")));
        step.sends
            .push(Send::Log(format!("find error: {err_text}")));
    }

    /// `jumpToFindMatch` — move the viewport to match `index`.
    pub(crate) fn jump_to_find_match(
        &mut self,
        index: usize,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        if index >= self.find_matches.len() {
            return;
        }
        self.find_index = index;
        let Some(found) = self.find_matches.get(index) else {
            return;
        };
        let abs = found.row;
        step.ops.push(DomOp::FindCounter(FindCounterState::Settled {
            index,
            total: self.find_total,
            more: self.find_flags.more,
        }));
        self.pending_find_target = Some(FindTarget { abs, index });
        if self.cache.contains_key(&abs) {
            let _: Option<Viewport> = self.complete_find_jump(viewport, step);
        } else {
            self.fetch_window_around(abs, step);
        }
    }

    /// `completeFindJump` — scroll/reveal the target once cached. Returns the
    /// post-scroll viewport so the caller can evict/sync against the position
    /// the jump moved to (main.js applies `rowsEl.scrollTop` before the
    /// response handler runs `evictFarWindows`/`syncWindow`), or `None` when
    /// the jump did not complete.
    pub(crate) fn complete_find_jump(
        &mut self,
        viewport: &Viewport,
        step: &mut Step,
    ) -> Option<Viewport> {
        let target = self.pending_find_target?;
        let total = self.total.unwrap_or(0).max(0);
        if target.abs >= total {
            self.pending_find_target = None;
            self.fetch_window(viewport, step);
            return None;
        }
        if !self.cache.contains_key(&target.abs) {
            return None;
        }
        self.pending_find_target = None;
        let Some(vis) = self.visible_index_for_abs(target.abs) else {
            return None; // hidden slot — backend only targets top-level rows
        };
        let half_viewport_rows = viewport
            .client_height
            .saturating_div(ROW_H)
            .saturating_div(2);
        let target_top = vis
            .saturating_mul(ROW_H)
            .saturating_sub(half_viewport_rows.saturating_mul(ROW_H))
            .max(0);
        step.ops.push(DomOp::SetScrollTop(target_top));
        let post_scroll_viewport = Viewport {
            scroll_top: target_top,
            ..*viewport
        };
        self.sync_window(&post_scroll_viewport, step);
        step.ops.push(DomOp::SetFindHighlight { abs: target.abs });
        step.ops.push(DomOp::RevealRow { abs: target.abs });
        step.sends.push(Send::StatusText(format!(
            "Match {} of {}{}",
            target.index.saturating_add(1),
            self.find_total,
            if self.find_flags.more { "+" } else { "" }
        )));
        Some(post_scroll_viewport)
    }

    /// `navigateFind` — move the find cursor by `delta`, wrapping.
    pub(crate) fn navigate_find(&mut self, delta: isize, viewport: &Viewport, step: &mut Step) {
        if !self.find_flags.active || self.find_matches.is_empty() {
            return;
        }
        let len = self.find_matches.len();
        let moved = if delta >= 0 {
            self.find_index.saturating_add(delta.unsigned_abs())
        } else {
            self.find_index
                .saturating_add(len)
                .saturating_sub(delta.unsigned_abs())
        };
        let next = moved.rem_euclid(len);
        self.jump_to_find_match(next, viewport, step);
    }

    // --- legacy flat-list search ------------------------------------------------

    /// `normalizeSearchHit` — canonicalize a search hit into a `HistoryRow`.
    pub(crate) fn normalize_search_hit(hit: &Value, index: i64) -> Value {
        if !hit.is_object() {
            return hit.clone();
        }
        let Some(text) = hit.get("text").and_then(Value::as_str) else {
            return hit.clone();
        };
        let is_git = hit
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|s| s == "Git")
            || hit
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|k| k == "git")
            || hit.get("git_oid").is_some();
        let repository = hit.get("repository").and_then(Value::as_str).unwrap_or("");
        let session_id = hit.get("session_id").and_then(Value::as_str).unwrap_or("");
        let group = if !session_id.is_empty() {
            format!("session:{session_id}")
        } else if is_git {
            if repository.is_empty() {
                "repo:search".to_owned()
            } else {
                format!("repo:{repository}")
            }
        } else {
            "search".to_owned()
        };
        let node_key = if is_git {
            hit.get("git_oid")
                .and_then(Value::as_str)
                .map_or_else(|| format!("search:{index}"), str::to_owned)
        } else {
            hit.get("op_id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("search:{index}"), str::to_owned)
        };
        let kind = hit.get("kind").and_then(Value::as_str).map_or_else(
            || {
                if is_git {
                    "git".to_owned()
                } else {
                    "message".to_owned()
                }
            },
            str::to_owned,
        );
        json!({
            "op_id": if is_git { Value::Null } else { hit.get("op_id").cloned().unwrap_or(Value::Null) },
            "git_oid": if is_git { hit.get("git_oid").cloned().unwrap_or(Value::Null) } else { Value::Null },
            "repository": if is_git { hit.get("repository").cloned().unwrap_or(Value::Null) } else { Value::Null },
            "summary": text,
            "timestamp_ms": hit.get("timestamp_ms").and_then(Value::as_f64).unwrap_or(0.0),
            "group": group,
            "node_key": node_key,
            "parents": json!([]),
            "is_submodule": host::row::bool(hit, "is_submodule"),
            "is_system": false,
            "author": hit.get("actor_id").and_then(Value::as_str).unwrap_or(""),
            "commit_id": "",
            "kind": kind,
            "lane": 0,
            "above": json!([]),
            "below": json!([]),
            "transitions": json!([]),
            "sub_ops": json!([]),
            "is_subop": false,
        })
    }

    /// `renderSearchResults` — render search hits as a flat result list.
    pub(crate) fn render_search_results(&mut self, hits: Vec<Value>, step: &mut Step) {
        self.view_flags.search_mode = true;
        self.reset_find_state();
        step.ops.push(DomOp::FindCounter(FindCounterState::Hidden));
        let mut hits = hits;
        if Self::hide_submodules() {
            hits.retain(|h| !host::row::bool(h, "is_submodule"));
        }
        // Search replaces the view: bump the generation so stale windows drop.
        self.view_gen = self.view_gen.saturating_add(1);
        self.pending_window_req_id = None;
        self.session_flags.snapshot_established = false;
        self.session_flags.layout_ready = false;
        self.current_search_epoch = None;
        let rows: Vec<Value> = hits
            .iter()
            .enumerate()
            .map(|(i, hit)| Self::normalize_search_hit(hit, i64::try_from(i).unwrap_or(0)))
            .collect();
        self.clear_expansion_state();
        self.recompute_expansion();
        self.cache.clear();
        self.total_fetched = 0;
        self.clear_selection();
        self.total = Some(i64::try_from(rows.len()).unwrap_or(0));
        for (i, row) in rows.into_iter().enumerate() {
            drop(self.cache.insert(i64::try_from(i).unwrap_or(0), row));
        }
        self.render_top = 0;
        self.render_bottom = -1;
        step.ops.push(DomOp::SetScrollTop(0));
        let total = self.total.unwrap_or(0);
        if total == 0 {
            Self::show_view_message(
                step,
                &format!("No results for \"{}\"", self.search_query),
                false,
            );
        } else {
            step.ops.push(DomOp::Reanchor {
                top: 0,
                bottom: total.saturating_sub(1).max(0),
            });
        }
        step.sends.push(Send::StatusText(format!(
            "{total} result{} for \"{}\"",
            if total == 1 { "" } else { "s" },
            self.search_query
        )));
        self.view_flags.data_ready = true;
        step.sends
            .push(Send::Log(format!("search: {total} result(s)")));
        step.sends.push(Send::Status {
            loaded: 0,
            total: u64::try_from(self.visible_total().max(0)).unwrap_or(u64::MAX),
        });
    }

    /// `resetHistory` — reset to the full history view and reload from the top.
    pub(crate) fn reset_history(&mut self, viewport: &Viewport, step: &mut Step) {
        self.view_flags.search_mode = false;
        self.search_query.clear();
        self.current_search_epoch = None;
        self.reset_find_state();
        step.ops.push(DomOp::FindCounter(FindCounterState::Hidden));
        self.total = None; // -1 semantics
        self.view_gen = self.view_gen.saturating_add(1);
        self.session_flags.snapshot_established = false;
        self.session_flags.layout_ready = false;
        self.clear_expansion_state();
        self.recompute_expansion();
        self.pending_window_req_id = None;
        self.cache.clear();
        self.total_fetched = 0;
        self.render_top = 0;
        self.render_bottom = -1;
        step.ops.push(DomOp::SetScrollTop(0));
        self.clear_selection();
        self.roving_abs = -1;
        self.view_flags.data_ready = false;
        Self::show_view_message(step, "Loading history…", false);
        self.fetch_window(viewport, step);
    }

    // --- profile -----------------------------------------------------------------

    /// `setProfile` — switch the Activity/Raw profile. `reset` mirrors the user
    /// action (coherent reset + persist `{profile, topRow:0}`); `persist:false`
    /// mirrors the restore path used on open/reveal before the first fetch.
    pub(crate) fn set_profile(
        &mut self,
        next: Profile,
        action: ProfileAction,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        if self.profile == next && action != ProfileAction::Reset {
            return;
        }
        self.profile = next;
        match action {
            ProfileAction::Reset => {
                step.sends.push(Send::StatusText(format!(
                    "Showing {} history",
                    self.profile.label()
                )));
                step.save_state = Some(json!({
                    "profile": match self.profile { Profile::Activity => "activity", Profile::Raw => "raw" },
                    "topRow": 0,
                }));
                self.reset_history(viewport, step);
            }
            ProfileAction::Restore => {}
        }
    }

    // --- open warnings ----------------------------------------------------------

    /// `collectOpenWarnings` — user-facing chain warnings from an Open response.
    pub(crate) fn collect_open_warnings(value: &Value) -> Vec<String> {
        let mut out = Vec::new();
        if !value.is_object() {
            return out;
        }
        if let Some(Value::Array(warnings)) = value.get("warnings") {
            for warning in warnings {
                if let Some(text) = warning.as_str() {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        out.push(trimmed.to_owned());
                    }
                }
            }
        }
        if let Some(diagnostics) = value.get("diagnostics") {
            if let Some(blobs) = diagnostics.get("blobs") {
                let missing = blobs.get("missing").and_then(Value::as_i64).unwrap_or(0);
                let corrupt = blobs.get("corrupt").and_then(Value::as_i64).unwrap_or(0);
                let unresolved = blobs.get("unresolved").and_then(Value::as_i64).unwrap_or(0);
                let mut summary = Vec::new();
                if missing > 0 {
                    summary.push(format!("{missing} missing"));
                }
                if corrupt > 0 {
                    summary.push(format!("{corrupt} corrupt"));
                }
                if unresolved > 0 {
                    summary.push(format!("{unresolved} unresolved"));
                }
                let summary = summary.join(", ");
                if !summary.is_empty() {
                    let max_count = missing.max(corrupt).max(unresolved);
                    let already = out.iter().any(|w| w.contains(&max_count.to_string()));
                    if !already {
                        out.push(format!(
                            "Chain data integrity: {summary} blob payload(s) in the durable store"
                        ));
                    }
                }
            }
        }
        out
    }
}

impl HistoryAppState {
    // --- messages ----------------------------------------------------------------

    /// Announce a status change to assistive tech / the status bar.
    fn announce(text: &str, step: &mut Step) {
        step.sends.push(Send::StatusText(text.to_owned()));
    }

    /// `showViewMessage` — full-pane loading/open message.
    pub(crate) fn show_view_message(step: &mut Step, text: &str, error: bool) {
        step.ops.push(DomOp::ShowMessage {
            text: text.to_owned(),
            error,
        });
    }

    /// `showRequestError` — terminal GetWindow/Search failure with Retry. The
    /// error pane is the settled terminal UI, so readiness flips on (main.js
    /// sets `window.__editchainDataReady = true` in `showRequestError`).
    pub(crate) fn show_request_error(&mut self, step: &mut Step, text: &str, retry: RetryAction) {
        self.view_flags.data_ready = true;
        step.ops.push(DomOp::ShowRequestError {
            text: text.to_owned(),
            retry,
        });
        step.sends.push(Send::Log(
            "request error shown; loader suspended until explicit recovery".to_owned(),
        ));
    }

    /// `reportStatus` — host status-bar counts from the live viewport.
    pub(crate) fn report_status(&self, viewport: &Viewport, step: &mut Step) {
        step.sends.push(Send::Status {
            loaded: u64::try_from(Self::viewport_visible_top(viewport).max(0)).unwrap_or(u64::MAX),
            total: u64::try_from(self.visible_total().max(0)).unwrap_or(u64::MAX),
        });
    }

    /// Handle one host message and produce the transition step. `viewport` is
    /// the live measurement the shell captured for this tick.
    pub(crate) fn handle_host_message(
        &mut self,
        msg: HostMessage,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        match msg.id {
            Id::Open => self.handle_open(msg.body, viewport, step),
            Id::Reveal => self.handle_reveal(viewport, step),
            Id::Ready | Id::Unknown(_) => {} // compatibility handshake / unknown ids
            Id::Request(id) => self.handle_response(id, msg.body, viewport, step),
        }
    }

    fn handle_open(&mut self, body: Option<Value>, viewport: &Viewport, step: &mut Step) {
        let unwrapped = host::unwrap(body);
        match unwrapped {
            Unwrapped::Ok(value) if value.is_null() => {
                // Explicit loading signal: keep the loading message on screen.
                Self::show_view_message(step, "Loading history…", false);
            }
            Unwrapped::Ok(value) if value.is_object() => {
                self.view_flags.data_ready = false;
                self.view_flags.search_mode = false;
                self.search_query.clear();
                self.reset_find_state();
                self.clear_selection();
                self.roving_abs = -1;
                step.ops.push(DomOp::FindCounter(FindCounterState::Hidden));
                // A fresh chain is a new view generation; drop stale responses
                // and re-establish the offset-0 snapshot from scratch.
                self.view_gen = self.view_gen.saturating_add(1);
                self.in_flight.clear();
                self.cache.clear();
                self.total_fetched = 0;
                self.pending_window_req_id = None;
                self.session_flags.layout_ready = false;
                self.current_search_epoch = None;
                self.session_flags.snapshot_established = false;
                self.clear_expansion_state();
                self.recompute_expansion();
                self.open_warnings = Self::collect_open_warnings(&value);
                if !self.open_warnings.is_empty() {
                    step.sends.push(Send::Log(format!(
                        "open warnings: {}",
                        self.open_warnings.join(" | ")
                    )));
                }
                let nodes = value
                    .get("nodes")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .max(0);
                let repos = value.get("repos").and_then(Value::as_i64).unwrap_or(0);
                step.sends
                    .push(Send::Log(format!("open: {nodes} nodes, {repos} repos")));
                self.total = Some(nodes);
                if nodes == 0 {
                    self.view_flags.data_ready = true;
                    Self::show_view_message(step, "No history found in this workspace", false);
                    return;
                }
                let (top_row, restored_profile) = self.restore_state();
                // Apply the persisted profile BEFORE the first fetch so the
                // offset-0 window carries the right hide_trace flag.
                self.set_profile(restored_profile, ProfileAction::Restore, viewport, step);
                self.fetch_window(viewport, step);
                let (want_top, want_bottom) = self.desired_visible_range(viewport);
                self.render_top = want_top;
                self.render_bottom = want_bottom;
                step.ops.push(DomOp::Reanchor {
                    top: want_top,
                    bottom: want_bottom,
                });
                if top_row > 0 {
                    step.ops
                        .push(DomOp::RestoreScrollTop { row_index: top_row });
                } else {
                    step.ops.push(DomOp::SetScrollTop(0));
                }
                // Persist the actually-restored viewport (the ONLY open-path
                // save; any earlier save would write scrollTop 0).
                step.save_state = Some(self.persisted_state(viewport));
                step.ops.push(DomOp::ProgressiveLoader(true));
                self.report_status(viewport, step);
            }
            Unwrapped::Ok(value) => {
                let err_text = String::from("unknown error");
                step.sends
                    .push(Send::Log(format!("open error: {err_text}")));
                self.view_flags.data_ready = true;
                Self::show_view_message(step, &format!("Failed to open history: {err_text}"), true);
                drop(value);
            }
            Unwrapped::Err(err_text) => {
                step.sends
                    .push(Send::Log(format!("open error: {err_text}")));
                self.view_flags.data_ready = true;
                Self::show_view_message(step, &format!("Failed to open history: {err_text}"), true);
            }
        }
    }

    fn handle_reveal(&mut self, viewport: &Viewport, step: &mut Step) {
        let (top_row, restored_profile) = self.restore_state();
        self.set_profile(restored_profile, ProfileAction::Restore, viewport, step);
        self.view_gen = self.view_gen.saturating_add(1);
        self.session_flags.snapshot_established = false;
        self.clear_expansion_state();
        self.recompute_expansion();
        step.sends.push(Send::Log(format!(
            "reveal: topRow={top_row} total={}",
            self.total.unwrap_or(-1)
        )));
        // main.js defers this block 50ms so the scaffold exists first; the
        // shell may apply the ops after the DOM settle the same way.
        self.fetch_window(viewport, step);
        let (want_top, want_bottom) = self.desired_visible_range(viewport);
        self.render_top = want_top;
        self.render_bottom = want_bottom;
        step.ops.push(DomOp::Reanchor {
            top: want_top,
            bottom: want_bottom,
        });
        if top_row > 0 {
            step.ops
                .push(DomOp::RestoreScrollTop { row_index: top_row });
        } else {
            step.ops.push(DomOp::SetScrollTop(0));
        }
        step.save_state = Some(self.persisted_state(viewport));
        step.ops.push(DomOp::ProgressiveLoader(true));
    }

    fn handle_response(
        &mut self,
        id: u64,
        body: Option<Value>,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        let Some(req) = self.in_flight.remove(&id) else {
            return; // unknown id (e.g. a replayed open response replay) — drop
        };
        let was_pending_window = self.pending_window_req_id == Some(id);
        if was_pending_window {
            self.pending_window_req_id = None;
        }
        // Stale-view rejection: a response issued under an older view
        // generation must never poison the new view. Self-heal by re-requesting
        // the current view's window when the stale response was the pending one.
        if req.gen_tag != self.view_gen {
            if was_pending_window {
                self.fetch_window(viewport, step);
            }
            return;
        }
        // Latest-query-wins for search responses (epoch correlation).
        if let Some(epoch) = req.search_epoch {
            if self.current_search_epoch != Some(epoch) {
                step.sends.push(Send::Log(format!(
                    "dropping stale search response (epoch {epoch} of {})",
                    self.current_search_epoch
                        .map_or_else(|| "none".to_owned(), |e| e.to_string())
                )));
                return;
            }
        }
        let unwrapped = host::unwrap(body);
        match unwrapped {
            Unwrapped::Err(err_text) => {
                step.sends
                    .push(Send::Log(format!("request error: {err_text}")));
                if was_pending_window || req.body.get("GetWindow").is_some() {
                    self.show_request_error(
                        step,
                        &format!("Failed to load history rows: {err_text}"),
                        RetryAction::ResetHistory,
                    );
                    return;
                }
                if req.body.get("Search").is_some() {
                    self.show_request_error(
                        step,
                        &format!("Search failed: {err_text}"),
                        RetryAction::ResubmitSearch,
                    );
                    return;
                }
                if req.body.get("FindInHistory").is_some() {
                    self.show_find_error(&err_text, step);
                }
            }
            Unwrapped::Ok(value) if value.is_object() => {
                // FindInHistory response — in-place find, never replaces the view.
                if value.get("matches").and_then(Value::as_array).is_some() {
                    self.apply_find_response(&value, viewport, step);
                    return;
                }
                // Legacy Search response — bare array or { results: [...] }.
                let search_hits = if let Some(hits) = value.as_array() {
                    Some(hits.clone())
                } else {
                    value.get("results").and_then(Value::as_array).cloned()
                };
                if let Some(hits) = search_hits {
                    self.render_search_results(hits, step);
                    return;
                }
                if value.get("rows").and_then(Value::as_array).is_some() {
                    self.handle_window_response(&value, &req.body, viewport, step);
                }
            }
            Unwrapped::Ok(_) => {}
        }
    }

    /// `GetWindow` response handling — the production two-pass hydration flow
    /// (rows first, then the layout geometry pass for the same page).
    fn handle_window_response(
        &mut self,
        value: &Value,
        req_body: &Value,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        let response_layout_ready = value
            .get("layout_ready")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if response_layout_ready {
            self.session_flags.layout_ready = true;
        }
        if let Some(total) = value.get("total").and_then(Value::as_i64) {
            self.total = Some(total.max(0));
        }
        if response_layout_ready {
            if let Some(max_lane) = value.get("max_lane").and_then(Value::as_i64) {
                if let Ok(max_lane) = u32::try_from(max_lane.max(0)) {
                    if max_lane != self.max_lane {
                        self.max_lane = max_lane;
                        step.ops.push(DomOp::RefreshHeader);
                    }
                }
            }
        }
        // Expansion indices ship only with the offset-0 window. Rebuild the
        // depth-first visibility map and re-anchor (the identity-mapped first
        // paint is now stale). `sub_op_counts` remains the compatibility path
        // for one-level services; current services also send nested spans.
        if let Some(counts) = value.get("sub_op_counts").and_then(Value::as_array) {
            self.sub_op_counts = counts
                .iter()
                .map(|c| c.as_i64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0))
                .collect();
            self.expansion_spans.clear();
            if let Some(spans) = value.get("expansion_spans").and_then(Value::as_array) {
                for span in spans {
                    let row = span.get("row").and_then(Value::as_i64);
                    let descendants = span
                        .get("descendant_count")
                        .and_then(Value::as_i64)
                        .and_then(|count| u32::try_from(count).ok());
                    if let (Some(row), Some(descendants)) = (row, descendants) {
                        if row >= 0 && descendants > 0 {
                            let _: Option<u32> = self.expansion_spans.insert(row, descendants);
                        }
                    }
                }
            }
            self.session_flags.snapshot_established = true;
            self.recompute_expansion();
            step.ops.push(DomOp::Reanchor {
                top: self.render_top,
                bottom: self.render_bottom,
            });
        }
        let base = req_body
            .get("GetWindow")
            .and_then(|w| w.get("offset"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(rows) = value.get("rows").and_then(Value::as_array) {
            for (i, row) in rows.iter().enumerate() {
                let abs = base.saturating_add(i64::try_from(i).unwrap_or(0));
                if !self.cache.contains_key(&abs) {
                    self.total_fetched = self.total_fetched.saturating_add(1);
                }
                drop(self.cache.insert(abs, row.clone()));
            }
        }
        // A find jump target's window has arrived: move the viewport to it
        // BEFORE eviction (eviction keys off the current viewport). The
        // post-scroll viewport becomes the effective viewport for the rest of
        // this response, mirroring main.js where `rowsEl.scrollTop` is applied
        // before `evictFarWindows()`/`syncWindow()`.
        let mut effective_viewport = *viewport;
        if let Some(target) = self.pending_find_target {
            if self.cache.contains_key(&target.abs) {
                if let Some(post_scroll) = self.complete_find_jump(viewport, step) {
                    effective_viewport = post_scroll;
                }
            }
        }
        if response_layout_ready {
            let include_layout = req_body
                .get("GetWindow")
                .and_then(|w| w.get("include_layout"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if include_layout {
                step.ops.push(DomOp::Reanchor {
                    top: self.render_top,
                    bottom: self.render_bottom,
                });
            }
        }
        self.evict_far_windows(&effective_viewport);
        self.sync_window(&effective_viewport, step);
        self.view_flags.data_ready = true;
        let total = self.total.unwrap_or(0);
        if !self.session_flags.announced_initial_load && total > 0 {
            self.session_flags.announced_initial_load = true;
            Self::announce(
                &format!("Loaded {} history rows", self.visible_total()),
                step,
            );
        }
        if self.cache.is_empty() && total == 0 {
            Self::show_view_message(step, "No history rows", false);
        }
        step.sends.push(Send::Log(format!(
            "cached {}/{} nodes (fetched {})",
            self.cache.len(),
            total,
            self.total_fetched
        )));
        self.report_status(viewport, step);
        step.save_state = Some(self.persisted_state(viewport));
        if !response_layout_ready {
            let include_layout = req_body
                .get("GetWindow")
                .and_then(|w| w.get("include_layout"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !include_layout {
                let Some(window) = req_body.get("GetWindow") else {
                    return;
                };
                let body = get_window(
                    window.get("offset").and_then(Value::as_i64).unwrap_or(0),
                    window.get("limit").and_then(Value::as_i64).unwrap_or(0),
                    &Self::filter_payload_from(window),
                    true,
                );
                let _: u64 = self.issue_request(body, true, None, step);
                return;
            }
        }
        if self.pending_find_target.is_some() {
            if let Some(target) = self.pending_find_target {
                self.fetch_window_around(target.abs, step);
            }
        } else {
            self.fetch_window(&effective_viewport, step);
        }
    }

    /// Rebuild the chain filter from a request window's own fields when the
    /// response asks for an exact re-issue of the same page.
    fn filter_payload_from(window: &Value) -> ChainFilter {
        let filter = window.get("filter");
        ChainFilter {
            summary_pattern: filter
                .and_then(|f| f.get("summary_pattern"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            kind_pattern: filter
                .and_then(|f| f.get("kind_pattern"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            include_kind_pattern: filter
                .and_then(|f| f.get("include_kind_pattern"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            hide_undated: filter
                .and_then(|f| f.get("hide_undated"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            splice: filter
                .and_then(|f| f.get("splice"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            hide_trace: filter
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vp() -> Viewport {
        Viewport::new(0, 800)
    }

    fn open_msg(nodes: i64) -> HostMessage {
        HostMessage::parse(
            &json!({ "id": "open", "body": { "Ok": { "nodes": nodes, "repos": 1 } } }),
        )
        .unwrap()
    }

    fn resp(id: u64, body: &Value) -> HostMessage {
        HostMessage::parse(&json!({ "id": id, "body": body })).unwrap()
    }

    fn row(index: i64, node_key: &str, lane: u32) -> Value {
        json!({
            "index": index,
            "node_key": node_key,
            "summary": format!("summary {node_key} **bold** tail"),
            "timestamp_ms": 1_700_000_000_000_i64,
            "group": "session:s1",
            "parents": [],
            "is_submodule": false,
            "is_system": true,
            "author": "human",
            "commit_id": "",
            "kind": "message",
            "lane": lane,
            "above": [],
            "below": [],
            "transitions": [],
            "sub_ops": [],
            "is_subop": false,
        })
    }

    fn window_response(
        offset: i64,
        count: i64,
        total: i64,
        layout: Option<(u32, Option<&Vec<u32>>)>,
    ) -> Value {
        let rows: Vec<Value> = (0..count)
            .map(|i| {
                row(
                    offset.saturating_add(i),
                    &format!("node:{}", offset.saturating_add(i)),
                    u32::try_from(i.wrapping_rem(3)).unwrap_or(0),
                )
            })
            .collect();
        let (include_layout, max_lane, sub_op_counts) = match layout {
            Some((max_lane, sub_op_counts)) => (true, max_lane, sub_op_counts),
            None => (false, 2, None),
        };
        json!({
            "Ok": {
                "rows": rows,
                "total": total,
                "chain_generation": 0,
                "max_lane": max_lane,
                "layout_ready": include_layout,
                "sub_op_counts": sub_op_counts,
            }
        })
    }

    fn last_get_window(state: &HistoryAppState) -> &serde_json::Map<String, Value> {
        state
            .request_log
            .iter()
            .rev()
            .find(|req| req.body.get("GetWindow").is_some())
            .expect("request log has a GetWindow")
            .body
            .get("GetWindow")
            .expect("envelope has GetWindow")
            .as_object()
            .expect("GetWindow is an object")
    }

    #[test]
    fn open_establishes_view_and_issues_first_window() {
        let mut state = HistoryAppState::default();
        let mut step = Step::new();
        state.handle_host_message(open_msg(1200), &vp(), &mut step);

        assert_eq!(state.total, Some(1200));
        assert_eq!(state.profile, Profile::Activity);
        assert!(!state.view_flags.data_ready);
        // The scaffold is re-anchored immediately (placeholders fill in later):
        // visible bottom (row 23 at 800px / ROW_H 34) + BUFFER 400 = 423,
        // matching main.js `desiredVisibleRange()` at open.
        assert_eq!(state.render_top, 0);
        assert_eq!(state.render_bottom, 423);
        // First fetch under the pre-layout view: offset 0, limited to the
        // desired cache range (visible bottom 23 + BUFFER 400 = 423 rows, so
        // limit 424 under a clientHeight of 800), with the Activity
        // hide_trace + hide_undated flags and layout disabled.
        assert_eq!(state.request_log.len(), 1);
        let window = last_get_window(&state);
        assert_eq!(window["offset"], 0);
        assert_eq!(window["limit"], 424);
        assert_eq!(window["include_layout"], false);
        assert_eq!(window["hide_submodules"], true);
        assert_eq!(
            window
                .get("filter")
                .and_then(|f| f.get("hide_undated"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            window
                .get("filter")
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            window
                .get("filter")
                .and_then(|f| f.get("splice"))
                .and_then(Value::as_bool),
            Some(true)
        );
        // The pending-window slot is claimed BEFORE the send is emitted so a
        // synchronous fixture response correlates (reentrancy contract).
        assert_eq!(state.pending_window_req_id, Some(1));
        assert!(step
            .sends
            .contains(&Send::Log("open: 1200 nodes, 1 repos".to_owned())));
        assert!(step.ops.iter().any(|op| matches!(
            op,
            DomOp::Reanchor {
                top: 0,
                bottom: 423
            }
        )));
        assert!(step
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::ProgressiveLoader(true))));
        // The open-path save persists the actually-restored viewport.
        let saved = step.save_state.expect("open persists state");
        assert_eq!(
            saved.get("profile").and_then(Value::as_str),
            Some("activity")
        );
        assert_eq!(saved.get("topRow").and_then(Value::as_i64), Some(0));
    }

    #[test]
    fn first_window_flow_does_the_two_pass_layout_hydration() {
        let mut state = HistoryAppState::default();
        let mut step = Step::new();
        state.handle_host_message(open_msg(1200), &vp(), &mut step);
        let pending = state.pending_window_req_id.expect("window in flight");

        // Pass 1: rows only (no layout), offset 0.
        let mut step2 = Step::new();
        state.handle_host_message(
            resp(pending, &window_response(0, 500, 1200, None)),
            &vp(),
            &mut step2,
        );
        assert_eq!(
            state.pending_window_req_id,
            Some(2),
            "the pending slot is synchronously re-claimed by the layout hydration re-issue"
        );
        assert_eq!(state.cache.len(), 500, "rows land at the requested offset");
        assert!(state.cache.contains_key(&499));
        assert!(!state.cache.contains_key(&500));
        assert!(!state.session_flags.layout_ready);
        // ...and the exact same page is re-issued with include_layout=true.
        let hydration = state.request_log.last().expect("layout hydration issued");
        assert_eq!(
            hydration
                .body
                .get("GetWindow")
                .and_then(|w| w.get("include_layout"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            hydration
                .body
                .get("GetWindow")
                .and_then(|w| w.get("offset"))
                .and_then(Value::as_i64),
            Some(0)
        );
        assert_eq!(
            hydration
                .body
                .get("GetWindow")
                .and_then(|w| w.get("limit"))
                .and_then(Value::as_i64),
            Some(424)
        );

        // Pass 2: layout hydration for the same page; maxLane bumps a header
        // refresh op and the offset-0 snapshot re-anchors the stale identity
        // paint; the window continues paging forward under layout.
        let pending = state.pending_window_req_id.expect("hydration in flight");
        let mut step3 = Step::new();
        state.handle_host_message(
            resp(
                pending,
                &window_response(0, 500, 1200, Some((5, Some(&vec![0; 1200])))),
            ),
            &vp(),
            &mut step3,
        );
        assert!(state.session_flags.snapshot_established);
        assert!(state.session_flags.layout_ready);
        assert_eq!(state.max_lane, 5);
        assert!(step3
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::RefreshHeader)));
        assert!(step3
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::Reanchor { .. })));
        assert!(
            state.view_flags.data_ready,
            "readiness is owned by the authoritative ViewFlags flag"
        );
        assert_eq!(state.cache.len(), 500);
        // The desired cache range (viewport 0..23 + BUFFER 400) is fully
        // cached after this page, so no further window request is issued; the
        // last request log entry stays the layout hydration of the same page.
        let next = last_get_window(&state);
        assert_eq!(next["offset"], 0);
        assert_eq!(next["limit"], 424);
        assert_eq!(next["include_layout"], true);
        assert_eq!(
            next.get("filter")
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(step3.sends.iter().any(|s| matches!(
            s,
            Send::Log(text) if text.starts_with("cached 500/1200 nodes (fetched 500)")
        )));
    }

    #[test]
    fn stale_window_response_is_dropped_and_self_heals() {
        let mut state = HistoryAppState::default();
        let mut step = Step::new();
        state.handle_host_message(open_msg(1200), &vp(), &mut step);
        let stale_id = state.pending_window_req_id.expect("window in flight");

        // A `reveal` (recreated webview context) starts a new view generation
        // WITHOUT clearing the in-flight window or the pending slot.
        state.handle_host_message(
            HostMessage::parse(&json!({ "id": "reveal" })).unwrap(),
            &vp(),
            &mut step,
        );
        assert_eq!(
            state.view_gen, 2,
            "open and reveal each start a new view generation"
        );
        assert_eq!(
            state.request_log.len(),
            1,
            "reveal reuses the pending window"
        );

        let mut step2 = Step::new();
        state.handle_host_message(
            resp(stale_id, &window_response(0, 500, 1200, None)),
            &vp(),
            &mut step2,
        );
        assert!(
            state.cache.is_empty(),
            "stale-generation rows never poison the cache"
        );
        // Because the stale response owned the pending-window slot, the current
        // view immediately re-requests its window (self-heal without waiting
        // for the progressive loader timer).
        assert_eq!(
            state.request_log.len(),
            2,
            "self-heal re-issues the current view's window"
        );
        assert_eq!(state.total_fetched, 0);
    }

    #[test]
    fn open_clears_in_flight_so_old_responses_drop_silently() {
        let mut state = HistoryAppState::default();
        let mut step = Step::new();
        state.handle_host_message(open_msg(1200), &vp(), &mut step);
        let old_id = state.pending_window_req_id.expect("window in flight");
        // A second open clears the request table entirely (authoritative view).
        state.handle_host_message(open_msg(1200), &vp(), &mut step);
        assert_eq!(state.request_log.len(), 2);
        assert_eq!(state.pending_window_req_id, Some(2));

        let mut step2 = Step::new();
        state.handle_host_message(
            resp(old_id, &window_response(0, 500, 1200, None)),
            &vp(),
            &mut step2,
        );
        assert!(state.cache.is_empty());
        assert_eq!(
            state.request_log.len(),
            2,
            "unknown ids are dropped silently"
        );
        assert_eq!(
            state.pending_window_req_id,
            Some(2),
            "the current pending window survives"
        );
    }

    #[test]
    fn stale_search_epoch_response_is_dropped() {
        let mut state = HistoryAppState::default();
        let mut step = Step::new();
        state.handle_host_message(open_msg(1200), &vp(), &mut step);
        state.session_flags.snapshot_established = true; // skip further window fetches

        let mut step2 = Step::new();
        state.submit_find("needle", &mut step2);
        let older_id = state.request_log.last().expect("find request issued").id;
        assert_eq!(state.current_search_epoch, Some(1));
        assert_eq!(
            state.current_search_epoch(),
            Some(1),
            "accessor mirrors the epoch"
        );
        // The pending query renders the searching counter and posts the exact
        // production FindInHistory envelope (candidate cap, chain filter).
        assert!(step2
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::FindCounter(FindCounterState::Pending))));
        let find_env = state
            .request_log
            .last()
            .expect("find request issued")
            .body
            .get("FindInHistory")
            .expect("FindInHistory envelope");
        assert_eq!(
            find_env.get("query").and_then(Value::as_str),
            Some("needle")
        );
        assert_eq!(
            find_env.get("top_k").and_then(Value::as_i64),
            Some(FIND_TOP_K)
        );
        assert_eq!(
            state.in_flight.get(&older_id).and_then(|f| f.search_epoch),
            Some(1),
            "the in-flight entry carries the epoch for correlation"
        );
        // A newer query supersedes epoch 1 before its response lands.
        state.submit_find("newer", &mut step2);
        let current_id = state
            .request_log
            .last()
            .expect("newer find request issued")
            .id;
        assert_eq!(state.current_search_epoch, Some(2));

        // The older epoch's response must be dropped (latest-query-wins).
        let mut step3 = Step::new();
        state.handle_host_message(
            resp(
                older_id,
                &json!({ "Ok": { "matches": [{ "row": 9, "node_key": "node:9", "summary": "old" }], "returned": 1, "more": false } }),
            ),
            &vp(),
            &mut step3,
        );
        assert!(state.find_matches.is_empty(), "stale find response dropped");
        assert!(step3.sends.iter().any(|s| matches!(
            s,
            Send::Log(text) if text.contains("dropping stale search response (epoch 1 of 2)")
        )));

        // The current epoch's response lands normally.
        let mut step4 = Step::new();
        state.handle_host_message(
            resp(
                current_id,
                &json!({ "Ok": { "matches": [{ "row": 4, "node_key": "node:4", "summary": "hit" }], "returned": 1, "more": false } }),
            ),
            &vp(),
            &mut step4,
        );
        assert_eq!(state.find_matches.len(), 1);
        assert_eq!(state.find_matches.first().map(|m| m.row), Some(4));
    }

    #[test]
    fn find_jump_fetches_the_target_window_around_offcache_rows_then_completes() {
        // A live viewport: the shell's real scroll is deep in the chain and the
        // client height is 400px (not the 800px test default), so every
        // centering/cache/sync decision must key off THIS viewport.
        let live_viewport = Viewport::new(12_000, 400);
        let mut state = HistoryAppState {
            total: Some(5000),
            session_flags: SessionFlags {
                layout_ready: true,
                snapshot_established: true, // identity mapping, no sub-op blocks
                ..SessionFlags::default()
            },
            ..Default::default()
        };
        state.recompute_expansion();
        for i in 0..500i64 {
            drop(state.cache.insert(i, row(i, &format!("node:{i}"), 1)));
        }

        let mut step = Step::new();
        state.submit_find("needle", &mut step);
        let find_id = state.request_log.last().expect("find request issued").id;
        let mut step2 = Step::new();
        state.handle_host_message(
            resp(find_id, &json!({ "Ok": { "matches": [{ "row": 2500, "node_key": "node:2500", "summary": "needle hit" }], "returned": 1, "more": true } })),
            &live_viewport,
            &mut step2,
        );
        assert_eq!(state.find_index, 0);
        assert!(state.find_flags.more);
        assert_eq!(state.find_total, 1);
        assert_eq!(
            state.find_matches.first().map(|m| m.node_key.as_str()),
            Some("node:2500"),
            "match normalization keeps the stable node key"
        );
        assert_eq!(
            state.find_matches.first().map(|m| m.summary.as_str()),
            Some("needle hit")
        );
        // Off-cache target triggers fetchWindowAround(2500) — a sparse jump,
        // not a linear scan from offset 0.
        assert_eq!(
            state.pending_find_target,
            Some(FindTarget {
                abs: 2500,
                index: 0
            })
        );
        assert_eq!(
            state
                .request_log
                .last()
                .expect("jump window issued")
                .body
                .get("GetWindow")
                .and_then(|w| w.get("offset"))
                .and_then(Value::as_i64),
            Some(2100)
        );

        // The jump's window arrives with rows at absolute offsets 2100..2599.
        let jump_id = state.pending_window_req_id.expect("jump window in flight");
        let rows: Vec<Value> = (2100..2600)
            .map(|i| row(i, &format!("node:{i}"), 1))
            .collect();
        let mut step3 = Step::new();
        state.handle_host_message(
            resp(
                jump_id,
                &json!({ "Ok": { "rows": rows, "total": 5000, "chain_generation": 0, "max_lane": 2, "layout_ready": true, "sub_op_counts": null } }),
            ),
            &live_viewport,
            &mut step3,
        );
        assert!(state.cache.contains_key(&2500), "target row is cached");
        assert_eq!(
            state.pending_find_target, None,
            "jump completes once cached"
        );
        // targetTop centers the visible row in the LIVE viewport: the half-
        // viewport offset derives from client_height 400, not the 800px test
        // default (which would produce a different scroll top).
        let half = 400i64
            .saturating_div(ROW_H)
            .saturating_div(2)
            .saturating_mul(ROW_H);
        let target_top = 2500i64.saturating_mul(ROW_H).saturating_sub(half).max(0);
        assert_eq!(target_top, 84_830);
        assert!(step3
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::SetScrollTop(t) if *t == target_top)));
        assert!(step3
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::SetFindHighlight { abs: 2500 })));
        assert!(step3
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::RevealRow { abs: 2500 })));
        assert!(step3
            .sends
            .iter()
            .any(|s| matches!(s, Send::StatusText(text) if text == "Match 1 of 1+")));
        // The jump's own response window is followed by a page request whose
        // range is derived from the POST-SCROLL viewport (the jump applied
        // scrollTop 84830 first): 2095 = 2500 - BUFFER in visible space, NOT
        // the original scroll-0 viewport's range (which would fetch nothing).
        let follow_up = last_get_window(&state);
        assert_eq!(
            follow_up.get("offset").and_then(Value::as_i64),
            Some(2095),
            "post-jump paging uses the post-scroll viewport"
        );
        assert_eq!(
            follow_up.get("limit").and_then(Value::as_i64),
            Some(500),
            "the post-jump page is capped at PAGE"
        );
        assert_eq!(state.render_top, 2095);
        assert_eq!(state.render_bottom, 2906);
    }

    #[test]
    fn find_jump_centers_using_the_live_viewport_when_cached() {
        // An on-cache match completes the jump inside the response handler;
        // the centering math must use the real client height (400 here), not a
        // hardcoded 800px viewport.
        let live_viewport = Viewport::new(0, 400);
        let mut state = HistoryAppState {
            total: Some(500),
            session_flags: SessionFlags {
                snapshot_established: true, // identity mapping
                ..SessionFlags::default()
            },
            ..Default::default()
        };
        state.recompute_expansion();
        for i in 0..500i64 {
            drop(state.cache.insert(i, row(i, &format!("node:{i}"), 1)));
        }

        let mut step = Step::new();
        state.submit_find("needle", &mut step);
        let find_id = state.request_log.last().expect("find request issued").id;
        let mut step2 = Step::new();
        state.handle_host_message(
            resp(
                find_id,
                &json!({ "Ok": { "matches": [{ "row": 250, "node_key": "node:250", "summary": "hit" }], "returned": 1, "more": false } }),
            ),
            &live_viewport,
            &mut step2,
        );
        let half = 400i64
            .saturating_div(ROW_H)
            .saturating_div(2)
            .saturating_mul(ROW_H);
        let target_top = 250i64.saturating_mul(ROW_H).saturating_sub(half).max(0);
        assert_eq!(target_top, 8_330);
        assert!(step2
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::SetScrollTop(t) if *t == target_top)));
        assert!(step2
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::SetFindHighlight { abs: 250 })));
        assert!(step2
            .sends
            .iter()
            .any(|s| matches!(s, Send::StatusText(text) if text == "Match 1 of 1")));
        assert_eq!(state.pending_find_target, None, "jump completed in place");
        // The fully-cached post-scroll range issues no extra window request.
        assert_eq!(
            state.request_log.len(),
            1,
            "no fetch after an on-cache jump"
        );
        assert_eq!(state.pending_window_req_id, None);
    }

    #[test]
    fn find_navigation_wraps_and_counter_text_matches_production() {
        let mut state = HistoryAppState {
            total: Some(10),
            session_flags: SessionFlags {
                snapshot_established: true,
                ..SessionFlags::default()
            },
            ..Default::default()
        };
        state.recompute_expansion();
        for i in 0..10i64 {
            drop(state.cache.insert(i, row(i, &format!("node:{i}"), 1)));
        }
        let mut step = Step::new();
        state.submit_find("needle", &mut step);
        let find_id = state.request_log.last().expect("find request").id;
        let mut step2 = Step::new();
        state.handle_host_message(
            resp(find_id, &json!({ "Ok": { "matches": [{ "row": 0 }, { "row": 5 }, { "row": 9 }], "returned": 3, "more": false } })),
            &vp(),
            &mut step2,
        );
        assert_eq!(state.find_total, 3);
        assert!(state.find_active(), "settled session is active");
        assert_eq!(state.find_index(), 0);
        assert_eq!(state.find_total(), 3);
        assert!(!state.find_more(), "returned all candidates");
        assert_eq!(state.current_find_match().map(|m| m.row), Some(0));
        assert_eq!(
            HistoryAppState::find_counter_text(&FindCounterState::Settled {
                index: 0,
                total: 3,
                more: false
            }),
            "1 of 3"
        );

        state.navigate_find(1, &vp(), &mut step2);
        assert_eq!(state.find_index, 1);
        assert_eq!(state.find_index(), 1);
        assert_eq!(state.current_find_match().map(|m| m.row), Some(5));
        assert_eq!(
            state.find_matches.get(state.find_index).map(|m| m.row),
            Some(5)
        );
        state.navigate_find(1, &vp(), &mut step2);
        assert_eq!(state.find_index, 2);
        assert_eq!(
            state.find_matches.get(state.find_index).map(|m| m.row),
            Some(9)
        );
        state.navigate_find(1, &vp(), &mut step2);
        assert_eq!(state.find_index, 0, "next wraps to the first match");
        state.navigate_find(-1, &vp(), &mut step2);
        assert_eq!(state.find_index, 2, "previous wraps to the last match");
        // more=true appends the truncation marker to the counter text.
        assert_eq!(
            HistoryAppState::find_counter_text(&FindCounterState::Settled {
                index: 0,
                total: 3,
                more: true
            }),
            "1 of 3+"
        );
        assert_eq!(
            HistoryAppState::find_counter_text(&FindCounterState::Zero),
            "0 of 0"
        );
        assert_eq!(
            HistoryAppState::find_counter_text(&FindCounterState::Error("boom".to_owned())),
            "error"
        );
    }

    #[test]
    fn find_zero_matches_and_compact_error_keep_the_chain_visible() {
        let mut state = HistoryAppState {
            total: Some(10),
            ..Default::default()
        };
        let mut step = Step::new();
        state.submit_find("absent term", &mut step);
        let find_id = state.request_log.last().expect("find request").id;
        let mut step2 = Step::new();
        state.handle_host_message(
            resp(
                find_id,
                &json!({ "Ok": { "matches": [], "returned": 0, "more": false } }),
            ),
            &vp(),
            &mut step2,
        );
        assert!(state.find_flags.active && state.find_matches.is_empty());
        assert!(step2
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::FindCounter(FindCounterState::Zero))));
        assert!(step2.sends.iter().any(|s| matches!(
            s,
            Send::StatusText(text) if text == "No matches for \"absent term\""
        )));

        let mut step3 = Step::new();
        state.show_find_error("service hiccup", &mut step3);
        assert!(!state.find_flags.active);
        assert!(step3.ops.iter().any(|op| matches!(
            op,
            DomOp::FindCounter(FindCounterState::Error(e)) if e == "service hiccup"
        )));
        assert!(step3
            .sends
            .iter()
            .any(|s| matches!(s, Send::StatusText(text) if text == "Find failed: service hiccup")));
    }

    #[test]
    fn profile_switch_resets_coherently_and_keeps_undated_rows_hidden() {
        let mut state = HistoryAppState::default();
        let mut step = Step::new();
        state.handle_host_message(open_msg(500), &vp(), &mut step);
        assert_eq!(
            last_get_window(&state)
                .get("filter")
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool),
            Some(true)
        );
        let gen_activity = state.view_gen;

        let mut step2 = Step::new();
        state.set_profile(Profile::Raw, ProfileAction::Reset, &vp(), &mut step2);
        assert_eq!(state.profile, Profile::Raw);
        assert_eq!(
            last_get_window(&state)
                .get("filter")
                .and_then(|f| f.get("hide_undated"))
                .and_then(Value::as_bool),
            Some(true),
            "Raw presentation still omits timestamp-zero rows"
        );
        assert_eq!(
            last_get_window(&state)
                .get("filter")
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool),
            Some(false)
        );
        // total resets to unknown so the fresh view re-requests offset 0 under
        // the new profile; the view generation bumps so stale windows drop.
        assert_eq!(state.total, None);
        assert_eq!(state.view_gen, gen_activity + 1);
        assert!(state.cache.is_empty(), "the old profile's cache is dropped");
        // main.js `resetHistory` keeps stale in-flight entries correlated (a
        // late response is rejected by the view-generation check) and issues a
        // fresh offset-0 window, so the table holds both requests.
        assert_eq!(
            state.in_flight.len(),
            2,
            "stale window + fresh window stay correlated"
        );
        let saved = step2
            .save_state
            .expect("profile switch persists immediately");
        assert_eq!(saved.get("profile").and_then(Value::as_str), Some("raw"));
        assert_eq!(saved.get("topRow").and_then(Value::as_i64), Some(0));
        assert!(
            state.request_log.len() > 1,
            "reset re-issues the first window"
        );
        assert_eq!(
            state
                .request_log
                .first()
                .and_then(|r| r.body.get("GetWindow"))
                .and_then(|w| w.get("filter"))
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool),
            Some(true)
        );

        let mut step3 = Step::new();
        state.set_profile(Profile::Activity, ProfileAction::Reset, &vp(), &mut step3);
        assert_eq!(
            last_get_window(&state)
                .get("filter")
                .and_then(|f| f.get("hide_trace"))
                .and_then(Value::as_bool),
            Some(true),
            "Activity restores hide_trace"
        );
    }

    #[test]
    fn expansion_prefix_sums_mapping_and_toggle_never_change_total() {
        let mut state = HistoryAppState {
            total: Some(6),
            sub_op_counts: vec![2, 0, 1],
            ..Default::default()
        };
        state.recompute_expansion();
        assert_eq!(state.block_starts, vec![0, 3, 4]);
        assert_eq!(state.block_index_of_abs(0), Some(0));
        assert_eq!(state.block_index_of_abs(3), Some(1));
        assert_eq!(state.block_index_of_abs(4), Some(2));
        assert_eq!(
            state.visible_total(),
            3,
            "collapsed view hides 3 sub-op slots"
        );
        assert!(!state.is_block_expanded(0), "collapsed by default");
        assert_eq!(state.abs_index_for_visible(0), Some(0));
        assert_eq!(
            state.abs_index_for_visible(1),
            Some(3),
            "collapsed: vis 1 is block 1's parent (block 0's sub-ops are the hidden gaps)"
        );
        assert_eq!(state.visible_index_for_abs(0), Some(0));
        assert_eq!(
            state.visible_index_for_abs(1),
            None,
            "hidden sub-op maps to nothing"
        );

        assert!(state.toggle_expanded(0), "top-level bundle toggles open");
        assert!(state.is_block_expanded(0));
        assert_eq!(state.visible_total(), 5);
        assert_eq!(state.abs_index_for_visible(1), Some(1));
        assert_eq!(state.abs_index_for_visible(2), Some(2));
        assert_eq!(state.visible_index_for_abs(1), Some(1));
        assert_eq!(
            state.abs_index_for_visible(3),
            Some(3),
            "vis 3 is block 1's parent; expanded block 0's sub-ops occupy vis 1..2"
        );
        assert_eq!(state.abs_index_for_visible(4), Some(4));
        assert_eq!(state.visible_index_for_abs(4), Some(4));
        assert_eq!(
            state.visible_index_for_abs(5),
            None,
            "block 2 is still collapsed"
        );

        assert!(state.toggle_expanded(0), "toggles back closed");
        assert!(!state.is_block_expanded(0));
        assert_eq!(state.visible_total(), 3);
        assert_eq!(state.abs_index_for_visible(1), Some(3));
        assert_eq!(
            state.total,
            Some(6),
            "expansion is a rendering decision only"
        );
        assert_eq!(state.block_index_of_abs(1), None);
        assert!(!state.toggle_expanded(1));
    }

    #[test]
    fn nested_expansion_spans_hide_inner_children_until_both_levels_are_open() {
        let mut state = HistoryAppState {
            total: Some(7),
            session_flags: SessionFlags {
                snapshot_established: true,
                ..SessionFlags::default()
            },
            sub_op_counts: vec![5, 0],
            expansion_spans: BTreeMap::from([(0, 5), (1, 2)]),
            ..Default::default()
        };
        state.recompute_expansion();
        assert_eq!(state.visible_abs, vec![0, 6]);

        assert!(state.toggle_expanded(0));
        assert_eq!(
            state.visible_abs,
            vec![0, 1, 4, 5, 6],
            "opening the work group reveals activities but keeps an inner bundle collapsed"
        );
        assert_eq!(state.visible_index_for_abs(2), None);

        assert!(state.toggle_expanded(1));
        assert_eq!(state.visible_abs, vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(state.visible_index_for_abs(2), Some(2));

        assert!(state.toggle_expanded(0));
        assert_eq!(state.visible_abs, vec![0, 6]);
        assert!(
            state.is_row_expanded(1),
            "inner disclosure state is retained"
        );
    }

    #[test]
    fn open_warnings_collect_strings_and_diagnostics_summary() {
        let value = json!({
            "nodes": 100,
            "repos": 1,
            "warnings": ["  blob 1 missing ", "", "blob 2 unresolved"],
            "diagnostics": { "blobs": { "missing": 0, "corrupt": 0, "unresolved": 0 } },
        });
        assert_eq!(
            HistoryAppState::collect_open_warnings(&value),
            vec!["blob 1 missing", "blob 2 unresolved"]
        );
        let with_blobs = json!({
            "nodes": 1,
            "repos": 1,
            "warnings": [],
            "diagnostics": { "blobs": { "missing": 2, "corrupt": 1, "unresolved": 0 } },
        });
        assert_eq!(
            HistoryAppState::collect_open_warnings(&with_blobs),
            vec!["Chain data integrity: 2 missing, 1 corrupt blob payload(s) in the durable store"]
        );
    }

    #[test]
    fn restore_and_save_state_round_trip() {
        let mut state = HistoryAppState::default();
        assert_eq!(state.restore_state(), (-1, Profile::Activity));
        state.persisted = Some(json!({ "topRow": 12, "profile": "raw" }));
        assert_eq!(state.restore_state(), (12, Profile::Raw));
        state.persisted = Some(json!({ "topRow": 0, "profile": "activity" }));
        assert_eq!(
            state.restore_state(),
            (-1, Profile::Activity),
            "zero topRow restores to top"
        );
        let saved = state.persisted_state(&vp());
        assert_eq!(
            saved.get("profile").and_then(Value::as_str),
            Some("activity")
        );
        assert_eq!(saved.get("topRow").and_then(Value::as_i64), Some(0));
    }

    #[test]
    fn desired_cache_range_maps_visible_window_plus_buffer_after_snapshot() {
        let mut state = HistoryAppState {
            total: Some(6),
            sub_op_counts: vec![0, 1, 0, 1],
            ..Default::default()
        };
        state.recompute_expansion();
        assert_eq!(state.block_starts, vec![0, 1, 3, 4]);
        assert_eq!(state.visible_total(), 4);
        // Deep scroll into visible slots 2..3 (absolute slot 2 and a hidden
        // collapsed sub-op slot of block 3).
        let vp = Viewport::new(2 * ROW_H, 34);
        assert_eq!(HistoryAppState::viewport_visible_top(&vp), 2);
        assert_eq!(state.viewport_visible_bottom(&vp), 3);
        let (top, bottom) = state.desired_cache_range(&vp);
        assert_eq!(top, 0, "cache keeps from absolute slot 0");
        // The bottom visible slot (vis 3) maps to block 3's parent row
        // (abs 4), which is drawable; only a null mapping falls back to
        // `total - 1` in main.js `desiredCacheRange()`.
        assert_eq!(bottom, 4);
    }

    #[test]
    fn evict_far_windows_bounds_the_cache_around_the_desired_range() {
        let mut state = HistoryAppState {
            total: Some(2000),
            session_flags: SessionFlags {
                snapshot_established: true,
                ..SessionFlags::default()
            },
            ..Default::default()
        };
        state.recompute_expansion();
        for i in 0..2000i64 {
            drop(state.cache.insert(i, row(i, &format!("node:{i}"), 1)));
        }
        state.evict_far_windows(&vp());
        assert!(!state.cache.contains_key(&1500), "far rows evicted");
        assert!(state.cache.contains_key(&0));
        // Desired cache range bottom is viewport bottom (row 23) + BUFFER 400
        // = absolute 423 (identity mapping); main.js `evictFarWindows` keeps
        // an extra BUFFER margin, so everything beyond 423 + 400 = 823 drops.
        assert!(
            state.cache.contains_key(&423),
            "the desired-range bottom stays cached"
        );
        assert!(
            state.cache.contains_key(&823),
            "the eviction BUFFER margin stays cached"
        );
        assert!(
            !state.cache.contains_key(&824),
            "rows past the eviction BUFFER margin are evicted"
        );
        assert_eq!(state.cache.len(), 824);
    }

    #[test]
    fn sync_window_reanchors_when_offscreen_window_moves() {
        let mut state = HistoryAppState {
            total: Some(2000),
            session_flags: SessionFlags {
                snapshot_established: true,
                layout_ready: true,
                ..SessionFlags::default()
            },
            ..Default::default()
        };
        state.recompute_expansion();
        for i in 0..2000i64 {
            drop(state.cache.insert(i, row(i, &format!("node:{i}"), 1)));
        }
        state.render_top = 0;
        state.render_bottom = 399;
        let vp = Viewport::new(1000 * ROW_H, 800);
        let mut step = Step::new();
        state.sync_window(&vp, &mut step);
        assert!(step.ops.iter().any(|op| matches!(
            op,
            DomOp::Reanchor {
                top: 600,
                bottom: 1423
            }
        )));
        assert_eq!(
            state.render_top, 600,
            "window re-anchors with BUFFER margins"
        );
        assert_eq!(state.render_bottom, 1423);
        assert_eq!(
            state.pending_window_req_id, None,
            "fully-cached window issues no fetch"
        );
    }

    #[test]
    fn open_error_is_visible_and_empty_open_shows_empty_state_without_fetching() {
        let mut state = HistoryAppState::default();
        let mut step = Step::new();
        state.handle_host_message(open_msg(0), &vp(), &mut step);
        assert!(state.view_flags.data_ready);
        assert!(step.ops.iter().any(|op| matches!(
            op,
            DomOp::ShowMessage { text, error: false } if text == "No history found in this workspace"
        )));
        assert!(
            state.request_log.is_empty(),
            "an empty chain never fetches a window"
        );
        assert_eq!(state.pending_window_req_id, None);

        let mut state2 = HistoryAppState::default();
        let mut step2 = Step::new();
        state2.handle_host_message(
            HostMessage::parse(
                &json!({ "id": "open", "body": { "Error": "service unavailable" } }),
            )
            .unwrap(),
            &vp(),
            &mut step2,
        );
        assert!(state2.view_flags.data_ready);
        assert!(step2.ops.iter().any(|op| matches!(
            op,
            DomOp::ShowMessage { text, error: true } if text == "Failed to open history: service unavailable"
        )));
        assert!(
            state2.request_log.is_empty(),
            "a failed open never fetches a window"
        );
    }

    #[test]
    fn request_error_paths_render_retry_and_never_poison_state() {
        let mut state = HistoryAppState {
            total: Some(10),
            ..Default::default()
        };
        let mut step = Step::new();
        state.submit_find("q", &mut step);
        let find_id = state.request_log.last().expect("find request").id;
        let mut step2 = Step::new();
        state.handle_host_message(
            resp(find_id, &json!({ "Error": "find service error" })),
            &vp(),
            &mut step2,
        );
        assert!(!state.find_flags.active);
        assert!(step2.ops.iter().any(|op| matches!(
            op,
            DomOp::FindCounter(FindCounterState::Error(e)) if e == "find service error"
        )));
        assert!(
            !state.view_flags.data_ready,
            "find errors never flip readiness"
        );

        let mut step3 = Step::new();
        state.show_request_error(
            &mut step3,
            "Failed to load history rows: dead service",
            RetryAction::ResetHistory,
        );
        assert!(step3.ops.iter().any(|op| matches!(
            op,
            DomOp::ShowRequestError { text, retry: RetryAction::ResetHistory }
                if text == "Failed to load history rows: dead service"
        )));
        assert!(state.view_flags.data_ready);
    }

    #[test]
    fn find_navigation_enabled_guards_the_exact_submitted_query() {
        let mut state = HistoryAppState {
            total: Some(10),
            session_flags: SessionFlags {
                snapshot_established: true,
                ..SessionFlags::default()
            },
            ..Default::default()
        };
        state.recompute_expansion();
        for i in 0..10i64 {
            drop(state.cache.insert(i, row(i, &format!("node:{i}"), 1)));
        }
        let mut step = Step::new();
        state.submit_find("needle", &mut step);
        let find_id = state.request_log.last().expect("find request").id;
        let mut step2 = Step::new();
        state.handle_host_message(
            resp(
                find_id,
                &json!({ "Ok": { "matches": [{ "row": 3 }], "returned": 1, "more": false } }),
            ),
            &vp(),
            &mut step2,
        );
        assert!(
            state.find_navigation_enabled("needle"),
            "settled exact query navigates"
        );
        assert!(
            !state.find_navigation_enabled("needl"),
            "edited text never navigates stale matches"
        );
        assert!(
            !state.find_navigation_enabled(""),
            "cleared input never navigates"
        );

        // clearFind ends the session without reloading history or moving the
        // scroll position (the chain was never replaced).
        let mut step3 = Step::new();
        state.clear_find(&mut step3);
        assert!(!state.find_flags.active);
        assert!(state.find_matches.is_empty());
        assert_eq!(state.search_query, "");
        assert_eq!(state.current_search_epoch, None);
        assert!(step3
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::FindCounter(FindCounterState::Hidden))));
        assert!(!state.find_navigation_enabled("needle"));
    }

    #[test]
    fn selection_and_roving_state_feed_the_live_row_context() {
        let mut state = HistoryAppState {
            total: Some(6),
            session_flags: SessionFlags {
                snapshot_established: true,
                ..SessionFlags::default()
            },
            sub_op_counts: vec![2, 0],
            ..Default::default()
        };
        state.recompute_expansion();
        for i in 0..6i64 {
            drop(state.cache.insert(i, row(i, &format!("node:{i}"), 1)));
        }
        // Select row 1 and pin the roving anchor to row 3.
        state.select_row(1);
        state.set_roving_abs(3);
        assert_eq!(state.selected_key(), Some("node:1"));
        assert_eq!(state.roving_abs(), 3);
        let context = state.row_context(super::super::rows::ViewMode::Activity, 1, false);
        assert_eq!(context.selected_key.as_deref(), Some("node:1"));
        assert_eq!(context.roving_abs, Some(3));
        let selected_spec =
            super::super::rows::RowSpec::from_value(state.cache.get(&1).expect("row 1"), &context);
        assert!(selected_spec.classes().contains("row-selected"));
        let unselected = state.row_context(super::super::rows::ViewMode::Activity, 2, false);
        assert_eq!(
            unselected.selected_key.as_deref(),
            Some("node:1"),
            "the context carries the global selection key for every row"
        );
        assert_eq!(
            unselected.roving_abs,
            Some(3),
            "the context carries the roving anchor for every row"
        );
        let unselected_spec = super::super::rows::RowSpec::from_value(
            state.cache.get(&2).expect("row 2"),
            &unselected,
        );
        assert!(
            !unselected_spec.classes().contains("row-selected"),
            "non-selected rows never carry the selected class"
        );
        assert_eq!(
            unselected_spec.aria.tabindex, -1,
            "only the roving anchor row is tabbable"
        );
        let anchor_spec = super::super::rows::RowSpec::from_value(
            state.cache.get(&3).expect("row 3"),
            &state.row_context(super::super::rows::ViewMode::Activity, 3, false),
        );
        assert_eq!(anchor_spec.aria.tabindex, 0);

        // A settled find match marks the row as find-current.
        let mut step = Step::new();
        state.submit_find("needle", &mut step);
        let find_id = state.request_log.last().expect("find request").id;
        let mut step2 = Step::new();
        state.handle_host_message(
            resp(
                find_id,
                &json!({ "Ok": { "matches": [{ "row": 3 }], "returned": 1, "more": false } }),
            ),
            &vp(),
            &mut step2,
        );
        let found = state.row_context(super::super::rows::ViewMode::Activity, 3, false);
        assert!(
            found.find_current,
            "the current match row carries find_current"
        );

        state.clear_selection();
        assert_eq!(state.selected_key(), None);
    }

    #[test]
    fn toggle_expanded_ui_reanchors_the_desired_window_and_fetches() {
        let mut state = HistoryAppState {
            total: Some(11),
            session_flags: SessionFlags {
                snapshot_established: true,
                ..SessionFlags::default()
            },
            sub_op_counts: vec![4, 0, 4],
            ..Default::default()
        };
        state.recompute_expansion();
        for i in [0, 5, 6] {
            drop(state.cache.insert(i, row(i, &format!("node:{i}"), 1)));
        }
        let viewport = Viewport::new(0, 800);
        let mut step = Step::new();
        state.toggle_expanded_ui(0, &viewport, &mut step);
        assert!(state.is_block_expanded(0), "the parent block toggles open");
        let (want_top, want_bottom) = state.desired_visible_range(&viewport);
        assert_eq!(state.render_top, want_top);
        assert_eq!(state.render_bottom, want_bottom);
        assert!(
            step.ops.iter().any(|op| matches!(
                op,
                DomOp::Reanchor { top, bottom }
                    if *top == want_top && *bottom == want_bottom
            )),
            "a full desired-window rebuild is planned"
        );
        assert!(
            step.sends
                .iter()
                .any(|send| matches!(send, Send::Request { .. })),
            "ensureFilled re-fetches missing rows"
        );

        // A non-expandable row never toggles.
        let mut step2 = Step::new();
        state.toggle_expanded_ui(1, &viewport, &mut step2);
        assert!(!state.is_block_expanded(1));
        assert!(step2.ops.is_empty());
    }

    #[test]
    fn submit_legacy_search_reissues_the_search_with_a_fresh_epoch() {
        let mut state = HistoryAppState::default();
        let mut step = Step::new();
        state.submit_legacy_search("needle", &mut step);
        assert_eq!(state.search_query, "needle");
        assert_eq!(state.current_search_epoch, Some(1));
        assert!(step
            .ops
            .iter()
            .any(|op| matches!(op, DomOp::FindCounter(FindCounterState::Pending))));
        let body = &state.request_log.last().expect("search request").body;
        let search = body.get("Search").expect("legacy Search envelope");
        assert_eq!(search.get("query").and_then(Value::as_str), Some("needle"));
        assert_eq!(search.get("mode").and_then(Value::as_str), Some("Lexical"));
        assert_eq!(
            search.get("top_k").and_then(Value::as_i64),
            Some(FIND_TOP_K)
        );
        assert_eq!(state.in_flight.len(), 1);

        // A fresh submission bumps the epoch; the older response is dropped.
        let mut step2 = Step::new();
        state.submit_legacy_search("newer", &mut step2);
        assert_eq!(state.current_search_epoch, Some(2));
        let older_id = state.request_log.first().expect("older search").id;
        let mut step3 = Step::new();
        state.handle_host_message(
            resp(
                older_id,
                &json!({ "Ok": { "results": [{ "text": "stale", "op_id": "op:1" }] } }),
            ),
            &vp(),
            &mut step3,
        );
        assert!(
            step3
                .sends
                .iter()
                .any(|send| matches!(send, Send::Log(text) if text.contains("stale search"))),
            "the stale response is dropped"
        );
    }
}
