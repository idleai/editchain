//! Rust-owned browser slice (3A): window/frame/lane presentation + DOM shell.
//!
//! This module is the first coherent vertical slice of the Rust-owned history
//! view and contains two layers:
//!
//! - Pure, native-tested helpers (compiled on every target):
//!   - [`graph_layout`] — the exact production lane geometry (`LANE_W *
//!     LANE_W_PULSE_SCALE` = the fixed 14.76px Pulse pitch), the graph-column
//!     budget, and the "no width autoscaling" invariant: lane X positions are
//!     derived from lane count only and are never rescaled to the column
//!     width.
//!   - [`window_rows`] — the ordered `RowSpec` plan for a rendered window
//!     (group-start chips, placeholders, visible/absolute mapping).
//!   - [`canvas_view`] / [`frame_rows`] / [`build_frame_value`] — the
//!     `GpuRenderer` frame contract serialized directly from cached/`RowSpec`
//!     data with no DOM observation.
//! - The wasm32-only [`HistoryDom`] shell: creates the single canvas under
//!   `#gpu-canvas-host`, renders rows as real DOM/text nodes (never
//!   application `innerHTML` strings), and owns the scroll-window mutations
//!   and Activity/Raw profile-control state.
//!
//! The host message bridge, `HistoryAppState` ownership, and `GpuRenderer`
//! instantiation live in the wasm32 shell in `crate::lib`; they drive this
//! module's pure plans into the DOM.

use serde_json::{json, Value};

use super::host::row as row_reader;
use super::rows::{RowSpec, ViewMode};
use super::state::{HistoryAppState, Profile, ROW_H};

// ---------------------------------------------------------------------------
// Pure graph/lane geometry (exact production constants)
// ---------------------------------------------------------------------------

/// Production `LANE_W` (media/main.js): the natural per-lane grid step.
pub(crate) const LANE_W: f64 = 18.0;

/// Production `LANE_W_PULSE_SCALE`: the fixed Pulse quiet-rail pitch factor.
pub(crate) const LANE_W_PULSE_SCALE: f64 = 0.82;

/// The fixed Pulse lane pitch (`LANE_W * LANE_W_PULSE_SCALE` = 14.76 CSS px).
///
/// Kept as an exact literal so serialized frames carry precisely 14.76 for
/// lane centers (`lane_x[0] = 14.76`, `lane_x[1] = 29.52`, …), matching the
/// production adapter's `laneXAll()` doubles bit-for-bit.
#[cfg(test)]
pub(crate) const PULSE_LANE_PITCH: f64 = 14.76;

/// Production `MIN_LANE_W`: lane-spacing floor under dense compression.
pub(crate) const MIN_LANE_W: f64 = 1.5;

/// Production `DOT_R`: node-dot radius before compression shrinking.
pub(crate) const DOT_R: f64 = 4.0;

/// Production graph stroke width (media/main.css Pulse override).
pub(crate) const LINE_WIDTH_CSS_PX: f64 = 1.4;

/// Production bundle glyph half-height/span (CSS px).
pub(crate) const BUNDLE_HALF_HEIGHT_CSS_PX: f64 = 7.0;

/// Production bundle capsule margin (CSS px).
pub(crate) const BUNDLE_MARGIN_CSS_PX: f64 = 1.0;

/// Production `MIN_COL_W.graph` (CSS px).
pub(crate) const MIN_GRAPH_COL_W: f64 = 40.0;

/// Production `MIN_CONTENT_W`: readable Content summary budget (CSS px).
pub(crate) const MIN_CONTENT_W: f64 = 160.0;

/// Production `DEFAULT_COL_W.date` (author/commit are hidden in Pulse).
pub(crate) const DEFAULT_COL_W_DATE: f64 = 140.0;

/// Production `HIDE_DATE_MAX`: at/below this width the date column drops too.
pub(crate) const HIDE_DATE_MAX: f64 = 400.0;

/// Production `GRAPH_MAX_FRACTION` cap on the graph column.
pub(crate) const GRAPH_MAX_FRACTION: f64 = 0.5;

/// Production `GRAPH_MAX_W_NARROW` cap for narrow panels.
pub(crate) const GRAPH_MAX_W_NARROW: f64 = 120.0;

/// Production compact-rail breakpoint width (CSS px, `<=480px`).
pub(crate) const COMPACT_RAIL_MAX_WIDTH: f64 = 480.0;

/// Bootstrap `MAX_SURFACE_EDGE` cap for WebGL texture limits.
pub(crate) const MAX_SURFACE_EDGE: f64 = 2048.0;

/// Bootstrap overscan rows kept visible above/below the canvas viewport.
pub(crate) const OVERSCAN_ROWS: f64 = 1.0;

/// The editor background the frame reports when the CSS variable is absent.
pub(crate) const DEFAULT_EDITOR_BACKGROUND_HEX: &str = "#1e1e1e";

/// Round a CSS-pixel value to two decimals like the production formatter.
pub(crate) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Exact `i64 → f64` for values below 2^53 (every pixel value this shell
/// produces): splitting into high/low u32 halves avoids the lossy float casts
/// denied crate-wide, mirroring `u32_to_f32` in `crate::lib`.
pub(crate) fn i64_to_f64(value: i64) -> f64 {
    let high = i32::try_from(value >> 32).unwrap_or(0);
    let low = u32::try_from(value & 0xffff_ffff_i64).unwrap_or(0);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

/// Round a CSS/device float to a whole `i64` without a cast: Rust std has no
/// lossless float→integer conversion, so a binary search runs over the exact
/// [`i64_to_f64`] mapping (correct for the bounded integral values produced
/// here).
pub(crate) fn f64_round_to_i64(value: f64) -> i64 {
    const MAX_EXACT: i64 = 9_007_199_254_740_992; // 2^53
    let max_exact = i64_to_f64(MAX_EXACT);
    let target = value.round().clamp(0.0, max_exact);
    let mut low = 0_i64;
    let mut high = MAX_EXACT;
    while low < high {
        let mid = low.saturating_add(high.saturating_sub(low).saturating_div(2));
        if i64_to_f64(mid) < target {
            low = mid.saturating_add(1);
        } else {
            high = mid;
        }
    }
    low
}

/// The pure lane/column layout for the Rust-owned graph (Slice 3A).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GraphLayout {
    /// Lane-center X positions for lanes `0..=max_lane` (CSS px, fixed).
    pub(crate) lane_x: Vec<f64>,
    /// Effective per-lane width (CSS px); 14.76 unless dense compression.
    pub(crate) lane_width: f64,
    /// Node-dot radius (CSS px), compressed with dense lanes.
    pub(crate) dot_radius: f64,
    /// The graph column's CSS width (natural lane width, budget-capped).
    pub(crate) column_width: f64,
}

/// Replicate `graphWidthBudget()`: fixed columns reserve their space first and
/// the graph gets the remainder, capped at half the viewport (and at the
/// compact rail width on narrow panels). Pulse hides author/commit at every
/// width; date is retained until the narrowest breakpoint.
fn graph_width_budget(rows_client_width: f64, window_inner_width: f64) -> f64 {
    let rows_w = rows_client_width.max(1.0);
    let hidden_date = window_inner_width <= HIDE_DATE_MAX;
    let fixed_w = if hidden_date { 0.0 } else { DEFAULT_COL_W_DATE };
    let graph_cap = (rows_w * GRAPH_MAX_FRACTION).floor().max(MIN_GRAPH_COL_W);
    let avail = (rows_w - fixed_w - MIN_CONTENT_W).max(MIN_GRAPH_COL_W);
    let budget = graph_cap.min(avail);
    if window_inner_width <= COMPACT_RAIL_MAX_WIDTH {
        budget.min(GRAPH_MAX_W_NARROW)
    } else {
        budget
    }
}

/// `graphLaneWidth()` + `laneX()` + `graphNaturalWidth()` in Rust.
///
/// Lane positions are computed from the lane count and the natural graph
/// width only; the rendered column width never rescales them (the "no width
/// autoscaling" invariant). With ordinary lane counts the pitch is exactly
/// `LANE_W * LANE_W_PULSE_SCALE` (14.76 CSS px) and `lane_x[l] = (l + 1) * pitch`.
pub(crate) fn graph_layout(
    max_lane: u32,
    rows_client_width: f64,
    window_inner_width: f64,
) -> GraphLayout {
    let num_lanes = f64::from(max_lane.saturating_add(1));
    let budget = graph_width_budget(rows_client_width, window_inner_width);
    let lane_width = (LANE_W * LANE_W_PULSE_SCALE)
        .min(budget / (num_lanes + 1.0))
        .max(MIN_LANE_W);
    let natural = ((num_lanes + 1.0) * lane_width).max(32.0).min(budget);
    let column_width = round2(natural);
    let lane_x = (0..=max_lane)
        .map(|lane| {
            let index = f64::from(lane);
            if num_lanes * lane_width <= column_width {
                (index + 1.0) * lane_width
            } else {
                (index + 0.5) * (column_width / num_lanes)
            }
        })
        .map(round2)
        .collect();
    GraphLayout {
        lane_x,
        lane_width,
        dot_radius: (lane_width / 2.0).clamp(1.5, DOT_R),
        column_width,
    }
}

/// Production `MIN_COL_W` per resizable column (media/main.js).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColKey {
    Graph,
    Content,
    Date,
    Author,
    Commit,
}

impl ColKey {
    /// The CSS var/class segment for this column (`--graph-w`, `.th.graph`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ColKey::Graph => "graph",
            ColKey::Content => "content",
            ColKey::Date => "date",
            ColKey::Author => "author",
            ColKey::Commit => "commit",
        }
    }

    /// Parse the `data-col` wire value back to a column key.
    pub(crate) fn parse(value: &str) -> Option<ColKey> {
        match value {
            "graph" => Some(ColKey::Graph),
            "content" => Some(ColKey::Content),
            "date" => Some(ColKey::Date),
            "author" => Some(ColKey::Author),
            "commit" => Some(ColKey::Commit),
            _ => None,
        }
    }

    /// Production `MIN_COL_W` for this column.
    pub(crate) fn min_width(self) -> f64 {
        match self {
            ColKey::Graph => 40.0,
            ColKey::Content | ColKey::Author | ColKey::Commit => 60.0,
            ColKey::Date => 90.0,
        }
    }

    /// Production `DEFAULT_COL_W` for the fixed columns (Pulse hides
    /// author/commit at every width; the date default applies when visible).
    pub(crate) fn default_width(self) -> f64 {
        match self {
            ColKey::Graph | ColKey::Content => 0.0,
            ColKey::Date => DEFAULT_COL_W_DATE,
            ColKey::Author | ColKey::Commit => 100.0,
        }
    }
}

/// User-dragged per-column width overrides (`colWidths` in main.js):
/// `None` = natural/default behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct ColWidths {
    /// `--graph-w`: natural lane-based width unless the divider was dragged.
    pub(crate) graph: Option<f64>,
    /// `--content-w`: flexible `minmax(0,1fr)` unless dragged.
    pub(crate) content: Option<f64>,
    /// `--date-w` override (author/commit are always hidden in Pulse).
    pub(crate) date: Option<f64>,
    pub(crate) author: Option<f64>,
    pub(crate) commit: Option<f64>,
}

impl ColWidths {
    /// The current override (or default) width for a fixed column.
    pub(crate) fn width(&self, col: ColKey) -> f64 {
        match col {
            ColKey::Graph => self.graph.unwrap_or(0.0),
            ColKey::Content => self.content.unwrap_or(0.0),
            ColKey::Date => self.date.unwrap_or_else(|| col.default_width()),
            ColKey::Author => self.author.unwrap_or_else(|| col.default_width()),
            ColKey::Commit => self.commit.unwrap_or_else(|| col.default_width()),
        }
    }

    /// Set (or clear, with `None`) a dragged width for a column.
    pub(crate) fn set(&mut self, col: ColKey, width: Option<f64>) {
        match col {
            ColKey::Graph => self.graph = width,
            ColKey::Content => self.content = width,
            ColKey::Date => self.date = width,
            ColKey::Author => self.author = width,
            ColKey::Commit => self.commit = width,
        }
    }
}

/// Production `hiddenColumns()`: Pulse always hides author/commit; the date
/// column drops at the narrowest breakpoint (`<=400px`).
pub(crate) fn hidden_columns(window_inner_width: f64) -> Vec<ColKey> {
    let mut hidden = vec![ColKey::Author, ColKey::Commit];
    if window_inner_width <= HIDE_DATE_MAX {
        hidden.push(ColKey::Date);
    }
    hidden
}

/// `currentGraphWidth()` — the effective graph column width (divider override
/// or the natural lane-based width), never below the graph column minimum.
/// Lane X positions are computed against the NATURAL width only, so dragging
/// the divider clips/extends the rail without rescaling the topology.
pub(crate) fn current_graph_width(layout: &GraphLayout, widths: &ColWidths) -> f64 {
    widths
        .graph
        .unwrap_or(layout.column_width)
        .max(ColKey::Graph.min_width())
}

/// The inline column-style string applied to rows/header/wrap (`colStyle()`).
///
/// Pulse hides author/commit at every width; date is fixed at its default
/// width until the narrowest breakpoint. Dragged overrides (divider state)
/// are applied exactly like `colWidths` in main.js.
pub(crate) fn col_style(
    graph_width_css: f64,
    window_inner_width: f64,
    widths: &ColWidths,
) -> String {
    let mut parts = vec![format!("--graph-w:{graph_width_css}px")];
    if let Some(content) = widths.content {
        parts.push(format!("--content-w:{content}px"));
    }
    let hidden = hidden_columns(window_inner_width);
    for col in [ColKey::Date, ColKey::Author, ColKey::Commit] {
        if hidden.contains(&col) {
            continue;
        }
        parts.push(format!("--{}-w:{}px", col.as_str(), widths.width(col)));
    }
    parts.join(";")
}

// ---------------------------------------------------------------------------
// Pure window/frame planning
// ---------------------------------------------------------------------------

/// One planned window row: the visible index and its presentation spec.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WindowRow {
    /// Visible (drawable) index; `abs` may differ when sub-op slots are hidden.
    pub(crate) vis: i64,
    /// The row's presentation spec (placeholder when uncached).
    pub(crate) spec: RowSpec,
}

/// Build the ordered window rows `[top, bottom]` (visible indices).
///
/// Mirrors production `reanchorTo` group-start semantics: the first rendered
/// row starts a group unconditionally and placeholders never update the
/// running group.
pub(crate) fn window_rows(state: &HistoryAppState, top: i64, bottom: i64) -> Vec<WindowRow> {
    window_rows_from(state, top, bottom, None)
}

/// The shared window walker with an explicit group for the row rendered just
/// above the window. `initial_group` seeds the running group so an additive
/// bottom append (production `appendRowsBelow`) does not mark a same-group
/// row as a group start; `None` marks the first row as a group start.
pub(crate) fn window_rows_from(
    state: &HistoryAppState,
    top: i64,
    bottom: i64,
    initial_group: Option<&str>,
) -> Vec<WindowRow> {
    let view = match state.profile {
        Profile::Activity => ViewMode::Activity,
        Profile::Raw => ViewMode::Raw,
    };
    let mut rows = Vec::new();
    let mut last_group = initial_group;
    for vis in top..=bottom {
        let Some(abs) = state.abs_index_for_visible(vis) else {
            continue; // hidden sub-op slot — no drawable row
        };
        let Some(row) = state.cache.get(&abs) else {
            rows.push(WindowRow {
                vis,
                spec: RowSpec::placeholder(abs),
            });
            continue;
        };
        let group = row_reader::str(row, "group");
        let is_group_start = last_group.is_none_or(|last| last != group);
        if is_group_start {
            last_group = Some(group);
        }
        rows.push(WindowRow {
            vis,
            spec: RowSpec::from_value(row, &state.row_context(view, abs, is_group_start)),
        });
    }
    rows
}

/// `window_rows` trimmed to the `RowSpec` list (the DOM render input).
pub(crate) fn window_specs(state: &HistoryAppState, top: i64, bottom: i64) -> Vec<RowSpec> {
    window_rows(state, top, bottom)
        .into_iter()
        .map(|row| row.spec)
        .collect()
}

/// The bounded canvas surface descriptor (bootstrap `canvasDimensions()`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CanvasView {
    /// CSS-pixel surface size (graph column width × rows viewport height).
    pub(crate) css_width: f64,
    pub(crate) css_height: f64,
    /// Backing-store (device pixel) size passed to `GpuRenderer::render`.
    pub(crate) backing_width: u32,
    pub(crate) backing_height: u32,
    /// Backing/CSS scale factor.
    pub(crate) scale: f64,
}

/// Size the canvas surface like bootstrap.js: DPR-aware, bounded so a long or
/// narrow window never exceeds common WebGL texture limits. CSS size is capped
/// at 1 CSS px so the surface descriptor always stays renderable.
pub(crate) fn canvas_view(css_width: f64, css_height: f64, device_pixel_ratio: f64) -> CanvasView {
    let preferred_scale = device_pixel_ratio.clamp(1.0, 2.0);
    let scale = preferred_scale
        .min(MAX_SURFACE_EDGE / css_width.max(1.0))
        .min(MAX_SURFACE_EDGE / css_height.max(1.0))
        .max(0.125);
    CanvasView {
        css_width: css_width.max(1.0),
        css_height: css_height.max(1.0),
        backing_width: u32::try_from(f64_round_to_i64(css_width.max(1.0) * scale).max(1))
            .unwrap_or(1),
        backing_height: u32::try_from(f64_round_to_i64(css_height.max(1.0) * scale).max(1))
            .unwrap_or(1),
        scale,
    }
}

/// One serialized frame row (the `GpuRenderer` contract row shape; snapshot
/// rows share it).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrameRow {
    /// Absolute expanded-history index.
    pub(crate) index: i64,
    /// Stable wire identity / node key.
    pub(crate) key: String,
    /// Canvas-relative CSS pixel top of the row's 34px band.
    pub(crate) top: f64,
    /// Canvas-relative CSS pixel bottom of the row's band.
    pub(crate) bottom: f64,
    /// Canvas-relative CSS pixel vertical center.
    pub(crate) middle: f64,
    /// The row's own lane.
    pub(crate) lane: u32,
    /// Lanes entering from above.
    pub(crate) above: Vec<u32>,
    /// Lanes leaving downward.
    pub(crate) below: Vec<u32>,
    /// Directed child→parent transition pairs.
    pub(crate) transitions: Vec<(u32, u32)>,
    /// Whether this is an expanded sub-op row.
    pub(crate) is_subop: bool,
    /// Whether this row is a typed Activity bundle.
    pub(crate) is_bundle: bool,
}

/// Build the `GpuRenderer` frame rows directly from cached/`RowSpec` data for
/// the current render window, keeping only rows intersecting the canvas
/// viewport (plus one row of overscan), exactly like bootstrap's collector.
pub(crate) fn frame_rows(
    state: &HistoryAppState,
    scroll_top: i64,
    host_height_css: f64,
) -> Vec<FrameRow> {
    let row_h_css = i64_to_f64(ROW_H);
    let visible_top = -OVERSCAN_ROWS * row_h_css;
    let visible_bottom = host_height_css + OVERSCAN_ROWS * row_h_css;
    let scroll_top_css = i64_to_f64(scroll_top);
    let mut out = Vec::new();
    for row in window_rows(state, state.render_top, state.render_bottom) {
        let vis_css = i64_to_f64(row.vis);
        let top = vis_css * row_h_css - scroll_top_css;
        let bottom = top + row_h_css;
        if bottom < visible_top || top > visible_bottom {
            continue;
        }
        let identity = &row.spec.identity;
        let graph = &row.spec.graph;
        out.push(FrameRow {
            index: identity.abs_index,
            key: identity.node_key.clone(),
            top: round2(top),
            bottom: round2(bottom),
            middle: round2((top + bottom) * 0.5),
            lane: graph.lane,
            above: graph.above.clone(),
            below: graph.below.clone(),
            transitions: graph.transitions.clone(),
            is_subop: graph.is_subop,
            is_bundle: graph.is_bundle,
        });
    }
    out
}

/// Serialize the complete frame contract (graph + rows) as JSON.
pub(crate) fn build_frame_value(
    layout: &GraphLayout,
    background_color: &str,
    rows: &[FrameRow],
) -> Value {
    let lane_x: Vec<Value> = layout.lane_x.iter().copied().map(Value::from).collect();
    let frame_rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "index": row.index,
                "key": row.key,
                "node_key": row.key,
                "top": row.top,
                "bottom": row.bottom,
                "middle": row.middle,
                "lane": row.lane,
                "above": row.above,
                "below": row.below,
                "transitions": row.transitions,
                "is_subop": row.is_subop,
                "is_bundle": row.is_bundle,
            })
        })
        .collect();
    json!({
        "graph": {
            "left": 0,
            "width": layout.column_width,
            "lane_x": lane_x,
            "dot_radius": layout.dot_radius,
            "line_width": LINE_WIDTH_CSS_PX,
            "bundle_half_height": BUNDLE_HALF_HEIGHT_CSS_PX,
            "bundle_margin": BUNDLE_MARGIN_CSS_PX,
            "background_color": background_color,
        },
        "rows": frame_rows,
    })
}

// ---------------------------------------------------------------------------
// wasm32 DOM shell
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod web {
    /// The pane-level chrome a full rebuild renders above the grid (production
    /// `warningHtml` + `bannerHtml` in `reanchorTo`).
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct PaneStatus {
        /// Legacy flat-list search mode: renders the results banner.
        pub(crate) search_mode: bool,
        /// The submitted query (banner text).
        pub(crate) search_query: String,
        /// The current authoritative total (banner count).
        pub(crate) total: i64,
        /// Non-blocking Open-response chain warnings.
        pub(crate) open_warnings: Vec<String>,
    }

    /// Everything a full rebuild needs beyond the row specs and column style.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct RebuildOptions {
        /// Scroll-spacer height (full history, CSS px).
        pub(crate) spacer_height_px: i64,
        /// The rendered window's offset inside the spacer (CSS px).
        pub(crate) wrap_top_px: i64,
        /// `aria-rowcount` (visible total).
        pub(crate) aria_rowcount: i64,
        /// The effective graph column width (label accessibility decision).
        pub(crate) graph_width_css: f64,
        /// Warnings/banner chrome state.
        pub(crate) status: PaneStatus,
    }

    use std::cell::Cell;
    use std::fmt;

    use wasm_bindgen::prelude::*;

    use super::{
        f64_round_to_i64, i64_to_f64, ColKey, FrameRow, Profile, RowSpec,
        DEFAULT_EDITOR_BACKGROUND_HEX, ROW_H,
    };
    use crate::app::rows::RowSummary;
    use crate::app::state::{FindCounterState, HistoryAppState, Viewport};

    /// Build a `JsValue` error string.
    pub(crate) fn js_error(message: impl fmt::Display) -> JsValue {
        JsValue::from_str(&message.to_string())
    }

    /// Convert a failed wasm-bindgen/DOM call into the shell's `JsValue`
    /// error type (casts return the rejected value; DOM calls return
    /// `JsValue`).
    fn js_err_from<T: Into<JsValue>>(error: T) -> JsValue {
        error.into()
    }

    /// Convert an element to its `Node` handle for DOM insertion.
    fn node_of(element: &web_sys::HtmlElement) -> Result<web_sys::Node, JsValue> {
        element
            .clone()
            .dyn_into::<web_sys::Node>()
            .map_err(js_err_from)
    }

    /// Query one required element by id, cast to `T`.
    fn require_element<T: JsCast>(document: &web_sys::Document, id: &str) -> Result<T, JsValue> {
        let element = document
            .get_element_by_id(id)
            .ok_or_else(|| js_error(format!("element #{id} is missing")))?;
        element
            .dyn_into::<T>()
            .map_err(|error| js_error(format!("element #{id} has the wrong type: {error:?}")))
    }

    /// The Rust-owned DOM shell (Slice 3A): owns `#rows`, the single GPU
    /// canvas under `#gpu-canvas-host`, the status surfaces, and the profile
    /// buttons. All row content is built with DOM/text nodes; application
    /// strings never pass through `innerHTML`.
    pub(crate) struct HistoryDom {
        rows: web_sys::HtmlDivElement,
        canvas_host: web_sys::HtmlDivElement,
        canvas: web_sys::HtmlCanvasElement,
        status_live: web_sys::HtmlElement,
        gpu_status: web_sys::HtmlElement,
        gpu_backend: web_sys::HtmlElement,
        gpu_rows_mirror: web_sys::HtmlElement,
        search_counter: web_sys::HtmlElement,
        search_input: web_sys::HtmlInputElement,
        search_prev: web_sys::HtmlButtonElement,
        search_next: web_sys::HtmlButtonElement,
        profile_activity: web_sys::HtmlButtonElement,
        profile_raw: web_sys::HtmlButtonElement,
        /// The graph column's effective CSS width (divider override or
        /// natural); the header label accessibility decision reads it.
        graph_width: Cell<f64>,
        /// Measured once: the smallest graph column width that renders the
        /// "Graph" columnheader label without clipping (`graphLabelMinW`).
        graph_label_min_width: Cell<Option<f64>>,
    }

    impl fmt::Debug for HistoryDom {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("HistoryDom")
                .field("rows", &"#rows")
                .field("canvas_id", &self.canvas.id())
                .field("canvas_host", &"#gpu-canvas-host")
                .finish_non_exhaustive()
        }
    }

    impl HistoryDom {
        /// Query the page scaffold and create the single canvas under
        /// `#gpu-canvas-host` (idempotent when the page already hosts one).
        pub(crate) fn new() -> Result<HistoryDom, JsValue> {
            let window =
                web_sys::window().ok_or_else(|| js_error("browser window is unavailable"))?;
            let document = window
                .document()
                .ok_or_else(|| js_error("browser document is unavailable"))?;
            let rows = require_element(&document, "rows")?;
            let canvas_host: web_sys::HtmlDivElement =
                require_element(&document, "gpu-canvas-host")?;
            let status_live = require_element(&document, "status-live")?;
            let gpu_status = require_element(&document, "gpu-status")?;
            let gpu_backend = require_element(&document, "gpu-backend")?;
            let gpu_rows_mirror = require_element(&document, "gpu-rows")?;
            let search_counter = require_element(&document, "search-counter")?;
            let search_input = require_element(&document, "search")?;
            let search_prev = require_element(&document, "search-prev")?;
            let search_next = require_element(&document, "search-next")?;
            let profile_activity = require_element(&document, "profile-activity")?;
            let profile_raw = require_element(&document, "profile-raw")?;

            let existing = canvas_host.query_selector("canvas").map_err(js_err_from)?;
            let canvas = if let Some(existing) = existing {
                existing
                    .dyn_into::<web_sys::HtmlCanvasElement>()
                    .map_err(|error| {
                        js_error(format!(
                            "element under #gpu-canvas-host is not a canvas: {error:?}"
                        ))
                    })?
            } else {
                let created: web_sys::HtmlCanvasElement = document
                    .create_element("canvas")?
                    .dyn_into()
                    .map_err(js_err_from)?;
                created.set_id("gpu-canvas");
                drop(canvas_host.append_child(&created).map_err(js_err_from)?);
                created
            };

            Ok(HistoryDom {
                rows,
                canvas_host,
                canvas,
                status_live,
                gpu_status,
                gpu_backend,
                gpu_rows_mirror,
                search_counter,
                search_input,
                search_prev,
                search_next,
                profile_activity,
                profile_raw,
                graph_width: Cell::new(0.0),
                graph_label_min_width: Cell::new(None),
            })
        }

        /// The scroll container element.
        pub(crate) fn rows(&self) -> web_sys::HtmlDivElement {
            self.rows.clone()
        }

        /// The current scroll viewport (CSS px).
        pub(crate) fn viewport(&self) -> Viewport {
            Viewport::new(self.scroll_top(), self.client_height())
        }

        /// Current `#rows.scrollTop` as whole CSS pixels.
        pub(crate) fn scroll_top(&self) -> i64 {
            i64::from(self.rows.scroll_top())
        }

        /// Current `#rows.clientHeight` as integer CSS pixels.
        pub(crate) fn client_height(&self) -> i64 {
            f64_round_to_i64(f64::from(self.rows.client_height()))
        }

        /// Current `#rows.clientHeight` as a float (frame math).
        pub(crate) fn client_height_css(&self) -> f64 {
            f64::from(self.rows.client_height())
        }

        /// Current `#rows.clientWidth` as a float (graph budget math).
        pub(crate) fn rows_client_width_css(&self) -> f64 {
            f64::from(self.rows.client_width())
        }

        /// The window's inner width (falls back to `#rows` width).
        pub(crate) fn window_inner_width_css(&self) -> f64 {
            let fallback = f64::from(self.rows.client_width());
            web_sys::window()
                .and_then(|window| window.inner_width().ok())
                .and_then(|value| value.as_f64())
                .filter(|width| *width > 0.0)
                .unwrap_or(fallback)
        }

        /// The device pixel ratio (bounded by the renderer contract).
        pub(crate) fn device_pixel_ratio() -> f64 {
            web_sys::window().map_or(1.0, |window| window.device_pixel_ratio())
        }

        /// The editor background CSS color (frame `background_color`).
        pub(crate) fn editor_background_color() -> String {
            let Some(window) = web_sys::window() else {
                return DEFAULT_EDITOR_BACKGROUND_HEX.to_owned();
            };
            let Some(document) = window.document() else {
                return DEFAULT_EDITOR_BACKGROUND_HEX.to_owned();
            };
            let Some(root) = document.document_element() else {
                return DEFAULT_EDITOR_BACKGROUND_HEX.to_owned();
            };
            let Some(style) = window.get_computed_style(&root).ok().flatten() else {
                return DEFAULT_EDITOR_BACKGROUND_HEX.to_owned();
            };
            let value = style
                .get_property_value("--vscode-editor-background")
                .unwrap_or_default()
                .trim()
                .to_owned();
            if value.is_empty() {
                DEFAULT_EDITOR_BACKGROUND_HEX.to_owned()
            } else {
                value
            }
        }

        /// Apply an integer scroll offset (whole CSS pixels).
        pub(crate) fn set_scroll_top(&self, px: i64) {
            self.rows.set_scroll_top(i32::try_from(px).unwrap_or(0));
        }

        /// Restore the persisted visible top row, clamped to the scroll range
        /// (production `restoreScrollTop`).
        pub(crate) fn restore_scroll_top(&self, row_index: i64, spacer_height_px: i64) {
            let row_h_css = i64_to_f64(ROW_H);
            let client = self.client_height_css();
            let spacer = i64_to_f64(spacer_height_px);
            let max_scroll = (spacer - client).max(0.0);
            let target = i64_to_f64(row_index.max(0)) * row_h_css;
            let scroll = i32::try_from(f64_round_to_i64(target.min(max_scroll))).unwrap_or(0);
            self.rows.set_scroll_top(scroll);
        }

        /// The `.table-wrap` element currently in `#rows`, or `None` when the
        /// table scaffold is absent (loading/error views have no table wrap).
        pub(crate) fn wrap(&self) -> Option<web_sys::HtmlElement> {
            self.rows
                .query_selector(".table-wrap")
                .ok()
                .flatten()
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        }

        /// The absolute index of the last rendered `.row[data-row]`, if any.
        pub(crate) fn last_row_abs(&self) -> Option<i64> {
            let wrap = self.wrap()?;
            let list = wrap.query_selector_all(".row[data-row]").ok()?;
            let mut last_abs = None;
            for index in 0..list.length() {
                let Some(node) = list.item(index) else {
                    continue;
                };
                let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                    continue;
                };
                if let Some(abs) = element.get_attribute("data-row") {
                    if let Ok(parsed) = abs.trim().parse::<i64>() {
                        last_abs = Some(parsed);
                    }
                }
            }
            last_abs
        }

        /// The absolute index of the row rendered directly above `abs`, if
        /// any (used by placeholder fill group-start decisions).
        pub(crate) fn previous_row_abs(&self, abs: i64) -> Option<i64> {
            let selector = format!(".row[data-row=\"{abs}\"]");
            let element = self.rows.query_selector(&selector).ok().flatten()?;
            let previous = element.previous_element_sibling()?;
            previous
                .get_attribute("data-row")
                .and_then(|raw| raw.trim().parse::<i64>().ok())
        }

        /// The absolute indices of currently rendered placeholder rows.
        pub(crate) fn placeholder_abs(&self) -> Vec<i64> {
            let Some(wrap) = self.wrap() else {
                return Vec::new();
            };
            let Ok(list) = wrap.query_selector_all(".row-placeholder[data-row]") else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for index in 0..list.length() {
                let Some(node) = list.item(index) else {
                    continue;
                };
                let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                    continue;
                };
                if let Some(raw) = element.get_attribute("data-row") {
                    if let Ok(parsed) = raw.trim().parse::<i64>() {
                        out.push(parsed);
                    }
                }
            }
            out
        }

        /// Every rendered absolute row index (real or placeholder) outside the
        /// kept visible window `[keep_top, keep_bottom]`. The shell trims by
        /// scanning the DOM (not by re-deriving the pre-step window) so rows
        /// added by a prepend/append during the same transition are removed
        /// too — stale rows would otherwise survive and later prepends would
        /// duplicate them.
        pub(crate) fn rows_outside(&self, keep_top: i64, keep_bottom: i64) -> Vec<i64> {
            let Some(wrap) = self.wrap() else {
                return Vec::new();
            };
            let Ok(list) = wrap.query_selector_all(".row[data-row]") else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for index in 0..list.length() {
                let Some(node) = list.item(index) else {
                    continue;
                };
                let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                    continue;
                };
                if let Some(raw) = element.get_attribute("data-row") {
                    if let Ok(abs) = raw.trim().parse::<i64>() {
                        if abs < keep_top || abs > keep_bottom {
                            out.push(abs);
                        }
                    }
                }
            }
            out
        }

        /// Replace `#rows` with a full-pane message (`showViewMessage`).
        pub(crate) fn show_message(&self, text: &str, error: bool) -> Result<(), JsValue> {
            let document = Self::document()?;
            let message: web_sys::HtmlDivElement = document
                .create_element("div")?
                .dyn_into()
                .map_err(js_err_from)?;
            let class = if error {
                "view-message error"
            } else {
                "view-message"
            };
            message.set_class_name(class);
            message.set_attribute("role", if error { "alert" } else { "status" })?;
            drop(
                message
                    .append_child(&document.create_text_node(text))
                    .map_err(js_err_from)?,
            );
            self.replace_rows_children(&message)
        }

        /// Show the terminal request error with a Retry button; returns the
        /// button so the shell can wire the recovery action.
        pub(crate) fn show_request_error(
            &self,
            text: &str,
        ) -> Result<web_sys::HtmlButtonElement, JsValue> {
            let document = Self::document()?;
            let message: web_sys::HtmlDivElement = document
                .create_element("div")?
                .dyn_into()
                .map_err(js_err_from)?;
            message.set_class_name("view-message error");
            let body: web_sys::HtmlDivElement = document
                .create_element("div")?
                .dyn_into()
                .map_err(js_err_from)?;
            body.set_class_name("request-error-text");
            drop(
                body.append_child(&document.create_text_node(text))
                    .map_err(js_err_from)?,
            );
            let button: web_sys::HtmlButtonElement = document
                .create_element("button")?
                .dyn_into()
                .map_err(js_err_from)?;
            button.set_class_name("retry-btn");
            button.set_attribute("type", "button")?;
            button.set_text_content(Some("Retry"));
            drop(message.append_child(&body).map_err(js_err_from)?);
            drop(message.append_child(&button).map_err(js_err_from)?);
            self.replace_rows_children(&message)?;
            Ok(button)
        }

        /// Replace `#rows` children with `node`.
        fn replace_rows_children(&self, node: &web_sys::HtmlDivElement) -> Result<(), JsValue> {
            self.rows.set_text_content(None);
            drop(self.rows.append_child(node).map_err(js_err_from)?);
            Ok(())
        }

        /// Replace `#rows` children with a document fragment.
        fn replace_rows_children_fragment(
            &self,
            fragment: &web_sys::DocumentFragment,
        ) -> Result<(), JsValue> {
            self.rows.set_text_content(None);
            drop(self.rows.append_child(fragment).map_err(js_err_from)?);
            Ok(())
        }

        /// Rebuild the whole rendered window (`reanchorTo`): optional
        /// warnings/banner chrome, the sticky header, the scroll spacer sized
        /// to the full history, and the positioned row wrap. Preserves the
        /// current scroll offset and restores focus to the previously focused
        /// row (preventScroll) exactly like the production rebuild.
        pub(crate) fn reanchor(
            &self,
            specs: &[RowSpec],
            col_style: &str,
            options: &RebuildOptions,
        ) -> Result<(), JsValue> {
            let RebuildOptions {
                spacer_height_px,
                wrap_top_px,
                aria_rowcount,
                graph_width_css,
                status,
            } = options;
            let document = Self::document()?;
            let previous_scroll = self.rows.scroll_top();
            let focused_abs = Self::focused_row_abs();

            let grid: web_sys::HtmlDivElement = document
                .create_element("div")?
                .dyn_into()
                .map_err(js_err_from)?;
            grid.set_class_name("tbl-grid");
            grid.set_attribute("role", "grid")?;
            grid.set_attribute("aria-label", "History rows")?;
            grid.set_attribute("aria-rowcount", &aria_rowcount.to_string())?;

            let header = build_header(self, &document, col_style, *graph_width_css)?;
            drop(grid.append_child(&header).map_err(js_err_from)?);

            let spacer: web_sys::HtmlDivElement = document
                .create_element("div")?
                .dyn_into()
                .map_err(js_err_from)?;
            spacer.set_class_name("scroll-spacer");
            spacer.set_attribute("role", "presentation")?;
            spacer
                .style()
                .set_property("height", &format!("{spacer_height_px}px"))?;

            let wrap: web_sys::HtmlDivElement = document
                .create_element("div")?
                .dyn_into()
                .map_err(js_err_from)?;
            wrap.set_class_name("table-wrap");
            wrap.set_attribute("role", "presentation")?;
            wrap.set_attribute("style", col_style)?;
            wrap.style()
                .set_property("top", &format!("{wrap_top_px}px"))?;

            for spec in specs {
                let row = build_row(&document, spec, col_style)?;
                drop(wrap.append_child(&row).map_err(js_err_from)?);
            }
            drop(spacer.append_child(&wrap).map_err(js_err_from)?);
            drop(grid.append_child(&spacer).map_err(js_err_from)?);

            let fragment = document.create_document_fragment();
            if !status.search_mode && !status.open_warnings.is_empty() {
                let warnings: web_sys::HtmlDivElement = document
                    .create_element("div")?
                    .dyn_into()
                    .map_err(js_err_from)?;
                warnings.set_class_name("open-warning");
                warnings.set_attribute("role", "status")?;
                for warning in &status.open_warnings {
                    let line: web_sys::HtmlDivElement = document
                        .create_element("div")?
                        .dyn_into()
                        .map_err(js_err_from)?;
                    line.set_class_name("open-warning-line");
                    line.set_text_content(Some(warning));
                    drop(warnings.append_child(&line).map_err(js_err_from)?);
                }
                drop(fragment.append_child(&warnings).map_err(js_err_from)?);
            }
            if status.search_mode {
                let banner: web_sys::HtmlDivElement = document
                    .create_element("div")?
                    .dyn_into()
                    .map_err(js_err_from)?;
                banner.set_class_name("search-banner");
                banner.set_attribute("role", "status")?;
                banner.set_attribute("aria-live", "polite")?;
                let plural = if status.total == 1 { "" } else { "s" };
                banner.set_text_content(Some(&format!(
                    "{} result{} for \"{}\"",
                    status.total, plural, status.search_query
                )));
                drop(fragment.append_child(&banner).map_err(js_err_from)?);
            }
            drop(fragment.append_child(&grid).map_err(js_err_from)?);
            self.replace_rows_children_fragment(&fragment)?;
            self.rows.set_scroll_top(previous_scroll);
            if let Some(abs) = focused_abs {
                let selector = format!(".row[data-row=\"{abs}\"]");
                if let Some(restored) = self.rows.query_selector(&selector).map_err(js_err_from)? {
                    if let Ok(restored) = restored.dyn_into::<web_sys::HtmlElement>() {
                        let options = web_sys::FocusOptions::new();
                        options.set_prevent_scroll(true);
                        drop(restored.focus_with_options(&options));
                    }
                }
            }
            Ok(())
        }

        /// Append rows below the rendered window (production
        /// `appendRowsBelow`). The specs must already carry the correct
        /// group-start decisions (the shell seeds them from the last rendered
        /// row's group).
        pub(crate) fn append_rows(
            &self,
            specs: &[RowSpec],
            col_style: &str,
        ) -> Result<(), JsValue> {
            let Some(wrap) = self.wrap() else {
                return Ok(());
            };
            let document = Self::document()?;
            for spec in specs {
                let row = build_row(&document, spec, col_style)?;
                let node = node_of(&row)?;
                drop(wrap.append_child(&node).map_err(js_err_from)?);
            }
            Ok(())
        }

        /// Prepend rows above the rendered window and shift the wrap down by
        /// the number of rows actually added (production `prependRowsAbove`).
        pub(crate) fn prepend_rows(
            &self,
            specs: &[RowSpec],
            col_style: &str,
            wrap_top_px: i64,
        ) -> Result<(), JsValue> {
            let Some(wrap) = self.wrap() else {
                return Ok(());
            };
            let document = Self::document()?;
            let mut anchor: Option<web_sys::Node> = wrap.first_child();
            for spec in specs {
                let row = build_row(&document, spec, col_style)?;
                let node = node_of(&row)?;
                drop(
                    wrap.insert_before(&node, anchor.as_ref())
                        .map_err(js_err_from)?,
                );
                anchor = Some(node);
            }
            wrap.style()
                .set_property("top", &format!("{wrap_top_px}px"))?;
            Ok(())
        }

        /// Replace the row element for an absolute index (placeholder fill and
        /// the prepend boundary re-evaluation share this path).
        pub(crate) fn replace_row_abs(
            &self,
            abs: i64,
            spec: &RowSpec,
            col_style: &str,
        ) -> Result<(), JsValue> {
            let Some(wrap) = self.wrap() else {
                return Ok(());
            };
            let selector = format!(".row[data-row=\"{abs}\"]");
            let Some(old) = wrap.query_selector(&selector).map_err(js_err_from)? else {
                return Ok(());
            };
            let document = Self::document()?;
            let new = build_row(&document, spec, col_style)?;
            let parent = old
                .parent_node()
                .ok_or_else(|| js_error("row has no parent"))?;
            drop(
                parent
                    .replace_child(&new_node(&new)?, &old)
                    .map_err(js_err_from)?,
            );
            Ok(())
        }

        /// Remove the rendered rows for the given absolute indices (trim ops).
        pub(crate) fn remove_abs(&self, abs_list: &[i64]) -> Result<(), JsValue> {
            let Some(wrap) = self.wrap() else {
                return Ok(());
            };
            for abs in abs_list {
                let selector = format!(".row[data-row=\"{abs}\"]");
                if let Some(element) = wrap.query_selector(&selector).map_err(js_err_from)? {
                    element.remove();
                }
            }
            Ok(())
        }

        /// Rebuild just the sticky header in place (`refreshHeader`).
        pub(crate) fn refresh_header(
            &self,
            col_style: &str,
            graph_width_css: f64,
        ) -> Result<(), JsValue> {
            let document = Self::document()?;
            let Some(old) = self
                .rows
                .query_selector(".tbl-header")
                .map_err(js_err_from)?
            else {
                return Ok(());
            };
            let parent = old
                .parent_node()
                .ok_or_else(|| js_error("header has no parent"))?;
            let new = build_header(self, &document, col_style, graph_width_css)?;
            drop(parent.replace_child(&new, &old).map_err(js_err_from)?);
            Ok(())
        }

        /// Position the canvas host over the graph column and size it to the
        /// rows viewport (production `positionOverlay`; in the Rust page the
        /// graph column is the first grid track at the layout's left edge).
        /// Also records the effective graph width for the header-label
        /// accessibility decision.
        pub(crate) fn position_canvas_host(&self, graph_width_css: f64) -> Result<(), JsValue> {
            self.graph_width.set(graph_width_css);
            let style = self.canvas_host.style();
            style.set_property("left", "0px")?;
            style.set_property("top", "0px")?;
            style.set_property("width", &format!("{graph_width_css}px"))?;
            style.set_property("height", &format!("{}px", self.client_height()))?;
            Ok(())
        }

        /// Set the canvas backing-store dimensions.
        pub(crate) fn set_canvas_backing(&self, width: u32, height: u32) {
            self.canvas.set_width(width);
            self.canvas.set_height(height);
        }

        /// Replace the hidden `#gpu-rows` mirror with `[data-row][data-key]`
        /// markers for the frame's rows (harness/e2e DOM contract).
        pub(crate) fn mirror_rows(&self, rows: &[FrameRow]) -> Result<(), JsValue> {
            let document = Self::document()?;
            let fragment = document.create_document_fragment();
            for row in rows {
                let element = document.create_element("div")?;
                element.set_class_name("gpu-row");
                element.set_attribute("data-row", &row.index.to_string())?;
                element.set_attribute("data-key", &row.key)?;
                drop(fragment.append_child(&element).map_err(js_err_from)?);
            }
            self.gpu_rows_mirror.set_text_content(None);
            drop(
                self.gpu_rows_mirror
                    .append_child(&fragment)
                    .map_err(js_err_from)?,
            );
            Ok(())
        }

        /// Update the status surfaces (`#gpu-status` and the live region).
        pub(crate) fn set_status(&self, text: &str) {
            self.gpu_status.set_text_content(Some(text));
            self.status_live.set_text_content(Some(text));
        }

        /// Update only the accessible live region (production `announce`).
        pub(crate) fn announce(&self, text: &str) {
            self.status_live.set_text_content(Some(text));
        }

        /// The search input element (event wiring).
        pub(crate) fn search_input(&self) -> web_sys::HtmlInputElement {
            self.search_input.clone()
        }

        /// The current trimmed search input value.
        pub(crate) fn search_input_value(&self) -> String {
            self.search_input.value().trim().to_owned()
        }

        /// `updateFindCounter` — the exact Pending/zero/error/settled/hidden
        /// counter state: class, text, `aria-label`, `aria-busy`, and `title`.
        pub(crate) fn set_find_counter_state(&self, state: &FindCounterState) {
            let element = &self.search_counter;
            for class in [
                "search-counter-pending",
                "search-counter-zero",
                "search-counter-error",
            ] {
                drop(element.class_list().remove_1(class));
            }
            element.set_text_content(Some(&HistoryAppState::find_counter_text(state)));
            match state {
                FindCounterState::Pending => {
                    drop(element.class_list().add_1("search-counter-pending"));
                    drop(element.set_attribute("aria-label", "Searching…"));
                    drop(element.set_attribute("aria-busy", "true"));
                    drop(element.remove_attribute("title"));
                }
                FindCounterState::Zero => {
                    drop(element.class_list().add_1("search-counter-zero"));
                    drop(element.remove_attribute("aria-label"));
                    drop(element.remove_attribute("aria-busy"));
                    drop(element.remove_attribute("title"));
                }
                FindCounterState::Error(detail) => {
                    drop(element.class_list().add_1("search-counter-error"));
                    drop(element.set_attribute("aria-label", &format!("Find failed: {detail}")));
                    drop(element.remove_attribute("aria-busy"));
                    drop(element.set_attribute("title", detail.as_str()));
                }
                FindCounterState::Settled { .. } | FindCounterState::Hidden => {
                    drop(element.remove_attribute("aria-label"));
                    drop(element.remove_attribute("aria-busy"));
                    drop(element.remove_attribute("title"));
                }
            }
        }

        /// `syncFindNavButtons` — Previous/Next are collapsed (hidden +
        /// disabled) unless a settled, navigable session exists.
        pub(crate) fn set_find_nav(&self, enabled: bool) {
            for button in [&self.search_prev, &self.search_next] {
                button.set_hidden(!enabled);
                button.set_disabled(!enabled);
            }
        }

        /// The previous-match nav button element (event wiring).
        pub(crate) fn search_prev_button(&self) -> web_sys::HtmlButtonElement {
            self.search_prev.clone()
        }

        /// The next-match nav button element (event wiring).
        pub(crate) fn search_next_button(&self) -> web_sys::HtmlButtonElement {
            self.search_next.clone()
        }

        /// `revealRow` — the minimal header-aware scroll so the target row is
        /// fully visible and never hidden under the sticky header. No scroll
        /// happens when the row is already visible.
        pub(crate) fn reveal_row(&self, abs: i64) -> Result<(), JsValue> {
            let selector = format!(".row[data-row=\"{abs}\"]");
            let Some(row) = self.rows.query_selector(&selector).map_err(js_err_from)? else {
                return Ok(());
            };
            let viewport = self.rows.get_bounding_client_rect();
            let header_height = self
                .rows
                .query_selector(".tbl-header")
                .ok()
                .flatten()
                .map_or(0.0, |header| header.get_bounding_client_rect().height());
            let rect = row.get_bounding_client_rect();
            let top = rect.top() - viewport.top();
            let bottom = rect.bottom() - viewport.top();
            let current = f64::from(self.rows.scroll_top());
            let height = f64::from(self.rows.client_height());
            if top < header_height {
                let next = f64_round_to_i64(current - (header_height - top));
                self.set_scroll_top(next);
            } else if bottom > height {
                let next = f64_round_to_i64(current + (bottom - height));
                self.set_scroll_top(next);
            }
            Ok(())
        }

        /// Mark `abs` as the current find-in-chain row in the DOM (the
        /// `row-find-current` accent class; selection state is the shell's
        /// responsibility).
        pub(crate) fn set_find_highlight(&self, abs: i64) -> Result<(), JsValue> {
            let previous = self
                .rows
                .query_selector(".row-find-current")
                .map_err(js_err_from)?;
            if let Some(previous) = previous {
                let is_target = previous
                    .get_attribute("data-row")
                    .as_deref()
                    .is_some_and(|raw| raw == abs.to_string());
                if !is_target {
                    drop(previous.class_list().remove_1("row-find-current"));
                }
            }
            let selector = format!(".row[data-row=\"{abs}\"]");
            if let Some(current) = self.rows.query_selector(&selector).map_err(js_err_from)? {
                drop(current.class_list().add_1("row-find-current"));
            }
            Ok(())
        }

        /// Remove every `row-find-current` marker from the rendered rows.
        pub(crate) fn clear_find_highlight(&self) -> Result<(), JsValue> {
            if let Some(current) = self
                .rows
                .query_selector(".row-find-current")
                .map_err(js_err_from)?
            {
                drop(current.class_list().remove_1("row-find-current"));
            }
            Ok(())
        }

        /// Re-apply the inline selection to the rendered DOM: exactly one
        /// `.row-selected`/`aria-selected=true` for `abs`.
        pub(crate) fn apply_selection(&self, abs: i64) -> Result<(), JsValue> {
            let target = abs.to_string();
            let list = self
                .rows
                .query_selector_all(".row.row-selected")
                .map_err(js_err_from)?;
            for index in 0..list.length() {
                let Some(node) = list.item(index) else {
                    continue;
                };
                let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                    continue;
                };
                let is_target = element
                    .get_attribute("data-row")
                    .as_deref()
                    .is_some_and(|raw| raw == target);
                if !is_target {
                    drop(element.class_list().remove_1("row-selected"));
                    drop(element.set_attribute("aria-selected", "false"));
                }
            }
            let selector = format!(".row[data-row=\"{target}\"]");
            if let Some(current) = self.rows.query_selector(&selector).map_err(js_err_from)? {
                drop(current.class_list().add_1("row-selected"));
                drop(current.set_attribute("aria-selected", "true"));
            }
            Ok(())
        }

        /// Remove every `.row-selected`/`aria-selected=true` marker.
        pub(crate) fn clear_selection_ui(&self) -> Result<(), JsValue> {
            if let Some(previous) = self
                .rows
                .query_selector(".row.row-selected")
                .map_err(js_err_from)?
            {
                drop(previous.class_list().remove_1("row-selected"));
                drop(previous.set_attribute("aria-selected", "false"));
            }
            Ok(())
        }

        /// The absolute index of the currently focused rendered row, if any
        /// (used to restore focus across full rebuilds like `reanchorTo`).
        fn focused_row_abs() -> Option<i64> {
            let active = web_sys::window()?.document()?.active_element()?;
            let row = active.closest(".row").ok().flatten()?;
            row.get_attribute("data-row")?.trim().parse::<i64>().ok()
        }

        /// The rendered header cell's on-screen width for a column (the drag
        /// start position for non-graph columns, production `currentColumnWidth`).
        pub(crate) fn header_cell_width(&self, col: ColKey) -> f64 {
            let selector = format!(".tbl-header .th.{}", col.as_str());
            self.rows
                .query_selector(&selector)
                .ok()
                .flatten()
                .and_then(|cell| cell.dyn_into::<web_sys::HtmlElement>().ok())
                .map_or(0.0, |cell| f64::from(cell.offset_width()))
        }

        /// `graphLabelMinW` — measure the narrowest graph column that renders
        /// the "Graph" columnheader label unclipped (once, from a detached
        /// probe that mirrors the header cell exactly).
        fn graph_label_min_width(&self) -> f64 {
            if let Some(measured) = self.graph_label_min_width.get() {
                return measured;
            }
            let measured = Self::document().ok().and_then(|document| {
                let probe = document.create_element("div").ok()?;
                probe.set_class_name("th graph");
                let style = probe.dyn_ref::<web_sys::HtmlElement>()?.style();
                drop(style.set_property("position", "absolute"));
                drop(style.set_property("visibility", "hidden"));
                drop(style.set_property("left", "-9999px"));
                drop(style.set_property("top", "0px"));
                drop(style.set_property("width", "auto"));
                drop(style.set_property("padding", "6px 8px"));
                drop(style.set_property("box-sizing", "border-box"));
                drop(style.set_property("font-weight", "700"));
                drop(style.set_property("white-space", "nowrap"));
                probe.set_text_content(Some("Graph"));
                let body = document.body()?;
                drop(body.append_child(&probe));
                let width = f64::from(probe.scroll_width());
                probe.remove();
                Some(width.max(1.0))
            });
            let measured = measured.unwrap_or(65.0);
            self.graph_label_min_width.set(Some(measured));
            measured
        }

        /// Create the draggable column-resize handles after a full rebuild
        /// (production `setupColumnResizeHandles`): one handle per visible
        /// column boundary, positioned at the header cell's right edge.
        pub(crate) fn install_resize_handles(
            &self,
            window_inner_width: f64,
        ) -> Result<(), JsValue> {
            let Some(wrap) = self.wrap() else {
                return Ok(());
            };
            let document = Self::document()?;
            let list = wrap
                .query_selector_all(".col-resize-handle")
                .map_err(js_err_from)?;
            for index in 0..list.length() {
                let item = list.item(index);
                let Some(node) = item else {
                    continue;
                };
                if let Some(handle) = node.dyn_ref::<web_sys::Element>() {
                    handle.remove();
                }
            }
            let Some(header) = self
                .rows
                .query_selector(".tbl-header")
                .map_err(js_err_from)?
            else {
                return Ok(());
            };
            let hidden = super::hidden_columns(window_inner_width);
            let mut columns = vec![ColKey::Graph];
            if !hidden.contains(&ColKey::Content) {
                columns.push(ColKey::Content);
            }
            if !hidden.contains(&ColKey::Date) {
                columns.push(ColKey::Date);
            }
            let wrap_width = f64::from(wrap.client_width());
            for col in columns {
                let selector = format!(".th.{}", col.as_str());
                let Some(cell) = header.query_selector(&selector).map_err(js_err_from)? else {
                    continue;
                };
                let Some(cell) = cell.dyn_into::<web_sys::HtmlElement>().ok() else {
                    continue;
                };
                let boundary = f64::from(cell.offset_left()) + f64::from(cell.offset_width());
                let handle: web_sys::HtmlDivElement = document
                    .create_element("div")?
                    .dyn_into()
                    .map_err(js_err_from)?;
                handle.set_class_name("col-resize-handle");
                handle.set_attribute("data-col", col.as_str())?;
                handle
                    .set_attribute("title", &format!("Drag to resize {} column", col.as_str()))?;
                let left = (boundary - 3.0).min((wrap_width - 6.0).max(0.0));
                handle.style().set_property("left", &format!("{left}px"))?;
                drop(wrap.append_child(&handle).map_err(js_err_from)?);
            }
            Ok(())
        }

        /// Whether the graph column label ("Graph") fits without clipping at
        /// the EFFECTIVE column width; used by the narrow-header
        /// accessibility decision. The width is passed in (not read from the
        /// renderer cell) so a header rebuilt before the first frame render
        /// still decides on the real layout.
        pub(crate) fn graph_label_fits(&self, graph_width_css: f64) -> bool {
            graph_width_css >= self.graph_label_min_width()
        }

        /// Update the backend badge (text + `data-backend`).
        pub(crate) fn set_backend(&self, backend: &str, label: &str) {
            self.gpu_backend.set_text_content(Some(label));
            drop(self.gpu_backend.set_attribute("data-backend", backend));
        }

        /// Toggle the segmented Activity/Raw profile buttons.
        pub(crate) fn set_profile_ui(&self, profile: Profile) {
            let activity_active = matches!(profile, Profile::Activity);
            Self::set_segmented_button(&self.profile_activity, activity_active);
            Self::set_segmented_button(&self.profile_raw, !activity_active);
        }

        /// The activity profile button element (event wiring).
        pub(crate) fn profile_activity_button(&self) -> web_sys::HtmlButtonElement {
            self.profile_activity.clone()
        }

        /// The raw profile button element (event wiring).
        pub(crate) fn profile_raw_button(&self) -> web_sys::HtmlButtonElement {
            self.profile_raw.clone()
        }

        fn document() -> Result<web_sys::Document, JsValue> {
            web_sys::window()
                .and_then(|window| window.document())
                .ok_or_else(|| js_error("browser document is unavailable"))
        }

        fn set_segmented_button(button: &web_sys::HtmlButtonElement, active: bool) {
            if active {
                drop(button.class_list().add_1("active"));
            } else {
                drop(button.class_list().remove_1("active"));
            }
            drop(button.set_attribute("aria-pressed", if active { "true" } else { "false" }));
        }
    }

    /// Convert a row element into its `Node` handle for replacement.
    fn new_node(element: &web_sys::HtmlDivElement) -> Result<web_sys::Node, JsValue> {
        element
            .clone()
            .dyn_into::<web_sys::Node>()
            .map_err(js_err_from)
    }

    /// Build the sticky `.tbl-header` row element. The Graph columnheader
    /// label renders as visually-hidden text when the rail is too narrow to
    /// fit it unclipped (narrow-header accessibility, `graphColumnHeaderLabel`).
    fn build_header(
        dom: &HistoryDom,
        document: &web_sys::Document,
        col_style: &str,
        graph_width_css: f64,
    ) -> Result<web_sys::HtmlDivElement, JsValue> {
        let header: web_sys::HtmlDivElement = document
            .create_element("div")?
            .dyn_into()
            .map_err(js_err_from)?;
        header.set_class_name("tbl-header");
        header.set_attribute("role", "row")?;
        header.set_attribute("style", col_style)?;
        let columns = [
            ("graph", Some("Graph")),
            ("content", Some("Content")),
            ("date", Some("Date")),
        ];
        for (class, label) in columns {
            let cell: web_sys::HtmlDivElement = document
                .create_element("div")?
                .dyn_into()
                .map_err(js_err_from)?;
            cell.set_class_name(&format!("th {class}"));
            cell.set_attribute("role", "columnheader")?;
            let label = label.unwrap_or_default();
            if class == "graph" && !dom.graph_label_fits(graph_width_css) {
                let span = make_element(document, "span", "visually-hidden", Some("Graph"))?;
                drop(cell.append_child(&node_of(&span)?).map_err(js_err_from)?);
            } else {
                drop(
                    cell.append_child(&document.create_text_node(label))
                        .map_err(js_err_from)?,
                );
            }
            drop(header.append_child(&cell).map_err(js_err_from)?);
        }
        Ok(header)
    }

    /// Create an element with a class name and optional text content.
    fn make_element(
        document: &web_sys::Document,
        tag: &str,
        class: &str,
        text: Option<&str>,
    ) -> Result<web_sys::HtmlElement, JsValue> {
        let element = document.create_element(tag)?;
        element.set_class_name(class);
        if let Some(text) = text {
            element.set_text_content(Some(text));
        }
        element
            .dyn_into::<web_sys::HtmlElement>()
            .map_err(js_err_from)
    }

    /// Build one `.row` element from its spec (production `buildRowHtml`).
    pub(crate) fn build_row(
        document: &web_sys::Document,
        spec: &RowSpec,
        col_style: &str,
    ) -> Result<web_sys::HtmlDivElement, JsValue> {
        let row: web_sys::HtmlDivElement = document
            .create_element("div")?
            .dyn_into()
            .map_err(js_err_from)?;
        let class = if spec.placeholder {
            RowSpec::PLACEHOLDER_CLASSES.to_owned()
        } else {
            spec.classes()
        };
        row.set_class_name(&class);
        row.set_attribute("data-row", &spec.identity.abs_index.to_string())?;
        if spec.placeholder {
            return Ok(row);
        }
        row.set_attribute("style", col_style)?;
        row.set_attribute("role", "row")?;
        row.set_attribute("tabindex", &spec.aria.tabindex.to_string())?;
        let selected = spec.aria.aria_selected.to_string();
        row.set_attribute("aria-selected", &selected)?;
        if let Some(expanded) = spec.aria.aria_expanded {
            let expanded_text = expanded.to_string();
            row.set_attribute("aria-expanded", &expanded_text)?;
        }
        row.set_attribute("aria-label", &spec.aria.aria_label)?;
        row.set_attribute("title", &spec.aria.title)?;
        row.set_attribute("data-base-aria-label", &spec.aria.base_aria_label)?;
        row.set_attribute("data-key", &spec.identity.node_key)?;
        if let Some(header) = &spec.work_unit_header {
            row.set_attribute("data-work-unit-id", &header.id)?;
        }
        if let Some(bundle) = &spec.bundle {
            row.set_attribute("data-activity-bundle", bundle.kind.as_str())?;
            if let Some(count) = bundle.member_count {
                row.set_attribute("data-bundle-count", &count.to_string())?;
            }
        }

        if let Some(label) = &spec.group_label {
            let chip = make_element(document, "div", "group-label", Some(label))?;
            chip.set_attribute("aria-hidden", "true")?;
            drop(row.append_child(&chip).map_err(js_err_from)?);
        }

        let graph_cell = make_element(document, "div", "graph-cell", None)?;
        graph_cell.set_attribute("role", "gridcell")?;
        drop(row.append_child(&graph_cell).map_err(js_err_from)?);

        let text_cell = make_element(document, "div", "text-cell", None)?;
        text_cell.set_attribute("role", "gridcell")?;
        let mut summary_class = String::from("summary");
        if spec.content_flags.work_unit_block {
            summary_class.push_str(" work-unit-block");
        }
        if spec.content_flags.work_unit_title_only {
            summary_class.push_str(" work-unit-title-only");
        }
        let summary = make_element(document, "div", &summary_class, None)?;
        summary.set_attribute("title", &spec.detail_summary)?;
        append_content(document, &summary, spec)?;
        drop(text_cell.append_child(&summary).map_err(js_err_from)?);
        drop(row.append_child(&text_cell).map_err(js_err_from)?);

        let date_cell = make_element(document, "div", "date-cell", Some(&spec.date_text))?;
        date_cell.set_attribute("role", "gridcell")?;
        if !spec.date_text.is_empty() {
            date_cell.set_attribute("title", &spec.date_text)?;
        }
        drop(row.append_child(&date_cell).map_err(js_err_from)?);

        let author_cell = make_element(document, "div", "author-cell", Some(&spec.author_text))?;
        author_cell.set_attribute("role", "gridcell")?;
        if !spec.author_text.is_empty() {
            author_cell.set_attribute("title", &spec.author_text)?;
        }
        drop(row.append_child(&author_cell).map_err(js_err_from)?);

        let commit_cell = make_element(document, "div", "commit-cell", Some(&spec.commit_text))?;
        commit_cell.set_attribute("role", "gridcell")?;
        commit_cell.set_attribute("title", &spec.commit_title)?;
        drop(row.append_child(&commit_cell).map_err(js_err_from)?);

        Ok(row)
    }

    /// Build the content-cell children (chevron, session chips, chrome,
    /// summary, work-unit ribbon, sub-op line) as DOM/text nodes.
    fn append_content(
        document: &web_sys::Document,
        parent: &web_sys::HtmlElement,
        spec: &RowSpec,
    ) -> Result<(), JsValue> {
        let parent_node = node_of(parent)?;
        if let Some(subop) = &spec.content.subop {
            let icon = make_element(
                document,
                "span",
                &format!("subop-icon codicon codicon-{}", subop.icon),
                None,
            )?;
            icon.set_attribute("aria-hidden", "true")?;
            let icon_node = node_of(&icon)?;
            drop(parent_node.append_child(&icon_node).map_err(js_err_from)?);
            let text = make_element(document, "span", "subop-summary", Some(&spec.plain_summary))?;
            let text_node = node_of(&text)?;
            drop(parent_node.append_child(&text_node).map_err(js_err_from)?);
            return Ok(());
        }
        let top = spec
            .content
            .top
            .as_ref()
            .ok_or_else(|| js_error("top-level row has no content"))?;
        let wu_start = spec.content_flags.work_unit_block;

        // Work-unit starts render their compact unit header in a separate
        // ribbon line ABOVE the row's own content line, exactly like
        // production's buildRowHtml; title-only units omit the row line.
        let content_target: Option<web_sys::HtmlElement> =
            if wu_start && !spec.content_flags.work_unit_title_only {
                let line = make_element(document, "span", "work-unit-row-line", None)?;
                drop(parent_node.append_child(&line).map_err(js_err_from)?);
                Some(line)
            } else {
                None
            };
        let content_node = if let Some(line) = &content_target {
            node_of(line)?
        } else {
            parent_node.clone()
        };

        if let Some(disclosure) = &top.chevron {
            let button: web_sys::HtmlButtonElement = document
                .create_element("button")?
                .dyn_into()
                .map_err(js_err_from)?;
            button.set_class_name("subop-chevron");
            button.set_attribute("type", "button")?;
            button.set_attribute("title", &disclosure.label)?;
            button.set_attribute("aria-label", &disclosure.label)?;
            let expanded_text = disclosure.expanded.to_string();
            button.set_attribute("aria-expanded", &expanded_text)?;
            button.set_text_content(Some(if disclosure.expanded {
                "\u{25be}"
            } else {
                "\u{25b8}"
            }));
            let button_node = node_of(
                &button
                    .dyn_into::<web_sys::HtmlElement>()
                    .map_err(js_err_from)?,
            )?;
            drop(
                content_node
                    .append_child(&button_node)
                    .map_err(js_err_from)?,
            );
        }
        if !wu_start {
            append_session_slot(document, &content_node, spec, spec.state.group_start)?;
        }
        if !top.chrome.is_empty() {
            let meta = make_element(document, "span", "row-meta", None)?;
            let meta_node = node_of(&meta)?;
            for item in &top.chrome {
                let badge = make_element(document, "span", &item.classes, Some(&item.text))?;
                badge.set_attribute("title", &item.title)?;
                if let Some(aria) = &item.aria_label {
                    badge.set_attribute("aria-label", aria)?;
                }
                let badge_node = node_of(&badge)?;
                drop(meta_node.append_child(&badge_node).map_err(js_err_from)?);
            }
            drop(content_node.append_child(&meta_node).map_err(js_err_from)?);
        }
        if let Some(summary) = &top.summary {
            append_row_summary(document, &content_node, spec, summary)?;
        }
        if wu_start {
            let header = spec
                .work_unit_header
                .as_ref()
                .ok_or_else(|| js_error("work-unit start has no header"))?;
            let ribbon_line = make_element(document, "span", "work-unit-ribbon-line", None)?;
            let ribbon_node = node_of(&ribbon_line)?;
            drop(
                parent_node
                    .append_child(&ribbon_node)
                    .map_err(js_err_from)?,
            );
            let title_span =
                make_element(document, "span", "work-unit-ribbon", Some(&header.title))?;
            title_span.set_attribute("title", &header.title)?;
            let title_node = node_of(&title_span)?;
            drop(ribbon_node.append_child(&title_node).map_err(js_err_from)?);
            append_session_slot(document, &ribbon_node, spec, header.session_chips)?;
            if header.show_count {
                let count = make_element(
                    document,
                    "span",
                    "work-unit-count",
                    Some(&header.count_text),
                )?;
                count.set_attribute("title", &header.count_title)?;
                let count_node = node_of(&count)?;
                drop(ribbon_node.append_child(&count_node).map_err(js_err_from)?);
            }
        }
        Ok(())
    }

    /// Append the session-meta slot (chips only at the group boundary).
    fn append_session_slot(
        document: &web_sys::Document,
        parent: &web_sys::Node,
        spec: &RowSpec,
        at_group_start: bool,
    ) -> Result<(), JsValue> {
        if spec.session.is_empty() {
            return Ok(());
        }
        let slot = make_element(document, "span", "session-meta-slot", None)?;
        let slot_node = node_of(&slot)?;
        if at_group_start {
            for chip in &spec.session {
                let label = format!("{}: {}", chip.title, chip.label);
                let chip_element = make_element(document, "span", &chip.class, Some(&chip.label))?;
                chip_element.set_attribute("title", &label)?;
                chip_element.set_attribute("aria-label", &label)?;
                let chip_node = node_of(&chip_element)?;
                drop(slot_node.append_child(&chip_node).map_err(js_err_from)?);
            }
        }
        drop(parent.append_child(&slot_node).map_err(js_err_from)?);
        Ok(())
    }

    /// Append the summary text (Git prefix chip + summary span).
    fn append_row_summary(
        document: &web_sys::Document,
        parent: &web_sys::Node,
        spec: &RowSpec,
        summary: &RowSummary,
    ) -> Result<(), JsValue> {
        if let Some(prefix) = &summary.git_prefix {
            let label = format!("Commit prefix: {prefix}");
            let chip = make_element(document, "span", "git-prefix-chip", Some(prefix))?;
            chip.set_attribute("title", &label)?;
            chip.set_attribute("aria-label", &label)?;
            let chip_node = node_of(&chip)?;
            drop(parent.append_child(&chip_node).map_err(js_err_from)?);
        }
        let mut class = String::from("summary-text");
        if summary.git_prefix.is_some() {
            class.push_str(" git-summary-text");
        }
        let text = make_element(document, "span", &class, Some(&spec.plain_summary))?;
        let text_node = node_of(&text)?;
        drop(parent.append_child(&text_node).map_err(js_err_from)?);
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) use web::*;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row_value(group: &str, key: &str) -> Value {
        json!({
            "node_key": key,
            "summary": "summary text",
            "group": group,
            "kind": "message",
            "lane": 0,
            "above": [],
            "below": [],
            "transitions": [],
            "sub_ops": [],
            "is_subop": false,
            "timestamp_ms": 1_768_492_800_000_i64,
            "author": "",
            "commit_id": "",
            "op_id": null,
            "git_oid": null,
            "repository": null,
            "record_role": "narrative",
            "activity_kind": "conversation",
            "visibility": "primary",
            "outcome": "unknown",
            "turn_id": "",
        })
    }

    fn cache_state(rows: &[Value], total: i64) -> HistoryAppState {
        let mut state = HistoryAppState {
            total: Some(total),
            ..HistoryAppState::default()
        };
        for (index, value) in rows.iter().enumerate() {
            let key = i64::try_from(index).unwrap_or(0);
            drop(state.cache.insert(key, value.clone()));
        }
        state
    }

    #[test]
    fn pulse_pitch_is_exactly_14_76_css_px() {
        let pitch = LANE_W * LANE_W_PULSE_SCALE;
        assert!(
            (pitch - PULSE_LANE_PITCH).abs() < 1e-12,
            "pitch is the exact Pulse constant"
        );
        assert!(
            (PULSE_LANE_PITCH - 14.76).abs() < 1e-12,
            "Pulse pitch is exactly 14.76 px"
        );
    }

    #[test]
    fn lane_positions_are_fixed_and_never_rescale_with_column_width() {
        let wide = graph_layout(1, 1440.0, 1440.0);
        assert_eq!(wide.lane_x.len(), 2, "lanes 0..=max_lane are present");
        assert!(
            (wide.lane_x.first().copied().unwrap_or(0.0) - 14.76).abs() < 1e-9,
            "lane 0 center is 14.76"
        );
        assert!(
            (wide.lane_x.get(1).copied().unwrap_or(0.0) - 29.52).abs() < 1e-9,
            "lane 1 center is 29.52"
        );
        assert!(
            (wide.column_width - 44.28).abs() < 1e-9,
            "2-lane natural width is 44.28"
        );
        let narrow = graph_layout(1, 300.0, 300.0);
        assert_eq!(narrow.lane_x, wide.lane_x, "resize never rescales lane X");
        assert!(
            (narrow.column_width - wide.column_width).abs() < 1e-9,
            "column width is lane-driven, never viewport-driven"
        );
    }

    #[test]
    fn dense_lanes_compress_inside_the_budget_without_moving_fixed_columns() {
        let layout = graph_layout(40, 1440.0, 1440.0);
        assert!(
            layout.column_width <= 720.0,
            "graph never exceeds half the viewport"
        );
        assert_eq!(layout.lane_x.len(), 41, "every lane has a position");
        assert!(
            layout.lane_x.windows(2).all(|pair| {
                pair.get(1).copied().unwrap_or(0.0) > pair.first().copied().unwrap_or(0.0)
            }),
            "lane centers stay monotonic under compression"
        );
    }

    #[test]
    fn window_rows_marks_group_starts_and_maps_visible_to_absolute() {
        let rows = vec![
            row_value("repo:a", "k0"),
            row_value("repo:a", "k1"),
            row_value("repo:b", "k2"),
        ];
        let state = cache_state(&rows, 3);
        let planned = window_rows(&state, 0, 2);
        assert_eq!(planned.len(), 3, "all three visible rows planned");
        let first = planned.first().expect("first row");
        assert!(first.spec.state.group_start, "first row starts repo:a");
        assert_eq!(first.vis, 0, "visible index 0");
        assert_eq!(first.spec.identity.abs_index, 0, "absolute index 0");
        let second = planned.get(1).expect("second row");
        assert!(!second.spec.state.group_start, "second row stays in repo:a");
        let third = planned.get(2).expect("third row");
        assert!(third.spec.state.group_start, "repo:b starts a new group");
    }

    #[test]
    fn window_rows_emits_placeholders_for_uncached_rows() {
        let state = cache_state(&[row_value("repo:a", "k0")], 5);
        let planned = window_rows(&state, 0, 4);
        assert_eq!(planned.len(), 5, "window covers the full range");
        let first = planned.first().expect("first row");
        assert!(!first.spec.placeholder, "row 0 is cached");
        let second = planned.get(1).expect("second row");
        assert!(second.spec.placeholder, "row 1 is a placeholder");
        assert_eq!(second.spec.identity.abs_index, 1, "placeholder identity");
        assert_eq!(
            RowSpec::PLACEHOLDER_CLASSES,
            "row row-placeholder",
            "placeholder class list"
        );
    }

    #[test]
    fn window_specs_carries_the_planned_specs_to_the_dom_renderer() {
        let rows = vec![row_value("repo:a", "k0"), row_value("repo:a", "k1")];
        let state = cache_state(&rows, 2);
        let specs = window_specs(&state, 0, 1);
        assert_eq!(specs.len(), 2, "specs cover the window");
        let first = specs.first().expect("first spec");
        assert_eq!(first.identity.abs_index, 0, "first spec identity");
        let second = specs.get(1).expect("second spec");
        assert_eq!(second.identity.node_key, "k1", "second spec node key");
    }

    #[test]
    fn append_window_seeds_group_start_from_the_previous_rendered_row() {
        let rows = vec![row_value("repo:a", "k0"), row_value("repo:a", "k1")];
        let state = cache_state(&rows, 2);
        let planned = window_rows_from(&state, 1, 1, Some("repo:a"));
        assert_eq!(planned.len(), 1, "one appended row");
        let row = planned.first().expect("appended row");
        assert!(
            !row.spec.state.group_start,
            "same group below an existing row is not a group start"
        );
        let planned = window_rows_from(&state, 1, 1, Some("repo:b"));
        let row = planned.first().expect("appended row");
        assert!(
            row.spec.state.group_start,
            "a new group below an existing row is a group start"
        );
    }

    #[test]
    fn canvas_view_bounds_the_surface_and_keeps_scale_sane() {
        let view = canvas_view(44.28, 560.0, 1.0);
        assert!(view.scale >= 0.125 && view.scale <= 2.0, "scale is bounded");
        assert!(
            view.backing_width >= 1 && view.backing_height >= 1,
            "backing is nonzero"
        );
        assert!(
            f64::from(view.backing_width).max(f64::from(view.backing_height))
                <= MAX_SURFACE_EDGE + 1.0,
            "backing edge stays within the texture cap"
        );
        let tall = canvas_view(44.28, 8192.0, 1.0);
        assert!(tall.scale < 1.0, "tall surfaces downscale below 1");
        assert!(
            f64::from(tall.backing_height) <= MAX_SURFACE_EDGE + 1.0,
            "tall backing height is capped"
        );
        let floor = canvas_view(44.28, 100_000.0, 1.0);
        assert!(
            (floor.scale - 0.125).abs() < 1e-9,
            "the 0.125 scale floor keeps extreme heights renderable"
        );
    }

    #[test]
    fn frame_rows_include_only_viewport_rows_plus_overscan() {
        let mut state = HistoryAppState {
            total: Some(5),
            ..HistoryAppState::default()
        };
        for index in 0..5 {
            let key = i64::from(index);
            let value = row_value("repo:a", &format!("k{index}"));
            drop(state.cache.insert(key, value));
        }
        state.render_top = 0;
        state.render_bottom = 4;
        // 100px viewport shows rows 0..=2 (0..102px) plus overscan row 3.
        let frame = frame_rows(&state, 0, 100.0);
        assert_eq!(frame.len(), 4, "three visible rows + one overscan row");
        let first = frame.first().expect("first frame row");
        assert_eq!(first.index, 0, "first frame row is row 0");
        assert_eq!(first.key, "k0", "node key rides the frame row");
        assert!((first.top - 0.0).abs() < 1e-9, "row 0 top is 0");
        assert!((first.bottom - 34.0).abs() < 1e-9, "row 0 bottom is 34");
        let scrolled = frame_rows(&state, 170, 100.0);
        assert!(
            scrolled.first().is_some_and(|row| row.index >= 3),
            "scrolling shifts the frame window"
        );
    }

    #[test]
    fn build_frame_value_matches_the_gpu_frame_contract() {
        let layout = graph_layout(1, 1440.0, 1440.0);
        let rows = vec![FrameRow {
            index: 0,
            key: "k0".to_owned(),
            top: 0.0,
            bottom: 34.0,
            middle: 17.0,
            lane: 0,
            above: Vec::new(),
            below: vec![0],
            transitions: vec![(0, 1)],
            is_subop: false,
            is_bundle: false,
        }];
        let frame = build_frame_value(&layout, DEFAULT_EDITOR_BACKGROUND_HEX, &rows);
        let graph = frame.get("graph").expect("frame has graph");
        assert_eq!(
            graph.get("lane_x").and_then(Value::as_array).map(Vec::len),
            Some(2),
            "lane_x covers every lane"
        );
        assert_eq!(
            graph.get("width").and_then(Value::as_f64),
            Some(44.28),
            "graph width is the natural column width"
        );
        assert_eq!(
            frame.get("rows").and_then(Value::as_array).map(Vec::len),
            Some(1)
        );
        let row = frame
            .get("rows")
            .and_then(|value| value.as_array())
            .and_then(|array| array.first())
            .expect("first frame row");
        assert_eq!(row.get("index"), Some(&Value::from(0)));
        assert_eq!(row.get("key"), Some(&Value::from("k0")));
        assert_eq!(row.get("node_key"), Some(&Value::from("k0")));
        assert_eq!(
            row.get("transitions")
                .and_then(Value::as_array)
                .and_then(|array| array.first()),
            Some(&Value::from(vec![0, 1]))
        );
    }

    #[test]
    fn col_style_keeps_the_fixed_date_column_outside_the_narrow_breakpoint() {
        let widths = ColWidths::default();
        assert_eq!(
            col_style(44.28, 1440.0, &widths),
            "--graph-w:44.28px;--date-w:140px",
            "wide panels carry the fixed date width"
        );
        assert_eq!(
            col_style(44.28, 380.0, &widths),
            "--graph-w:44.28px",
            "narrow panels drop the date track"
        );
        let dragged = ColWidths {
            content: Some(220.0),
            date: Some(90.0),
            ..ColWidths::default()
        };
        assert_eq!(
            col_style(44.28, 1440.0, &dragged),
            "--graph-w:44.28px;--content-w:220px;--date-w:90px",
            "dragged overrides reach the inline style"
        );
    }

    #[test]
    fn every_column_has_min_default_and_parse_round_trips() {
        for col in [
            ColKey::Graph,
            ColKey::Content,
            ColKey::Date,
            ColKey::Author,
            ColKey::Commit,
        ] {
            assert!(col.min_width() >= 40.0, "{} min width", col.as_str());
            assert!(col.default_width() >= 0.0, "{} default width", col.as_str());
            assert_eq!(
                ColKey::parse(col.as_str()),
                Some(col),
                "{} parses back",
                col.as_str()
            );
        }
        assert_eq!(ColKey::parse("nope"), None);
        assert_eq!(ColKey::parse(""), None);
    }

    #[test]
    fn col_widths_set_applies_and_clears_overrides() {
        let mut widths = ColWidths::default();
        widths.set(ColKey::Graph, Some(180.0));
        widths.set(ColKey::Content, Some(220.0));
        widths.set(ColKey::Date, Some(90.0));
        assert_eq!(widths.graph, Some(180.0));
        assert_eq!(widths.content, Some(220.0));
        assert_eq!(widths.date, Some(90.0));
        widths.set(ColKey::Graph, None);
        assert_eq!(widths.graph, None);
    }

    #[test]
    fn hidden_columns_keep_author_and_commit_hidden_and_drop_date_narrow() {
        assert_eq!(hidden_columns(1440.0), vec![ColKey::Author, ColKey::Commit]);
        assert_eq!(
            hidden_columns(380.0),
            vec![ColKey::Author, ColKey::Commit, ColKey::Date]
        );
    }

    #[test]
    fn graph_override_changes_the_column_width_but_never_lane_centers() {
        let widths = ColWidths {
            graph: Some(180.0),
            ..ColWidths::default()
        };
        let layout = graph_layout(1, 1440.0, 1440.0);
        assert!(
            (current_graph_width(&layout, &widths) - 180.0).abs() < 1e-9,
            "the divider override wins over the natural width"
        );
        assert!(
            (current_graph_width(&layout, &ColWidths::default()) - layout.column_width).abs()
                < 1e-9,
            "no override means the natural lane-based width"
        );
        assert_eq!(
            layout.lane_x,
            vec![14.76, 29.52],
            "lane centers never rescale with the divider"
        );
    }
}
