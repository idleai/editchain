//! Read-only browser diagnostics and render observations.
//!
//! Export names and window properties are the interface consumed by the shipped
//! loader and existing browser/VS Code suites. Diagnostic failures can be
//! recorded while the shell is borrowed; reporting never borrows it again.

use super::{ShellData, SHELL_DATA};
use crate::app::dom;
use crate::app::row_input::RowInput;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

#[derive(Debug)]
pub(super) struct Diagnostics {
    pub(super) renderer_ready: bool,
    pub(super) wasm_ready: bool,
    render_count: u64,
    generation: u64,
    started_at_ms: f64,
    first_window_ms: Option<f64>,
    last_render_ms: Option<f64>,
    last_frame_rows: Vec<dom::FrameRow>,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            renderer_ready: false,
            wasm_ready: false,
            render_count: 0,
            generation: 0,
            started_at_ms: performance_now(),
            first_window_ms: None,
            last_render_ms: None,
            last_frame_rows: Vec::new(),
        }
    }
}

/// The window handle (the shell runs inside a browser context).
fn window_handle() -> Option<web_sys::Window> {
    web_sys::window()
}

/// Current performance clock in ms.
fn performance_now() -> f64 {
    window_handle()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}

/// Set a property on the JS window (debug hooks the harness reads).
pub(super) fn set_window_prop(name: &str, value: &JsValue) {
    let Some(window) = window_handle() else {
        return;
    };
    let window_value: JsValue = window.into();
    drop(js_sys::Reflect::set(
        &window_value,
        &JsValue::from_str(name),
        value,
    ));
}

/// Record an error without reborrowing the shell from a failing DOM effect.
pub(super) fn record_error(message: &str) {
    web_sys::console::error_1(&JsValue::from_str(message));
    set_window_prop("__editchainLastError", &JsValue::from_str(message));
}

/// Render a JS value as diagnostic text.
pub(super) fn js_value_text(value: &JsValue) -> String {
    js_sys::JSON::stringify(value)
        .map(|js| js.as_string().unwrap_or_default())
        .unwrap_or_default()
}

/// Install the read-only harness hooks (`__editchainGetTotal`,
/// `__editchainRowAt`). These are pure facades over
/// the live shell — no JS app state or business logic lives here.
pub(super) fn install_parity_hooks() {
    let get_total = Closure::<dyn FnMut() -> f64>::wrap(Box::new(|| {
        SHELL_DATA.with(|cell| {
            cell.borrow().as_ref().map_or(-1.0, |shell| {
                dom::i64_to_f64(shell.state.total.unwrap_or(-1))
            })
        })
    }));
    let row_at = Closure::<dyn FnMut(f64) -> JsValue>::wrap(Box::new(|abs: f64| {
        SHELL_DATA.with(|cell| {
            let shell_ref = cell.borrow();
            let Some(shell) = shell_ref.as_ref() else {
                return JsValue::NULL;
            };
            let index = dom::f64_round_to_i64(abs);
            let Some(row) = shell.state.cache.get_by_index(index) else {
                return JsValue::NULL;
            };
            js_sys::JSON::parse(&row.wire_json()).unwrap_or(JsValue::NULL)
        })
    }));
    set_window_prop("__editchainGetTotal", get_total.as_ref().unchecked_ref());
    set_window_prop("__editchainRowAt", row_at.as_ref().unchecked_ref());
    // The window props hold the JS functions; the wasm closures leak
    // deliberately for the shell's lifetime.
    get_total.forget();
    row_at.forget();
}

/// Mirror shell/state flags onto the window (debug contract).
pub(super) fn sync_debug_props() {
    SHELL_DATA.with(|cell| {
        if let Some(shell) = cell.borrow_mut().as_mut() {
            sync_debug_props_locked(shell);
        }
    });
}

pub(super) fn sync_debug_props_locked(shell: &mut ShellData) {
    let ready = shell.state.data_ready();
    set_window_prop("__editchainDataReady", &JsValue::from_bool(ready));
    set_window_prop(
        "__editchainInFlightCount",
        &js_sys::Number::from(u32::try_from(shell.state.requests.len()).unwrap_or(u32::MAX)).into(),
    );
    set_window_prop(
        "__editchainViewGen",
        &js_sys::Number::from(u32::try_from(shell.state.view_gen).unwrap_or(u32::MAX)).into(),
    );
    set_window_prop(
        "__editchainGeneration",
        &js_sys::Number::from(u32::try_from(shell.diagnostics.generation).unwrap_or(u32::MAX))
            .into(),
    );
    set_window_prop(
        "__editchainRenderCount",
        &js_sys::Number::from(u32::try_from(shell.diagnostics.render_count).unwrap_or(u32::MAX))
            .into(),
    );
    set_window_prop(
        "__editchainRendererReady",
        &JsValue::from_bool(shell.diagnostics.renderer_ready),
    );
    set_window_prop(
        "__editchainWasmReady",
        &JsValue::from_bool(shell.diagnostics.wasm_ready),
    );
    set_window_prop(
        "__editchainRendererInstanceId",
        &JsValue::from_str(&shell.instance_id),
    );
    set_window_prop("__editchainProfile", &JsValue::from_str("activity"));
}

/// `__editchainDataReady` mirror: content has rendered for the view.
#[wasm_bindgen(js_name = "debugDataReady")]
#[must_use]
pub fn debug_data_ready() -> bool {
    SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|shell| shell.state.data_ready())
    })
}

/// `__editchainGetTotal` data source: the authoritative history total, or
/// `-1` while unknown.
#[wasm_bindgen(js_name = "debugTotal")]
#[must_use]
pub fn debug_total() -> f64 {
    SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|shell| shell.state.total)
            .map_or(-1.0, dom::i64_to_f64)
    })
}

/// `__editchainRendererDebug.findState()` data source: the settled find session
/// (read-only parity facade; never app state).
#[wasm_bindgen(js_name = "debugFindState")]
#[must_use]
pub fn debug_find_state() -> String {
    SHELL_DATA.with(|cell| {
        let borrow = cell.borrow();
        let Some(shell) = borrow.as_ref() else {
            return String::new();
        };
        json!({
            "active": shell.state.find_active(),
            "index": shell.state.find_index(),
            "total": shell.state.find_total(),
            "more": shell.state.find_more(),
            "epoch": shell.state.current_search_epoch(),
            "currentRow": shell.state.current_find_match().map(|found| found.row.get()),
        })
        .to_string()
    })
}

/// `__editchainRendererInstanceId` data source.
#[wasm_bindgen(js_name = "debugRendererInstanceId")]
#[must_use]
pub fn debug_renderer_instance_id() -> String {
    SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|shell| shell.instance_id.clone())
            .unwrap_or_default()
    })
}

/// `__editchainGraphState` data source (render window + lane geometry).
#[wasm_bindgen(js_name = "debugGraphState")]
#[must_use]
pub fn debug_graph_state() -> String {
    SHELL_DATA.with(|cell| {
        let borrow = cell.borrow();
        let Some(shell) = borrow.as_ref() else {
            return String::new();
        };
        json!({
            "renderTop": shell.state.render_top,
            "renderBottom": shell.state.render_bottom,
            "maxLane": shell.state.max_lane,
            "layoutReady": shell.state.layout_ready(),
            "graphWidth": shell.current_graph_width(),
        })
        .to_string()
    })
}

/// `__editchainGraphAdapter.laneXAll` data source (fixed CSS-px centers).
#[wasm_bindgen(js_name = "debugLaneXAll")]
#[must_use]
pub fn debug_lane_x_all() -> String {
    SHELL_DATA.with(|cell| {
        let borrow = cell.borrow();
        let Some(shell) = borrow.as_ref() else {
            return String::new();
        };
        json!(shell.layout().lane_x).to_string()
    })
}

/// `__editchainRendererDebug.snapshot()` data source.
#[wasm_bindgen(js_name = "debugSnapshot")]
#[must_use]
pub fn debug_snapshot() -> String {
    SHELL_DATA.with(|cell| {
        let borrow = cell.borrow();
        let Some(shell) = borrow.as_ref() else {
            return String::new();
        };
        let rows: Vec<Value> = shell
            .diagnostics
            .last_frame_rows
            .iter()
            .map(|row| {
                json!({
                    "index": row.index,
                    "key": row.key,
                    "node_key": row.key,
                    "lane": row.lane,
                    "above": row.above,
                    "below": row.below,
                    "transitions": row.transitions,
                    "top": row.top,
                    "bottom": row.bottom,
                    "middle": row.middle,
                    "is_subop": row.is_subop,
                    "is_bundle": row.is_bundle,
                    "expanded": row.expanded,
                })
            })
            .collect();
        json!({
            "rows": rows,
            "total": shell.state.total.unwrap_or(-1),
            "backend": "svg",
        })
        .to_string()
    })
}

/// `__editchainRendererDebug.metrics()` data source (bootstrap field names).
#[wasm_bindgen(js_name = "debugMetrics")]
#[must_use]
pub fn debug_metrics() -> String {
    SHELL_DATA.with(|cell| {
        let borrow = cell.borrow();
        let Some(shell) = borrow.as_ref() else {
            return String::new();
        };
        json!({
            "initMs": (performance_now() - shell.diagnostics.started_at_ms).max(0.0),
            "firstWindowMs": shell.diagnostics.first_window_ms,
            "lastRenderMs": shell.diagnostics.last_render_ms,
            "renderCount": shell.diagnostics.render_count,
            "domRows": shell.diagnostics.last_frame_rows.len(),
            "generation": shell.diagnostics.generation,
            "rendererReady": shell.diagnostics.renderer_ready,
            "dataReady": shell.state.data_ready(),
        })
        .to_string()
    })
}

/// `__editchainInFlightCount` data source.
#[wasm_bindgen(js_name = "debugInFlightCount")]
#[must_use]
pub fn debug_in_flight_count() -> u64 {
    SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|shell| u64::try_from(shell.state.requests.len()).ok())
            .unwrap_or(u64::MAX)
    })
}

/// `__editchainViewGen` data source.
#[wasm_bindgen(js_name = "debugViewGen")]
#[must_use]
pub fn debug_view_gen() -> u64 {
    SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0, |shell| shell.state.view_gen)
    })
}

/// `__editchainRowAt` data source: the cached row JSON at an absolute index.
#[wasm_bindgen(js_name = "debugRowAt")]
#[must_use]
pub fn debug_row_at(abs: i64) -> String {
    SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|shell| shell.state.cache.get_by_index(abs))
            .map_or_else(|| "null".to_owned(), RowInput::wire_json)
    })
}

/// The active graph renderer: the per-row SVG cells (`svg`).
#[wasm_bindgen(js_name = "debugBackend")]
#[must_use]
pub fn debug_backend() -> String {
    "svg".to_owned()
}

/// DOM generation counter (render passes) for `whenIdle` stability checks.
#[wasm_bindgen(js_name = "debugGeneration")]
#[must_use]
pub fn debug_generation() -> u64 {
    SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0, |shell| shell.diagnostics.generation)
    })
}

/// Successful per-row SVG render passes.
#[wasm_bindgen(js_name = "debugRenderCount")]
#[must_use]
pub fn debug_render_count() -> u64 {
    SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0, |shell| shell.diagnostics.render_count)
    })
}

impl ShellData {
    /// Publish debug and accessibility state after SVG rows change.
    pub(super) fn publish_render_state(&mut self) {
        let host_height = self.dom.client_height_css();
        let rows = dom::frame_rows(&self.state, self.dom.scroll_top(), host_height);
        let started = performance_now();
        self.diagnostics.last_frame_rows.clone_from(&rows);
        self.diagnostics.render_count = self.diagnostics.render_count.saturating_add(1);
        self.diagnostics.generation = self.diagnostics.generation.saturating_add(1);
        self.diagnostics.last_render_ms = Some(performance_now() - started);
        if self.diagnostics.first_window_ms.is_none() {
            self.diagnostics.first_window_ms =
                Some(performance_now() - self.diagnostics.started_at_ms);
        }
        set_window_prop("__editchainLastError", &JsValue::NULL);
        let total = self.state.total.unwrap_or(0);
        self.dom
            .set_status(&format!("{} / {} rows", rows.len(), total));
    }
}
