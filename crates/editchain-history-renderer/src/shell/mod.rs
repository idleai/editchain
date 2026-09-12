//! Browser shell composition, DOM effects, and startup.
//!
//! Owns startup sequencing (VS Code API acquisition through the narrow
//! wasm-bindgen binding, message-listener installation before
//! `webviewReady`, state restore/save), the `HistoryAppState` machine, the
//! reducer `Step` sends and DOM ops, scroll/search controls and persistence,
//! the debug hooks, and the per-row SVG graph render pass.
//!
//! Reentrancy contract: the fixture bridge dispatches correlated
//! responses synchronously inside `postMessage`, so host messages are
//! queued and drained by [`runtime`] with sends executed only after
//! the shell borrow is released; DOM ops are applied before sends within
//! each transition so nested response steps never reorder the window.

use std::cell::RefCell;

use serde_json::json;
use wasm_bindgen::prelude::*;

use crate::app::dom::{self, HistoryDom};
use crate::app::host::Send;
use crate::app::rows::{self, RowContext, RowSpec};
use crate::app::state::{DomOp, HistoryAppState, Step, Viewport, ROW_H};

mod diagnostics;
mod input;
mod resources;
mod runtime;

use diagnostics::{
    install_parity_hooks, js_value_text, record_error, set_window_prop, sync_debug_props,
};
use resources::{EventListener, ResizeSubscription, Timer};
use runtime::{acquire_vscode_api, execute_send, get_state_, on_message_event};

use input::{
    on_progressive_tick, on_resize_observed, on_retry, on_row_click, on_row_dblclick,
    on_row_focusin, on_row_keydown, on_rows_mousedown, on_scroll, on_search_input,
    on_search_keydown, on_search_nav,
};

thread_local! {
    /// The live shell; created once at startup.
    static SHELL_DATA: RefCell<Option<ShellData>> = const { RefCell::new(None) };
}

/// The live shell: DOM + state machine + SVG render counters.
#[derive(Debug)]
struct ShellData {
    vscode: JsValue,
    dom: HistoryDom,
    state: HistoryAppState,
    instance_id: String,
    diagnostics: diagnostics::Diagnostics,
    progressive_timer: Option<Timer>,
    /// Debounced viewport-resize timer (production `resizeTimer`).
    resize_timer: Option<Timer>,
    /// Last observed `#rows` width (the resize observer ignores
    /// height-only notifications, matching `lastRowsWidth`).
    last_rows_width: f64,
    resize_observer: Option<ResizeSubscription>,
    /// Replaced whenever the current error pane is retired.
    retry_listener: Option<EventListener>,
    /// User-dragged column widths (divider state; `None` = natural).
    col_widths: dom::ColWidths,
    drawn_graph: Option<dom::GraphCellSpec>,
}

impl ShellData {
    fn layout(&self) -> dom::GraphLayout {
        dom::graph_layout(
            self.state.graph_max_lane(),
            self.dom.rows_client_width_css(),
            self.dom.window_inner_width_css(),
        )
    }

    /// `currentGraphWidth()` — the effective graph column width (divider
    /// override or natural). Lane centers stay pinned to the natural
    /// width (`layout().lane_x`), so dragging never rescales topology.
    fn current_graph_width(&self) -> f64 {
        let layout = self.layout();
        dom::current_graph_width(&layout, &self.col_widths)
    }

    /// The per-row SVG graph cell geometry: pinned natural lane centers,
    /// the compressed dot radius, the rendered column width (divider
    /// override or natural), and the fixed `ROW_H` cell height.
    fn graph_cell_spec(&self) -> dom::GraphCellSpec {
        let layout = self.layout();
        dom::GraphCellSpec {
            lane_x: layout.lane_x.clone(),
            dot_radius: layout.dot_radius,
            width: self.current_graph_width(),
            height: dom::i64_to_f64(ROW_H),
        }
    }

    fn col_style(&self) -> String {
        let layout = self.layout();
        dom::col_style(
            dom::current_graph_width(&layout, &self.col_widths),
            self.dom.window_inner_width_css(),
            &self.col_widths,
        )
    }

    fn pane_status(&self) -> dom::PaneStatus {
        dom::PaneStatus {
            open_warnings: self.state.open_warnings.clone(),
        }
    }

    /// The state-aware per-row context (selection/find/expansion/roving).
    fn row_context(&self, abs_index: i64, is_group_start: bool) -> RowContext {
        self.state.row_context(abs_index, is_group_start)
    }

    fn spacer_height_px(&self) -> i64 {
        self.state.visible_total().saturating_mul(ROW_H).max(1)
    }

    fn reanchor_window(&mut self, top: i64, bottom: i64) -> Result<(), JsValue> {
        self.render_window(top, bottom, false)
    }

    fn render_window(&mut self, top: i64, bottom: i64, live: bool) -> Result<(), JsValue> {
        self.retry_listener = None;
        let specs = dom::window_specs(&self.state, top, bottom);
        let col_style = self.col_style();
        let graph = self.graph_cell_spec();
        let options = dom::RebuildOptions {
            spacer_height_px: self.spacer_height_px(),
            wrap_top_px: top.saturating_mul(ROW_H),
            aria_rowcount: self.state.visible_total(),
            graph_width_css: self.current_graph_width(),
            graph,
            status: self.pane_status(),
        };
        if live {
            self.dom.patch_live(&specs, &col_style, &options)?;
        } else {
            self.dom.reanchor(&specs, &col_style, &options)?;
        }
        self.drawn_graph = Some(options.graph);
        self.install_resize_handles()?;
        Ok(())
    }

    /// Re-create the column-divider handles after a full rebuild
    /// (`setupColumnResizeHandles`).
    fn install_resize_handles(&mut self) -> Result<(), JsValue> {
        self.dom
            .install_resize_handles(self.dom.window_inner_width_css())
    }

    /// `applyRovingTabindex` — enforce exactly one tabbable row over the
    /// rendered window; the anchor falls back to the first rendered row
    /// when the current one was scrolled/trimmed away.
    fn apply_roving_tabindex(&mut self) {
        let Some(wrap) = self.dom.wrap() else {
            return;
        };
        let Ok(list) = wrap.query_selector_all(".row") else {
            return;
        };
        if list.length() == 0 {
            return;
        }
        let mut anchor_abs = None;
        let mut first_abs = None;
        for index in 0..list.length() {
            let item = list.item(index);
            let Some(node) = item else {
                continue;
            };
            let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                continue;
            };
            let Some(abs) = element
                .get_attribute("data-row")
                .and_then(|raw| raw.trim().parse::<i64>().ok())
            else {
                continue;
            };
            if first_abs.is_none() {
                first_abs = Some(abs);
            }
            if abs == self.state.roving_abs() {
                anchor_abs = Some(abs);
            }
        }
        let anchor = anchor_abs.or(first_abs);
        let Some(anchor) = anchor else {
            return;
        };
        if anchor != self.state.roving_abs() {
            self.state.set_roving_abs(anchor);
        }
        let anchor_text = anchor.to_string();
        for index in 0..list.length() {
            let item = list.item(index);
            let Some(node) = item else {
                continue;
            };
            let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                continue;
            };
            let Some(abs) = element
                .get_attribute("data-row")
                .and_then(|raw| raw.trim().parse::<i64>().ok())
            else {
                continue;
            };
            drop(element.set_attribute(
                "tabindex",
                if abs.to_string() == anchor_text {
                    "0"
                } else {
                    "-1"
                },
            ));
        }
    }

    /// `syncFindNavButtons` — Previous/Next are shown/enabled only when a
    /// settled, navigable find session matches the exact input text.
    fn sync_find_nav(&mut self) {
        let value = self.dom.search_input_value();
        let enabled = self.state.find_navigation_enabled(&value);
        self.dom.set_find_nav(enabled);
    }

    /// Production click/chevron semantics: select the row, then toggle
    /// disclosure for any row with children, including an existing bundle
    /// nested one level inside a work group.
    fn row_select_and_toggle(&mut self, abs: i64, viewport: &Viewport, step: &mut Step) {
        self.state.select_row(abs);
        if let Err(error) = self.dom.apply_selection(abs) {
            record_error(&format!(
                "selection apply failed: {}",
                js_value_text(&error)
            ));
        }
        if self.open_diff_for_abs(abs, step) {
            return;
        }
        let expandable = self
            .state
            .cache
            .get_by_index(abs)
            .is_some_and(rows::has_sub_ops);
        if expandable {
            self.state.toggle_expanded_ui(abs, viewport, step);
        }
    }

    /// Post the exact advertised file-change identity for host-side native
    /// diff materialization. Returns whether this row is a file row.
    fn open_diff_for_abs(&self, abs: i64, step: &mut Step) -> bool {
        let Some(row) = self.state.cache.get_by_index(abs) else {
            return false;
        };
        let Some(mut envelope) = rows::open_diff_envelope(row) else {
            return false;
        };
        if let Some(fields) = envelope.as_object_mut() {
            drop(fields.insert("snapshot_id".to_owned(), json!(self.state.snapshot_id)));
        }
        step.sends.push(Send::OpenDiff(envelope));
        true
    }

    /// `openRawJson` — post the exact `openJson` identity envelope
    /// (`git_oid`+`repository` or `op_id`), or announce the absence.
    fn open_json_for_abs(&mut self, abs: i64, step: &mut Step) {
        let Some(row) = self.state.cache.get_by_index(abs) else {
            return;
        };
        if let Some(mut envelope) = rows::open_json_envelope(row) {
            if let Some(fields) = envelope.as_object_mut() {
                drop(fields.insert("snapshot_id".to_owned(), json!(self.state.snapshot_id)));
            }
            step.sends.push(Send::OpenJson(envelope));
        } else {
            step.sends.push(Send::StatusText(
                "No raw record is available for this row".to_owned(),
            ));
        }
    }

    fn append_window(&mut self, from: i64, to: i64) -> Result<(), JsValue> {
        let last_group = self.last_rendered_group();
        let specs = dom::window_rows_from(&self.state, from, to, last_group.as_deref())
            .into_iter()
            .map(|row| row.spec)
            .collect::<Vec<_>>();
        let col_style = self.col_style();
        let graph = self.graph_cell_spec();
        self.dom.append_rows(&specs, &col_style, &graph)
    }

    fn prepend_window(&mut self, from: i64, to: i64) -> Result<(), JsValue> {
        let planned = dom::window_rows_from(&self.state, from, to, None);
        let specs = planned
            .iter()
            .map(|row| row.spec.clone())
            .collect::<Vec<_>>();
        let col_style = self.col_style();
        let graph = self.graph_cell_spec();
        self.dom.prepend_rows(
            &specs,
            &col_style,
            self.state.render_top.saturating_mul(ROW_H),
            &graph,
        )?;
        // Production re-evaluates the old first-row chip against its new
        // previous sibling after a prepend crosses a group boundary.
        let boundary_vis = to.saturating_add(1);
        let Some(prev_group) = planned
            .iter()
            .rev()
            .find(|row| !row.spec.placeholder)
            .map(|row| row.spec.group.clone())
        else {
            return Ok(());
        };
        let boundary =
            dom::window_rows_from(&self.state, boundary_vis, boundary_vis, Some(&prev_group));
        if let Some(row) = boundary.first() {
            let abs = row.spec.identity.abs_index;
            self.dom
                .replace_row_abs(abs, &row.spec, &col_style, &graph)?;
        }
        Ok(())
    }

    fn trim_top(&mut self, keep_top: i64) -> Result<(), JsValue> {
        // Trim by scanning the rendered DOM and mapping each rendered
        // absolute id back through the collapsed-mode mapping (production
        // `trimTop`): visible bounds never compare directly to `data-row`
        // absolute values, and rows added by a prepend/append during this
        // same transition are covered too. The wrap then shifts to the
        // state's advanced visible top (`setWrapTop(renderTop)`).
        let rendered = self.dom.rendered_row_abs();
        let remove = dom::rows_outside_visible(&self.state, &rendered, keep_top, i64::MAX);
        self.dom.remove_abs(&remove)?;
        self.dom
            .set_wrap_top(self.state.render_top.saturating_mul(ROW_H))
    }

    fn trim_bottom(&mut self, keep_bottom: i64) -> Result<(), JsValue> {
        let rendered = self.dom.rendered_row_abs();
        let remove = dom::rows_outside_visible(&self.state, &rendered, i64::MIN, keep_bottom);
        self.dom.remove_abs(&remove)
    }

    fn fill_placeholders(&mut self) -> Result<(), JsValue> {
        let col_style = self.col_style();
        let graph = self.graph_cell_spec();
        for abs in self.dom.placeholder_abs() {
            let Some(row) = self.state.cache.get_by_index(abs) else {
                continue;
            };
            let prev_group = self.previous_rendered_group(abs);
            let group = row.source.group.as_str();
            let is_group_start = prev_group.as_deref().is_none_or(|prev| prev != group);
            let context = self.row_context(abs, is_group_start);
            let spec = RowSpec::from_row(row, &context);
            self.dom.replace_row_abs(abs, &spec, &col_style, &graph)?;
        }
        Ok(())
    }

    fn previous_rendered_group(&self, abs: i64) -> Option<String> {
        let prev = self.dom.previous_row_abs(abs)?;
        self.state
            .cache
            .get_by_index(prev)
            .map(|row| row.source.group.clone())
    }

    fn last_rendered_group(&self) -> Option<String> {
        let abs = self.dom.last_row_abs()?;
        self.state
            .cache
            .get_by_index(abs)
            .map(|row| row.source.group.clone())
    }

    fn refresh_header(&mut self) -> Result<(), JsValue> {
        let col_style = self.col_style();
        let graph_width = self.current_graph_width();
        self.dom.refresh_header(&col_style, graph_width)
    }

    /// Apply one reducer DOM op.
    fn apply_op(&mut self, op: &DomOp) -> Result<(), JsValue> {
        match op {
            DomOp::ReanchorLive {
                top,
                bottom,
                scroll_top,
            } => {
                let before = self.dom.capture_live_rows()?;
                let focused = self.dom.has_row_focus().then(|| self.state.roving_abs());
                self.render_window(*top, *bottom, true)?;
                self.dom.set_scroll_top(*scroll_top);
                if let Some(absolute) = focused {
                    self.dom.focus_live_row(absolute)?;
                }
                self.dom.animate_live_rows(&before)
            }
            DomOp::ShowMessage { text, error } => {
                self.retry_listener = None;
                self.dom.show_message(text, *error)
            }
            DomOp::ShowRequestError { text, retry } => {
                let button = self.dom.show_request_error(text)?;
                let retry = *retry;
                self.retry_listener =
                    Some(EventListener::new(button.into(), "click", move |_| {
                        on_retry(retry);
                    })?);
                Ok(())
            }
            DomOp::Reanchor { top, bottom } => self.reanchor_window(*top, *bottom),
            DomOp::AppendBelow { from, to } => self.append_window(*from, *to),
            DomOp::PrependAbove { from, to } => self.prepend_window(*from, *to),
            DomOp::TrimTop { keep_top } => self.trim_top(*keep_top),
            DomOp::TrimBottom { keep_bottom } => self.trim_bottom(*keep_bottom),
            DomOp::FillPlaceholders => self.fill_placeholders(),
            DomOp::RefreshHeader => self.refresh_header(),
            DomOp::SetScrollTop(px) => {
                self.dom.set_scroll_top(*px);
                Ok(())
            }
            DomOp::RestoreScrollTop { row_index } => {
                let spacer = self.spacer_height_px();
                self.dom.restore_scroll_top(*row_index, spacer);
                Ok(())
            }
            DomOp::ProgressiveLoader(active) => {
                if *active {
                    self.start_progressive_loader();
                } else {
                    self.stop_progressive_loader();
                }
                Ok(())
            }
            DomOp::FindCounter(state) => {
                self.dom.set_find_counter_state(state);
                self.sync_find_nav();
                Ok(())
            }
            DomOp::RevealRow { abs } => self.dom.reveal_row(*abs),
            DomOp::SetFindHighlight { abs } => {
                self.dom.apply_selection(*abs)?;
                self.dom.set_find_highlight(*abs)
            }
            DomOp::ClearFindHighlight => {
                self.dom.clear_selection_ui()?;
                self.dom.clear_find_highlight()
            }
        }
    }

    /// Apply all reducer DOM ops in order (called while the shell borrow is
    /// held, BEFORE the step's sends are posted). The roving-tabindex
    /// invariant and the find-nav enabled state are re-reconciled after
    /// every step so they survive any DOM mutation.
    fn apply_step_ops(&mut self, step: &Step) {
        for op in &step.ops {
            let result = self.apply_op(op);
            if let Err(error) = result {
                let message = format!("DOM op failed: {}", js_value_text(&error));
                record_error(&message);
            }
        }
        if let Err(error) = self.sync_graph_frame() {
            record_error(&format!(
                "Graph frame update failed: {}",
                js_value_text(&error)
            ));
        }
        self.apply_roving_tabindex();
        self.sync_find_nav();
    }

    fn sync_graph_frame(&mut self) -> Result<(), JsValue> {
        if self.state.data_ready()
            && self.dom.wrap().is_some()
            && self
                .drawn_graph
                .as_ref()
                .is_some_and(|graph| *graph != self.graph_cell_spec())
        {
            self.render_window(self.state.render_top, self.state.render_bottom, true)?;
        }
        Ok(())
    }

    fn start_progressive_loader(&mut self) {
        if self.progressive_timer.is_some() {
            return;
        }
        match Timer::interval(250, on_progressive_tick) {
            Ok(timer) => {
                self.progressive_timer = Some(timer);
            }
            Err(error) => {
                let message = format!(
                    "progressive loader failed to start: {}",
                    js_value_text(&error)
                );
                record_error(&message);
            }
        }
    }

    fn stop_progressive_loader(&mut self) {
        self.progressive_timer = None;
    }
}

fn js_value_error(message: &str) -> JsValue {
    JsValue::from_str(message)
}

/// Install the shell, listeners, and webviewReady handshake.
fn install_shell() -> Result<(), JsValue> {
    // Surface Rust panic messages before the wasm abort trap: without a
    // hook, a panic in a wasm32-unknown-unknown release build is a silent
    // "unreachable" and leaves the loader's marker stuck on loading.
    std::panic::set_hook(Box::new(|info| {
        let message = format!("{info}");
        web_sys::console::error_1(&JsValue::from_str(&message));
        set_window_prop("__editchainLastError", &JsValue::from_str(&message));
    }));
    let vscode = acquire_vscode_api()?;
    let dom = HistoryDom::new()?;
    let initial_rows_width = f64::from(dom.rows().client_width());
    let mut state = HistoryAppState::default();
    let restored = get_state_(&vscode);
    if let Ok(value) = js_sys::JSON::stringify(&restored) {
        if let Some(text) = value.as_string() {
            state.persisted = serde_json::from_str(&text).ok();
        }
    }
    let shell = ShellData {
        vscode,
        dom,
        state,
        instance_id: new_instance_id(),
        diagnostics: diagnostics::Diagnostics::default(),
        progressive_timer: None,
        resize_timer: None,
        last_rows_width: initial_rows_width,
        resize_observer: None,
        retry_listener: None,
        col_widths: dom::ColWidths::default(),
        drawn_graph: None,
    };
    shell.dom.set_status("idle");
    shell.dom.set_find_nav(false);
    let rows_el = shell.dom.rows();
    let search_input = shell.dom.search_input();
    let search_prev = shell.dom.search_prev_button();
    let search_next = shell.dom.search_next_button();
    SHELL_DATA.with(|cell| drop(cell.borrow_mut().replace(shell)));

    let window =
        web_sys::window().ok_or_else(|| js_value_error("browser window is unavailable"))?;
    let message_closure = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(on_message_event));
    window.add_event_listener_with_callback("message", message_closure.as_ref().unchecked_ref())?;
    message_closure.forget();
    let scroll_closure = Closure::<dyn FnMut()>::wrap(Box::new(on_scroll));
    rows_el.add_event_listener_with_callback("scroll", scroll_closure.as_ref().unchecked_ref())?;
    scroll_closure.forget();
    // Search controls: keyboard (Enter/Escape/Arrow), input-clearing, and
    // the Previous/Next buttons (mousedown keeps focus in the input).
    let search_keydown_closure = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::wrap(Box::new(
        |event: web_sys::KeyboardEvent| on_search_keydown(&event),
    ));
    search_input.add_event_listener_with_callback(
        "keydown",
        search_keydown_closure.as_ref().unchecked_ref(),
    )?;
    search_keydown_closure.forget();
    let search_input_closure =
        Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_search_input()));
    search_input
        .add_event_listener_with_callback("input", search_input_closure.as_ref().unchecked_ref())?;
    search_input_closure.forget();
    let prev_mousedown =
        Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
            let mouse: web_sys::MouseEvent = event.unchecked_into();
            mouse.prevent_default();
        }));
    search_prev
        .add_event_listener_with_callback("mousedown", prev_mousedown.as_ref().unchecked_ref())?;
    prev_mousedown.forget();
    let next_mousedown =
        Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
            let mouse: web_sys::MouseEvent = event.unchecked_into();
            mouse.prevent_default();
        }));
    search_next
        .add_event_listener_with_callback("mousedown", next_mousedown.as_ref().unchecked_ref())?;
    next_mousedown.forget();
    let prev_click = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_search_nav(-1)));
    search_prev.add_event_listener_with_callback("click", prev_click.as_ref().unchecked_ref())?;
    prev_click.forget();
    let next_click = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_search_nav(1)));
    search_next.add_event_listener_with_callback("click", next_click.as_ref().unchecked_ref())?;
    next_click.forget();

    // Delegated row interactions: click (with detail guard + chevron),
    // double-click (raw JSON), focusin (roving anchor), keydown (roving
    // navigation / disclosure / activation), and mousedown (divider drag).
    let rows_click =
        Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
            on_row_click(&event);
        }));
    rows_el.add_event_listener_with_callback("click", rows_click.as_ref().unchecked_ref())?;
    rows_click.forget();
    let rows_dblclick =
        Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
            on_row_dblclick(&event);
        }));
    rows_el.add_event_listener_with_callback("dblclick", rows_dblclick.as_ref().unchecked_ref())?;
    rows_dblclick.forget();
    let rows_focusin =
        Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
            on_row_focusin(&event);
        }));
    rows_el.add_event_listener_with_callback("focusin", rows_focusin.as_ref().unchecked_ref())?;
    rows_focusin.forget();
    let rows_keydown = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::wrap(Box::new(
        |event: web_sys::KeyboardEvent| on_row_keydown(&event),
    ));
    rows_el.add_event_listener_with_callback("keydown", rows_keydown.as_ref().unchecked_ref())?;
    rows_keydown.forget();
    let rows_mousedown =
        Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
            on_rows_mousedown(&event);
        }));
    rows_el
        .add_event_listener_with_callback("mousedown", rows_mousedown.as_ref().unchecked_ref())?;
    rows_mousedown.forget();

    // Viewport resize: window resize + a ResizeObserver over #rows
    // (production `onViewportResize`, debounced).
    let window_resize =
        Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_resize_observed()));
    window.add_event_listener_with_callback("resize", window_resize.as_ref().unchecked_ref())?;
    window_resize.forget();
    match ResizeSubscription::new(&rows_el, on_resize_observed) {
        Ok(subscription) => {
            SHELL_DATA.with(|cell| {
                if let Some(shell) = cell.borrow_mut().as_mut() {
                    shell.resize_observer = Some(subscription);
                }
            });
        }
        Err(_) => {
            record_error(
                "ResizeObserver is unavailable; viewport resize falls back to window resize",
            );
        }
    }

    install_parity_hooks();

    // Readiness handshake AFTER the listener exists (synchronous fixture
    // replies must correlate; see the Send::WebviewReady contract).
    let instance_id = SHELL_DATA.with(|cell| {
        cell.borrow_mut()
            .as_mut()
            .map(|shell| shell.instance_id.clone())
            .unwrap_or_default()
    });
    execute_send(&Send::WebviewReady(instance_id));
    SHELL_DATA.with(|cell| {
        if let Some(shell) = cell.borrow_mut().as_mut() {
            shell.diagnostics.wasm_ready = true;
        }
    });
    sync_debug_props();
    Ok(())
}

/// A per-view renderer-instance id (production `rendererInstanceId`).
fn new_instance_id() -> String {
    let now = js_sys::Number::from(js_sys::Date::now())
        .to_string_with_radix(36)
        .unwrap_or_default();
    let random = js_sys::Number::from(js_sys::Math::random())
        .to_string_with_radix(36)
        .unwrap_or_default();
    let suffix = random.slice(2, random.length());
    format!("{now}-{suffix}")
}

/// WASM startup entry: install the Rust shell (listener before
/// `webviewReady`) and mark the per-row SVG renderer ready. No canvas
/// surface is created on this path — the graph lives inside the scrolling
/// row DOM, so there is nothing to position or chase.
///
/// # Errors
///
/// Returns an error when the shell scaffold or the VS Code API is
/// unavailable.
#[wasm_bindgen(js_name = "startHistoryView")]
pub fn start_history_view() -> Result<(), JsValue> {
    install_shell()?;
    SHELL_DATA.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(shell) = borrow.as_mut() else {
            return;
        };
        shell.diagnostics.renderer_ready = true;
        shell.publish_render_state();
    });
    set_window_prop("__editchainLastError", &JsValue::NULL);
    sync_debug_props();
    Ok(())
}

pub use diagnostics::{
    debug_backend, debug_data_ready, debug_find_state, debug_generation, debug_graph_state,
    debug_in_flight_count, debug_lane_x_all, debug_metrics, debug_render_count,
    debug_renderer_instance_id, debug_row_at, debug_snapshot, debug_total, debug_view_gen,
};
