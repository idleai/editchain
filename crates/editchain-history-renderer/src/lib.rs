//! Rust/WebAssembly history renderer for the VS Code history view.
//!
//! The webview is owned end-to-end by Rust/WASM: it manages the host protocol,
//! virtualized rows, interaction state, DOM, and per-row SVG graph fragments.

#[cfg(any(target_arch = "wasm32", test))]
mod app;

#[cfg(target_arch = "wasm32")]
mod shell {
    //! Rust-owned browser shell (Slice 3A).
    //!
    //! Owns startup sequencing (VS Code API acquisition through the narrow
    //! wasm-bindgen binding, message-listener installation before
    //! `webviewReady`, state restore/save), the `HistoryAppState` machine, the
    //! reducer `Step` sends and DOM ops, scroll/search controls and persistence,
    //! the debug hooks, and the per-row SVG graph render pass.
    //!
    //! Reentrancy contract: the fixture bridge dispatches correlated
    //! responses synchronously inside `postMessage`, so host messages are
    //! queued and drained by [`pump_messages`] with sends executed only after
    //! the shell borrow is released; DOM ops are applied before sends within
    //! each transition so nested response steps never reorder the window.

    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use serde_json::{json, Value};
    use wasm_bindgen::prelude::*;

    use crate::app::dom::{self, ColKey, HistoryDom};
    use crate::app::host::{self, Send};
    use crate::app::rows::{self, RowContext, RowSpec};
    use crate::app::state::{DomOp, HistoryAppState, RetryAction, Step, Viewport, ROW_H};

    /// Narrow wasm-bindgen binding for VS Code API acquisition: the webview
    /// host (and the harness fixture bridge) provide `acquireVsCodeApi` as a
    /// window global. Every later API call goes through [`vscode_call`].
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = "acquireVsCodeApi", catch)]
        fn acquire_vscode_api() -> Result<JsValue, JsValue>;
    }

    thread_local! {
        /// Pending host messages (structured-clone data payloads).
        static MSG_QUEUE: RefCell<VecDeque<JsValue>> = const { RefCell::new(VecDeque::new()) };
        /// True while the pump drains the queue (reentrancy guard for the
        /// synchronous fixture bridge dispatch inside `postMessage`).
        static PUMP_ACTIVE: Cell<bool> = const { Cell::new(false) };
        /// Host envelopes queued for deferred dispatch (see
        /// [`schedule_post_flush`]: wasm-bindgen `Closure`s reject recursive
        /// invocation, and the harness fixture bridge dispatches correlated
        /// responses synchronously inside `postMessage`, so posts must never
        /// happen inside a listener closure's own stack).
        static POST_QUEUE: RefCell<VecDeque<Value>> = const { RefCell::new(VecDeque::new()) };
        /// True while the deferred-post flush future is scheduled/running.
        static FLUSH_ACTIVE: Cell<bool> = const { Cell::new(false) };
        /// The live shell; created once at startup.
        static SHELL_DATA: RefCell<Option<ShellData>> = const { RefCell::new(None) };
        /// Active column-divider drag (window-level mousemove/mouseup state).
        static COLUMN_DRAG: RefCell<Option<ColumnDrag>> = const { RefCell::new(None) };
        /// True while a transition (or the message pump) holds the shell
        /// borrow. Synchronous DOM events such as `focusin` can fire inside
        /// one (e.g. `element.focus()` restores focus after a rebuild); nested
        /// transitions are skipped because the outer transition already owns
        /// the state change.
        static TRANSITION_ACTIVE: Cell<bool> = const { Cell::new(false) };
    }

    /// Boolean shell state, grouped under the `struct_excessive_bools` gate.
    #[derive(Debug, Default, Clone, Copy)]
    struct ShellFlags {
        /// The per-row SVG graph renderer is ready (first window rendered).
        renderer_ready: bool,
        /// Startup completed (listener installed, webviewReady posted).
        wasm_ready: bool,
    }

    /// The live shell: DOM + state machine + SVG render counters.
    #[derive(Debug)]
    struct ShellData {
        flags: ShellFlags,
        vscode: JsValue,
        dom: HistoryDom,
        state: HistoryAppState,
        instance_id: String,
        /// Successful per-row SVG render passes.
        render_count: u64,
        /// DOM generation counter (the harness `whenIdle` settles on two
        /// stable generations, unchanged).
        generation: u64,
        started_at_ms: f64,
        first_window_ms: Option<f64>,
        last_render_ms: Option<f64>,
        last_frame_rows: Vec<dom::FrameRow>,
        last_error: Option<String>,
        progressive_timer: Option<i32>,
        /// Debounced viewport-resize timer (production `resizeTimer`).
        resize_timer: Option<i32>,
        /// Last observed `#rows` width (the resize observer ignores
        /// height-only notifications, matching `lastRowsWidth`).
        last_rows_width: f64,
        /// Retains the observer for the shell lifetime. Dropping the JS wrapper
        /// permits the browser to collect it and silently stop observations.
        resize_observer: Option<web_sys::ResizeObserver>,
        /// User-dragged column widths (divider state; `None` = natural).
        col_widths: dom::ColWidths,
    }

    /// An active column-divider drag (window-level listeners persist for the
    /// whole gesture; `move`/`up` closures remove themselves on mouseup).
    #[derive(Debug)]
    struct ColumnDrag {
        col: ColKey,
        start_x: f64,
        start_w: f64,
        move_closure: Option<Closure<dyn FnMut(web_sys::Event)>>,
        up_closure: Option<Closure<dyn FnMut(web_sys::Event)>>,
    }

    impl ShellData {
        fn layout(&self) -> dom::GraphLayout {
            dom::graph_layout(
                self.state.max_lane,
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
            self.dom.reanchor(&specs, &col_style, &options)?;
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
                self.state.roving_abs = anchor;
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
            let expandable = self.state.cache.get(&abs).is_some_and(rows::has_sub_ops);
            if expandable {
                self.state.toggle_expanded_ui(abs, viewport, step);
            }
        }

        /// Post the exact advertised file-change identity for host-side native
        /// diff materialization. Returns whether this row is a file row.
        fn open_diff_for_abs(&self, abs: i64, step: &mut Step) -> bool {
            let Some(row) = self.state.cache.get(&abs) else {
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
            let Some(row) = self.state.cache.get(&abs) else {
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
                let Some(row) = self.state.cache.get(&abs) else {
                    continue;
                };
                let prev_group = self.previous_rendered_group(abs);
                let group = host::row::str(row, "group");
                let is_group_start = prev_group.as_deref().is_none_or(|prev| prev != group);
                let context = self.row_context(abs, is_group_start);
                let spec = RowSpec::from_value(row, &context);
                self.dom.replace_row_abs(abs, &spec, &col_style, &graph)?;
            }
            Ok(())
        }

        fn previous_rendered_group(&self, abs: i64) -> Option<String> {
            let prev = self.dom.previous_row_abs(abs)?;
            self.state
                .cache
                .get(&prev)
                .map(|row| host::row::owned_str(row, "group"))
        }

        fn last_rendered_group(&self) -> Option<String> {
            let abs = self.dom.last_row_abs()?;
            self.state
                .cache
                .get(&abs)
                .map(|row| host::row::owned_str(row, "group"))
        }

        fn refresh_header(&mut self) -> Result<(), JsValue> {
            let col_style = self.col_style();
            let graph_width = self.current_graph_width();
            self.dom.refresh_header(&col_style, graph_width)
        }

        /// Apply one reducer DOM op.
        fn apply_op(&mut self, op: &DomOp) -> Result<(), JsValue> {
            match op {
                DomOp::ShowMessage { text, error } => self.dom.show_message(text, *error),
                DomOp::ShowRequestError { text, retry } => {
                    let button = self.dom.show_request_error(text)?;
                    let retry = *retry;
                    let closure = Closure::<dyn FnMut()>::wrap(Box::new(move || {
                        on_retry(retry);
                    }));
                    button.add_event_listener_with_callback(
                        "click",
                        closure.as_ref().unchecked_ref(),
                    )?;
                    closure.forget();
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
                    let Some(row) = self.state.cache.get(abs) else {
                        return Ok(());
                    };
                    let node_key = host::row::owned_str(row, "node_key");
                    self.state.selected_key = Some(node_key);
                    self.dom.apply_selection(*abs)?;
                    self.dom.set_find_highlight(*abs)
                }
                DomOp::ClearFindHighlight => {
                    self.state.clear_selection();
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
            self.apply_roving_tabindex();
            self.sync_find_nav();
        }

        fn start_progressive_loader(&mut self) {
            if self.progressive_timer.is_some() {
                return;
            }
            let Some(window) = web_sys::window() else {
                return;
            };
            let closure = Closure::<dyn FnMut()>::wrap(Box::new(on_progressive_tick));
            let result = window.set_interval_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                250,
            );
            match result {
                Ok(handle) => {
                    self.progressive_timer = Some(handle);
                    closure.forget();
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
            if let Some(handle) = self.progressive_timer.take() {
                if let Some(window) = web_sys::window() {
                    window.clear_interval_with_handle(handle);
                }
            }
        }

        /// Publish debug and accessibility state after SVG rows change.
        fn publish_render_state(&mut self) {
            let host_height = self.dom.client_height_css();
            let rows = dom::frame_rows(&self.state, self.dom.scroll_top(), host_height);
            let started = performance_now();
            self.last_frame_rows.clone_from(&rows);
            self.render_count = self.render_count.saturating_add(1);
            self.generation = self.generation.saturating_add(1);
            self.last_render_ms = Some(performance_now() - started);
            if self.first_window_ms.is_none() {
                self.first_window_ms = Some(performance_now() - self.started_at_ms);
            }
            self.last_error = None;
            set_window_prop("__editchainLastError", &JsValue::NULL);
            let total = self.state.total.unwrap_or(0);
            self.dom
                .set_status(&format!("{} / {} rows", rows.len(), total));
        }

        /// Persist the step's save-state through the narrow binding.
        fn persist(&self, state: &Value) {
            let Some(js) = js_sys::JSON::parse(&state.to_string()).ok() else {
                return;
            };
            let vscode = self.vscode.clone();
            let result = set_state_(&vscode, &js);
            if let Err(error) = result {
                record_error(&format!("setState failed: {}", js_value_text(&error)));
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
    fn set_window_prop(name: &str, value: &JsValue) {
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

    /// Record a shell error: console + `__editchainLastError` + shell field.
    fn record_error(message: &str) {
        web_sys::console::error_1(&JsValue::from_str(message));
        set_window_prop("__editchainLastError", &JsValue::from_str(message));
        SHELL_DATA.with(|cell| {
            if let Some(shell) = cell.borrow_mut().as_mut() {
                shell.last_error = Some(message.to_owned());
            }
        });
    }

    /// Parse one host message's structured-clone data into JSON.
    fn message_value(data: &JsValue) -> Value {
        let string = js_sys::JSON::stringify(data)
            .map(|js| js.as_string().unwrap_or_default())
            .unwrap_or_default();
        serde_json::from_str(&string).unwrap_or(Value::Null)
    }

    /// Render a JS value as diagnostic text.
    fn js_value_text(value: &JsValue) -> String {
        js_sys::JSON::stringify(value)
            .map(|js| js.as_string().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Call one method on the acquired VS Code API through `Reflect` (the
    /// narrow binding surface: acquisition is wasm-bindgen, dispatch is
    /// structural).
    fn vscode_call(api: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
        let function: js_sys::Function =
            js_sys::Reflect::get(api, &JsValue::from_str(method))?.dyn_into()?;
        let arguments: js_sys::Array = args.iter().collect();
        js_sys::Reflect::apply(&function, api, &arguments)
    }

    /// `vscode.postMessage(message)`.
    fn post_message_(api: &JsValue, message: &JsValue) -> Result<(), JsValue> {
        vscode_call(api, "postMessage", std::slice::from_ref(message)).map(|_| ())
    }

    /// `vscode.getState()` (best-effort: returns `undefined` on failure).
    fn get_state_(api: &JsValue) -> JsValue {
        vscode_call(api, "getState", &[]).unwrap_or(JsValue::UNDEFINED)
    }

    /// `vscode.setState(state)`.
    fn set_state_(api: &JsValue, state: &JsValue) -> Result<(), JsValue> {
        vscode_call(api, "setState", std::slice::from_ref(state)).map(|_| ())
    }

    /// Post one host envelope through the narrow binding.
    fn post_envelope(envelope: &Value) {
        POST_QUEUE.with(|queue| queue.borrow_mut().push_back(envelope.clone()));
        schedule_post_flush();
    }

    /// Queue the deferred host-post flush on the microtask queue. Each
    /// `postMessage` runs in its own microtask, so the fixture bridge's
    /// synchronous response dispatch can never re-enter a wasm-bindgen
    /// `Closure` that is still on the stack.
    fn schedule_post_flush() {
        if FLUSH_ACTIVE.with(Cell::get) {
            return;
        }
        FLUSH_ACTIVE.with(|cell| cell.set(true));
        wasm_bindgen_futures::spawn_local(async {
            loop {
                // Yield first: the flush must never post from inside the
                // closure stack that queued the envelope.
                let yielded = js_sys::Promise::resolve(&JsValue::UNDEFINED);
                let _resolved: Result<JsValue, JsValue> =
                    wasm_bindgen_futures::JsFuture::from(yielded).await;
                let envelope = POST_QUEUE.with(|queue| queue.borrow_mut().pop_front());
                let Some(envelope) = envelope else {
                    FLUSH_ACTIVE.with(|cell| cell.set(false));
                    break;
                };
                let parsed = js_sys::JSON::parse(&envelope.to_string()).ok();
                let Some(message) = parsed else {
                    continue;
                };
                let vscode = SHELL_DATA.with(|cell| {
                    cell.borrow_mut()
                        .as_mut()
                        .map_or(JsValue::UNDEFINED, |shell| shell.vscode.clone())
                });
                let posted = post_message_(&vscode, &message);
                if let Err(error) = posted {
                    record_error(&format!("postMessage failed: {}", js_value_text(&error)));
                }
            }
        });
    }

    fn js_value_error(message: &str) -> JsValue {
        JsValue::from_str(message)
    }

    /// Execute one host send (called with NO shell borrow held — the fixture
    /// bridge may re-enter the listener synchronously).
    fn execute_send(send: &Send) {
        match send {
            Send::RefreshHistory => post_envelope(&json!({ "type": "refreshHistory" })),
            Send::Request { id, body } => {
                let envelope = json!({ "id": *id, "body": body });
                post_envelope(&envelope);
            }
            Send::Log(text) => {
                web_sys::console::info_1(&JsValue::from_str(text));
                post_envelope(&json!({ "type": "log", "text": text }));
            }
            Send::Status { loaded, total } => {
                post_envelope(&json!({ "type": "status", "loaded": *loaded, "total": *total }));
            }
            Send::StatusText(text) => {
                SHELL_DATA.with(|cell| {
                    if let Some(shell) = cell.borrow_mut().as_mut() {
                        shell.dom.announce(text);
                    }
                });
                post_envelope(&json!({ "type": "statusText", "text": text }));
            }
            Send::OpenJson(body) | Send::OpenDiff(body) => {
                post_envelope(body);
            }
            Send::WebviewReady(instance_id) => {
                post_envelope(&json!({ "type": "webviewReady", "instanceId": instance_id }));
            }
        }
    }

    /// Execute a step's sends (no shell borrow held).
    fn execute_sends(sends: Vec<Send>) {
        for send in sends {
            execute_send(&send);
        }
    }

    /// One bounded transition's host output.
    #[derive(Debug, Default)]
    struct TransitionOutput {
        sends: Vec<Send>,
        save_state: Option<Value>,
    }

    /// Run one bounded transition: borrow the shell, capture the pre-window,
    /// mutate state, apply DOM ops, and return the sends to post. The sends
    /// execute with no borrow held (responses may re-enter synchronously), the
    /// step's save-state persists after them, and a renderer frame is scheduled
    /// once the DOM settled.
    fn run_transition(transition: impl FnOnce(&mut ShellData) -> TransitionOutput) {
        if TRANSITION_ACTIVE.with(Cell::get) {
            // A synchronous DOM event (e.g. `focusin` from `element.focus()`)
            // fired inside another transition; the outer transition already
            // owns the state change, so a nested borrow would panic.
            return;
        }
        TRANSITION_ACTIVE.with(|cell| cell.set(true));
        let mut output = SHELL_DATA.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(shell) = borrow.as_mut() else {
                return TransitionOutput::default();
            };
            transition(shell)
        });
        let sends = std::mem::take(&mut output.sends);
        execute_sends(sends);
        if let Some(save_state) = output.save_state.take() {
            SHELL_DATA.with(|cell| {
                if let Some(shell) = cell.borrow_mut().as_mut() {
                    shell.persist(&save_state);
                }
            });
        }
        after_transition();
        TRANSITION_ACTIVE.with(|cell| cell.set(false));
    }

    /// Host message listener: queue the payload and drain the pump. The
    /// fixture bridge dispatches responses synchronously inside `postMessage`,
    /// so this may re-enter while sends are executing; the queue keeps the
    /// ordering deterministic.
    fn on_message_event(event: web_sys::Event) {
        let message: web_sys::MessageEvent = event.unchecked_into();
        MSG_QUEUE.with(|queue| queue.borrow_mut().push_back(message.data()));
        pump_messages();
    }

    /// Scroll handler: keep the bounded window in sync with the viewport.
    fn on_scroll() {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            shell.state.sync_window(&viewport, &mut step);
            shell.state.fetch_window(&viewport, &mut step);
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Terminal request-error Retry button.
    fn on_retry(action: RetryAction) {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            match action {
                RetryAction::ResetHistory => shell.state.reset_history(&viewport, &mut step),
                RetryAction::RefreshSnapshot => shell.state.refresh_snapshot(&viewport, &mut step),
            }
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Progressive loader tick: buffer history ahead of the scroll position.
    fn on_progressive_tick() {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            shell.state.fetch_window(&viewport, &mut step);
            shell.state.sync_window(&viewport, &mut step);
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Exit the in-place find interaction without changing the history view.
    fn exit_search() {
        run_transition(|shell| {
            let mut step = Step::new();
            shell.state.clear_find(&mut step);
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Find-in-chain keyboard behaviour:
    /// Enter submits / advances the same settled query; Shift+Enter steps
    /// back; Escape/empty clears; ArrowDown/Up navigate settled matches for
    /// the exact submitted query.
    fn on_search_keydown(event: &web_sys::KeyboardEvent) {
        let key = event.key();
        let trimmed = SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|shell| shell.dom.search_input_value())
                .unwrap_or_default()
        });
        if key == "Enter" {
            if trimmed.is_empty() {
                exit_search();
                return;
            }
            let same_settled = SHELL_DATA.with(|cell| {
                cell.borrow().as_ref().is_some_and(|shell| {
                    shell.state.find_active() && trimmed == shell.state.search_query
                })
            });
            if same_settled {
                event.prevent_default();
                let delta: isize = if event.shift_key() { -1 } else { 1 };
                run_transition(|shell| {
                    let viewport = shell.dom.viewport();
                    let mut step = Step::new();
                    shell.state.navigate_find(delta, &viewport, &mut step);
                    shell.apply_step_ops(&step);
                    TransitionOutput {
                        sends: std::mem::take(&mut step.sends),
                        save_state: step.save_state.take(),
                    }
                });
            } else {
                run_transition(|shell| {
                    let mut step = Step::new();
                    shell.state.submit_find(&trimmed, &mut step);
                    shell.apply_step_ops(&step);
                    TransitionOutput {
                        sends: std::mem::take(&mut step.sends),
                        save_state: step.save_state.take(),
                    }
                });
            }
            return;
        }
        if key == "Escape" {
            event.prevent_default();
            exit_search();
            return;
        }
        if key == "ArrowDown" || key == "ArrowUp" {
            let navigable = SHELL_DATA.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .is_some_and(|shell| shell.state.find_navigation_enabled(&trimmed))
            });
            if navigable {
                event.prevent_default();
                let delta: isize = if key == "ArrowDown" { 1 } else { -1 };
                run_transition(|shell| {
                    let viewport = shell.dom.viewport();
                    let mut step = Step::new();
                    shell.state.navigate_find(delta, &viewport, &mut step);
                    shell.apply_step_ops(&step);
                    TransitionOutput {
                        sends: std::mem::take(&mut step.sends),
                        save_state: step.save_state.take(),
                    }
                });
            }
        }
    }

    /// `input` handler: an emptied input exits the find interaction;
    /// edited-but-unsubmitted text disables the nav buttons (the guard lives
    /// in the shell's `sync_find_nav`).
    fn on_search_input() {
        run_transition(|shell| {
            let mut step = Step::new();
            if shell.dom.search_input_value().is_empty() {
                shell.state.clear_find(&mut step);
            }
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Previous/Next find-nav click: the same wrapping `navigateFind` path the
    /// arrows use, then focus returns to the input so editing stays immediate.
    fn on_search_nav(delta: isize) {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            shell.state.navigate_find(delta, &viewport, &mut step);
            shell.apply_step_ops(&step);
            drop(shell.dom.search_input().focus());
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// The absolute index of the closest `.row` to an event target, if any.
    fn closest_row_abs(target: &web_sys::EventTarget) -> Option<i64> {
        let element: web_sys::Element = target.clone().dyn_into().ok()?;
        let row = element.closest(".row").ok().flatten()?;
        row.get_attribute("data-row")?.trim().parse::<i64>().ok()
    }

    /// Whether the event target is inside `selector` (delegation guards).
    fn target_inside(target: &web_sys::EventTarget, selector: &str) -> bool {
        target
            .clone()
            .dyn_into::<web_sys::Element>()
            .ok()
            .and_then(|element| element.closest(selector).ok().flatten())
            .is_some()
    }

    /// Chevron or ordinary-row click: select the row; expandable rows toggle
    /// their disclosure (the detail guard lives in the click handler).
    fn on_row_click(event: &web_sys::Event) {
        let mouse: web_sys::MouseEvent = (*event).clone().unchecked_into();
        let Some(target) = event.target() else {
            return;
        };
        let Some(abs) = closest_row_abs(&target) else {
            return; // header / handles / spacer are never row targets
        };
        let chevron = target_inside(&target, ".subop-chevron");
        let in_button = target_inside(&target, "button");
        if chevron {
            mouse.prevent_default();
            mouse.stop_propagation();
        } else if in_button || mouse.detail() > 1 {
            return;
        }
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            shell.row_select_and_toggle(abs, &viewport, &mut step);
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Double-click on an ordinary row opens raw JSON. File rows already open
    /// their native diff on the first click and must never replace it with the
    /// normalized operation JSON on the second click.
    fn on_row_dblclick(event: &web_sys::Event) {
        let Some(target) = event.target() else {
            return;
        };
        if target_inside(&target, "button") || target_inside(&target, ".row-file") {
            return;
        }
        let Some(abs) = closest_row_abs(&target) else {
            return;
        };
        run_transition(|shell| {
            let mut step = Step::new();
            shell.state.select_row(abs);
            if let Err(error) = shell.dom.apply_selection(abs) {
                record_error(&format!(
                    "selection apply failed: {}",
                    js_value_text(&error)
                ));
            }
            shell.open_json_for_abs(abs, &mut step);
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// `focusin`: any row receiving focus becomes the roving anchor.
    fn on_row_focusin(event: &web_sys::Event) {
        let Some(target) = event.target() else {
            return;
        };
        let Some(abs) = closest_row_abs(&target) else {
            return;
        };
        run_transition(|shell| {
            if abs != shell.state.roving_abs {
                shell.state.set_roving_abs(abs);
                shell.apply_roving_tabindex();
            }
            TransitionOutput::default()
        });
    }

    /// Roving keyboard navigation over the rendered rows: ArrowUp/Down move
    /// focus, Home/End jump to the window
    /// edges, ArrowRight/ArrowLeft toggle expandable rows, Enter/Space
    /// activate (disclosure for expandable rows, raw JSON for ordinary ones).
    fn on_row_keydown(event: &web_sys::KeyboardEvent) {
        let key = event.key();
        let Some(target) = event.target() else {
            return;
        };
        let Some(abs) = closest_row_abs(&target) else {
            return;
        };
        match key.as_str() {
            "ArrowDown" | "ArrowUp" | "Home" | "End" => {
                let direction = if key == "ArrowDown" {
                    1
                } else if key == "ArrowUp" {
                    -1
                } else {
                    0
                };
                let home_end = key == "Home" || key == "End";
                run_transition(|shell| {
                    let Some(wrap) = shell.dom.wrap() else {
                        return TransitionOutput::default();
                    };
                    let Ok(list) = wrap.query_selector_all(".row") else {
                        return TransitionOutput::default();
                    };
                    let length = list.length();
                    if length == 0 {
                        return TransitionOutput::default();
                    }
                    let rows_capacity = usize::try_from(length).unwrap_or(usize::MAX);
                    let mut rows = Vec::with_capacity(rows_capacity);
                    for index in 0..length {
                        let item = list.item(index);
                        let Some(node) = item else {
                            continue;
                        };
                        let Some(element) = node.dyn_into::<web_sys::Element>().ok() else {
                            continue;
                        };
                        let Some(row_abs) = element
                            .get_attribute("data-row")
                            .and_then(|raw| raw.trim().parse::<i64>().ok())
                        else {
                            continue;
                        };
                        rows.push((row_abs, element));
                    }
                    if rows.is_empty() {
                        return TransitionOutput::default();
                    }
                    let current = rows.iter().position(|(row_abs, _)| *row_abs == abs);
                    let len = rows.len();
                    let next = if home_end {
                        if key == "Home" {
                            0
                        } else {
                            len.saturating_sub(1)
                        }
                    } else {
                        let current = current.unwrap_or(0);
                        let moved = if direction > 0 {
                            current.checked_add(1)
                        } else {
                            current.checked_sub(1)
                        };
                        if let Some(moved) = moved.filter(|index| *index < len) {
                            moved
                        } else {
                            return TransitionOutput::default();
                        }
                    };
                    let Some((target_abs, element)) = rows.get(next) else {
                        return TransitionOutput::default();
                    };
                    event.prevent_default();
                    let Some(target) = element.dyn_ref::<web_sys::HtmlElement>() else {
                        return TransitionOutput::default();
                    };
                    shell.state.set_roving_abs(*target_abs);
                    shell.apply_roving_tabindex();
                    drop(target.focus());
                    TransitionOutput::default()
                });
            }
            "ArrowRight" | "ArrowLeft" => {
                let expanded = SHELL_DATA.with(|cell| {
                    cell.borrow()
                        .as_ref()
                        .is_some_and(|shell| shell.state.is_row_expanded(abs))
                });
                let wants_toggle = SHELL_DATA.with(|cell| {
                    cell.borrow().as_ref().is_some_and(|shell| {
                        let row = shell.state.cache.get(&abs);
                        let Some(row) = row else {
                            return false;
                        };
                        if !rows::has_sub_ops(row) {
                            return false;
                        }
                        (key == "ArrowRight" && !expanded) || (key == "ArrowLeft" && expanded)
                    })
                });
                if wants_toggle {
                    event.prevent_default();
                    toggle_row_disclosure(abs);
                }
            }
            "Enter" | " " => {
                if target_inside(&target, "button") {
                    return; // native button activation handles it
                }
                let expandable = SHELL_DATA.with(|cell| {
                    cell.borrow().as_ref().is_some_and(|shell| {
                        shell.state.cache.get(&abs).is_some_and(rows::has_sub_ops)
                    })
                });
                if expandable {
                    event.prevent_default();
                    toggle_row_disclosure(abs);
                    return;
                }
                event.prevent_default();
                run_transition(|shell| {
                    let mut step = Step::new();
                    shell.state.select_row(abs);
                    if let Err(error) = shell.dom.apply_selection(abs) {
                        record_error(&format!(
                            "selection apply failed: {}",
                            js_value_text(&error)
                        ));
                    }
                    if key == "Enter" && !shell.open_diff_for_abs(abs, &mut step) {
                        shell.open_json_for_abs(abs, &mut step);
                    }
                    shell.apply_step_ops(&step);
                    TransitionOutput {
                        sends: std::mem::take(&mut step.sends),
                        save_state: step.save_state.take(),
                    }
                });
            }
            _ => {}
        }
    }

    /// `toggleDisclosureKeyboard` — toggle a row's reveal state, rebuild the
    /// desired window, and re-focus the fresh parent row so keyboard focus
    /// survives the DOM replacement.
    fn toggle_row_disclosure(abs: i64) {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            shell.state.toggle_expanded_ui(abs, &viewport, &mut step);
            shell.apply_step_ops(&step);
            let selector = format!(".row[data-row=\"{abs}\"]");
            if let Ok(Some(fresh)) = shell.dom.rows().query_selector(&selector) {
                if let Ok(fresh) = fresh.dyn_into::<web_sys::HtmlElement>() {
                    drop(fresh.focus());
                }
            }
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// `mousedown` on a `.col-resize-handle` starts the column drag.
    fn on_rows_mousedown(event: &web_sys::Event) {
        let mouse: web_sys::MouseEvent = (*event).clone().unchecked_into();
        let Some(target) = event.target() else {
            return;
        };
        let Some(element) = target.clone().dyn_into::<web_sys::Element>().ok() else {
            return;
        };
        let Some(handle) = element.closest(".col-resize-handle").ok().flatten() else {
            return;
        };
        let Some(col) = handle
            .get_attribute("data-col")
            .and_then(|raw| ColKey::parse(&raw))
        else {
            return;
        };
        mouse.prevent_default();
        start_column_drag(col, f64::from(mouse.client_x()));
    }

    /// The current effective width of a resizable column (drag start).
    fn column_start_width(shell: &ShellData, col: ColKey) -> f64 {
        match col {
            ColKey::Graph => shell.current_graph_width(),
            ColKey::Activity
            | ColKey::Tags
            | ColKey::Content
            | ColKey::Date
            | ColKey::Author
            | ColKey::Commit => shell.dom.header_cell_width(col).max(col.min_width()),
        }
    }

    /// Begin a divider drag: pin the start geometry, mark the body, and
    /// install window-level move/up listeners (removed on mouseup).
    fn start_column_drag(col: ColKey, client_x: f64) {
        let start_w = SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .map_or(0.0, |shell| column_start_width(shell, col))
        });
        let Some(window) = web_sys::window() else {
            return;
        };
        let move_closure =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_column_move(&event);
            }));
        let up_closure =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_column_up(&event);
            }));
        drop(
            window.add_event_listener_with_callback(
                "mousemove",
                move_closure.as_ref().unchecked_ref(),
            ),
        );
        drop(
            window.add_event_listener_with_callback("mouseup", up_closure.as_ref().unchecked_ref()),
        );
        let body = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.body());
        if let Some(body) = body {
            drop(body.class_list().add_1("col-resizing"));
        }
        COLUMN_DRAG.with(|cell| {
            *cell.borrow_mut() = Some(ColumnDrag {
                col,
                start_x: client_x,
                start_w,
                move_closure: Some(move_closure),
                up_closure: Some(up_closure),
            });
        });
    }

    /// Drag move: update the dragged column's width and re-render the current
    /// window so the grid tracks the mouse.
    fn on_column_move(event: &web_sys::Event) {
        let mouse: web_sys::MouseEvent = (*event).clone().unchecked_into();
        let Some((col, start_x, start_w)) = COLUMN_DRAG.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|drag| (drag.col, drag.start_x, drag.start_w))
        }) else {
            return;
        };
        let next = (start_w + f64::from(mouse.client_x()) - start_x).max(col.min_width());
        run_transition(|shell| {
            shell.col_widths.set(col, Some(next));
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            let (top, bottom) = (shell.state.render_top, shell.state.render_bottom);
            step.ops.push(DomOp::Reanchor { top, bottom });
            shell.state.sync_window(&viewport, &mut step);
            shell.state.fetch_window(&viewport, &mut step);
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Drag up: remove the window listeners and the resizing body class.
    fn on_column_up(_event: &web_sys::Event) {
        let drag = COLUMN_DRAG.with(|cell| cell.borrow_mut().take());
        let Some(drag) = drag else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        if let Some(move_closure) = &drag.move_closure {
            drop(window.remove_event_listener_with_callback(
                "mousemove",
                move_closure.as_ref().unchecked_ref(),
            ));
        }
        if let Some(up_closure) = &drag.up_closure {
            drop(window.remove_event_listener_with_callback(
                "mouseup",
                up_closure.as_ref().unchecked_ref(),
            ));
        }
        let body = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.body());
        if let Some(body) = body {
            drop(body.class_list().remove_1("col-resizing"));
        }
    }

    /// `ResizeObserver` / window-resize entry: ignore height-only changes, then
    /// debounce the full layout re-render (`onViewportResize`, 150ms).
    fn on_resize_observed() {
        let changed = SHELL_DATA.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(shell) = borrow.as_mut() else {
                return false;
            };
            let width = f64::from(shell.dom.rows().client_width());
            if (width - shell.last_rows_width).abs() < 0.5 {
                return false;
            }
            shell.last_rows_width = width;
            if let Some(handle) = shell.resize_timer.take() {
                if let Some(window) = web_sys::window() {
                    window.clear_timeout_with_handle(handle);
                }
            }
            true
        });
        if !changed {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        let closure = Closure::<dyn FnMut()>::wrap(Box::new(on_resize_debounced));
        let scheduled = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            150,
        );
        match scheduled {
            Ok(handle) => {
                closure.forget();
                SHELL_DATA.with(|cell| {
                    if let Some(shell) = cell.borrow_mut().as_mut() {
                        shell.resize_timer = Some(handle);
                    }
                });
            }
            Err(error) => {
                let message = format!(
                    "resize debounce failed to schedule: {}",
                    js_value_text(&error)
                );
                record_error(&message);
            }
        }
    }

    /// The debounced viewport-resize rebuild (`onViewportResize`): re-render
    /// the current window at the new width, then re-sync/fetch as needed.
    fn on_resize_debounced() {
        SHELL_DATA.with(|cell| {
            if let Some(shell) = cell.borrow_mut().as_mut() {
                shell.resize_timer = None;
            }
        });
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            step.ops.push(DomOp::Reanchor {
                top: shell.state.render_top,
                bottom: shell.state.render_bottom,
            });
            shell.state.sync_window(&viewport, &mut step);
            shell.state.fetch_window(&viewport, &mut step);
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Install the read-only harness hooks (`__editchainGetTotal`,
    /// `__editchainRowAt`). These are pure facades over
    /// the live shell — no JS app state or business logic lives here.
    fn install_parity_hooks() {
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
                let Some(row) = shell.state.cache.get(&index) else {
                    return JsValue::NULL;
                };
                js_sys::JSON::parse(&row.to_string()).unwrap_or(JsValue::NULL)
            })
        }));
        set_window_prop("__editchainGetTotal", get_total.as_ref().unchecked_ref());
        set_window_prop("__editchainRowAt", row_at.as_ref().unchecked_ref());
        // The window props hold the JS functions; the wasm closures leak
        // deliberately for the shell's lifetime.
        get_total.forget();
        row_at.forget();
    }

    /// Drain queued host messages and any synchronous responses they trigger.
    fn pump_messages() {
        if PUMP_ACTIVE.with(Cell::get) || TRANSITION_ACTIVE.with(Cell::get) {
            return;
        }
        PUMP_ACTIVE.with(|cell| cell.set(true));
        TRANSITION_ACTIVE.with(|cell| cell.set(true));
        loop {
            let message = MSG_QUEUE.with(|queue| queue.borrow_mut().pop_front());
            let Some(message) = message else {
                break;
            };
            let mut output = SHELL_DATA.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let Some(shell) = borrow.as_mut() else {
                    return TransitionOutput::default();
                };
                let Some(parsed) = host::HostMessage::parse(&message_value(&message)) else {
                    return TransitionOutput::default();
                };
                let viewport = shell.dom.viewport();
                let mut step = Step::new();
                shell
                    .state
                    .handle_host_message(parsed, &viewport, &mut step);
                shell.apply_step_ops(&step);
                TransitionOutput {
                    sends: std::mem::take(&mut step.sends),
                    save_state: step.save_state.take(),
                }
            });
            let sends = std::mem::take(&mut output.sends);
            execute_sends(sends);
            if let Some(save_state) = output.save_state.take() {
                SHELL_DATA.with(|cell| {
                    if let Some(shell) = cell.borrow_mut().as_mut() {
                        shell.persist(&save_state);
                    }
                });
            }
        }
        PUMP_ACTIVE.with(|cell| cell.set(false));
        TRANSITION_ACTIVE.with(|cell| cell.set(false));
        after_transition();
    }

    /// Post-step sync: mirror readiness flags to the window debug properties
    /// and publish the SVG render state after rows changed.
    fn after_transition() {
        SHELL_DATA.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(shell) = borrow.as_mut() else {
                return;
            };
            sync_debug_props_locked(shell);
            shell.publish_render_state();
        });
    }

    /// Mirror shell/state flags onto the window (debug contract).
    fn sync_debug_props() {
        SHELL_DATA.with(|cell| {
            if let Some(shell) = cell.borrow_mut().as_mut() {
                sync_debug_props_locked(shell);
            }
        });
    }

    fn sync_debug_props_locked(shell: &mut ShellData) {
        let ready = shell.state.view_flags.data_ready;
        set_window_prop("__editchainDataReady", &JsValue::from_bool(ready));
        set_window_prop(
            "__editchainInFlightCount",
            &js_sys::Number::from(u32::try_from(shell.state.in_flight.len()).unwrap_or(u32::MAX))
                .into(),
        );
        set_window_prop(
            "__editchainViewGen",
            &js_sys::Number::from(u32::try_from(shell.state.view_gen).unwrap_or(u32::MAX)).into(),
        );
        set_window_prop(
            "__editchainGeneration",
            &js_sys::Number::from(u32::try_from(shell.generation).unwrap_or(u32::MAX)).into(),
        );
        set_window_prop(
            "__editchainRenderCount",
            &js_sys::Number::from(u32::try_from(shell.render_count).unwrap_or(u32::MAX)).into(),
        );
        set_window_prop(
            "__editchainRendererReady",
            &JsValue::from_bool(shell.flags.renderer_ready),
        );
        set_window_prop(
            "__editchainWasmReady",
            &JsValue::from_bool(shell.flags.wasm_ready),
        );
        set_window_prop(
            "__editchainRendererInstanceId",
            &JsValue::from_str(&shell.instance_id),
        );
        set_window_prop("__editchainProfile", &JsValue::from_str("activity"));
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
            flags: ShellFlags::default(),
            vscode,
            dom,
            state,
            instance_id: new_instance_id(),
            render_count: 0,
            generation: 0,
            started_at_ms: performance_now(),
            first_window_ms: None,
            last_render_ms: None,
            last_frame_rows: Vec::new(),
            last_error: None,
            progressive_timer: None,
            resize_timer: None,
            last_rows_width: initial_rows_width,
            resize_observer: None,
            col_widths: dom::ColWidths::default(),
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
        let message_closure =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(on_message_event));
        window.add_event_listener_with_callback(
            "message",
            message_closure.as_ref().unchecked_ref(),
        )?;
        message_closure.forget();
        let scroll_closure = Closure::<dyn FnMut()>::wrap(Box::new(on_scroll));
        rows_el
            .add_event_listener_with_callback("scroll", scroll_closure.as_ref().unchecked_ref())?;
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
        search_input.add_event_listener_with_callback(
            "input",
            search_input_closure.as_ref().unchecked_ref(),
        )?;
        search_input_closure.forget();
        let prev_mousedown =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                let mouse: web_sys::MouseEvent = event.unchecked_into();
                mouse.prevent_default();
            }));
        search_prev.add_event_listener_with_callback(
            "mousedown",
            prev_mousedown.as_ref().unchecked_ref(),
        )?;
        prev_mousedown.forget();
        let next_mousedown =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                let mouse: web_sys::MouseEvent = event.unchecked_into();
                mouse.prevent_default();
            }));
        search_next.add_event_listener_with_callback(
            "mousedown",
            next_mousedown.as_ref().unchecked_ref(),
        )?;
        next_mousedown.forget();
        let prev_click =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_search_nav(-1)));
        search_prev
            .add_event_listener_with_callback("click", prev_click.as_ref().unchecked_ref())?;
        prev_click.forget();
        let next_click = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_search_nav(1)));
        search_next
            .add_event_listener_with_callback("click", next_click.as_ref().unchecked_ref())?;
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
        rows_el
            .add_event_listener_with_callback("dblclick", rows_dblclick.as_ref().unchecked_ref())?;
        rows_dblclick.forget();
        let rows_focusin =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_row_focusin(&event);
            }));
        rows_el
            .add_event_listener_with_callback("focusin", rows_focusin.as_ref().unchecked_ref())?;
        rows_focusin.forget();
        let rows_keydown = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::wrap(Box::new(
            |event: web_sys::KeyboardEvent| on_row_keydown(&event),
        ));
        rows_el
            .add_event_listener_with_callback("keydown", rows_keydown.as_ref().unchecked_ref())?;
        rows_keydown.forget();
        let rows_mousedown =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_rows_mousedown(&event);
            }));
        rows_el.add_event_listener_with_callback(
            "mousedown",
            rows_mousedown.as_ref().unchecked_ref(),
        )?;
        rows_mousedown.forget();

        // Viewport resize: window resize + a ResizeObserver over #rows
        // (production `onViewportResize`, debounced).
        let window_resize =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_resize_observed()));
        window
            .add_event_listener_with_callback("resize", window_resize.as_ref().unchecked_ref())?;
        window_resize.forget();
        let resize_callback = Closure::<
            dyn FnMut(Vec<web_sys::ResizeObserverEntry>, web_sys::ResizeObserver),
        >::wrap(Box::new(|_, _| on_resize_observed()));
        let observer_fn: js_sys::Function = resize_callback
            .as_ref()
            .unchecked_ref::<js_sys::Function>()
            .clone();
        resize_callback.forget();
        match web_sys::ResizeObserver::new(&observer_fn) {
            Ok(resize_observer) => {
                resize_observer.observe(&rows_el);
                SHELL_DATA.with(|cell| {
                    if let Some(shell) = cell.borrow_mut().as_mut() {
                        shell.resize_observer = Some(resize_observer);
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
                shell.flags.wasm_ready = true;
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
            shell.flags.renderer_ready = true;
            shell.last_error = None;
            shell.publish_render_state();
        });
        set_window_prop("__editchainLastError", &JsValue::NULL);
        sync_debug_props();
        Ok(())
    }

    /// `__editchainDataReady` mirror: content has rendered for the view.
    #[wasm_bindgen(js_name = "debugDataReady")]
    #[must_use]
    pub fn debug_data_ready() -> bool {
        SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .is_some_and(|shell| shell.state.view_flags.data_ready)
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
                "currentRow": shell.state.current_find_match().map(|found| found.row),
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
                "layoutReady": shell.state.session_flags.layout_ready,
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
                "initMs": (performance_now() - shell.started_at_ms).max(0.0),
                "firstWindowMs": shell.first_window_ms,
                "lastRenderMs": shell.last_render_ms,
                "renderCount": shell.render_count,
                "domRows": shell.last_frame_rows.len(),
                "generation": shell.generation,
                "rendererReady": shell.flags.renderer_ready,
                "dataReady": shell.state.view_flags.data_ready,
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
                .and_then(|shell| u64::try_from(shell.state.in_flight.len()).ok())
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
                .and_then(|shell| shell.state.cache.get(&abs))
                .map_or_else(|| "null".to_owned(), Value::to_string)
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
        SHELL_DATA.with(|cell| cell.borrow().as_ref().map_or(0, |shell| shell.generation))
    }

    /// Successful per-row SVG render passes.
    #[wasm_bindgen(js_name = "debugRenderCount")]
    #[must_use]
    pub fn debug_render_count() -> u64 {
        SHELL_DATA.with(|cell| cell.borrow().as_ref().map_or(0, |shell| shell.render_count))
    }
}

/// Re-export the wasm shell entry points so the `#[wasm_bindgen]` exports stay
/// reachable (and importable by the generated JS bindings).
#[cfg(target_arch = "wasm32")]
pub use shell::{
    debug_backend, debug_data_ready, debug_find_state, debug_generation, debug_graph_state,
    debug_in_flight_count, debug_lane_x_all, debug_metrics, debug_render_count,
    debug_renderer_instance_id, debug_row_at, debug_snapshot, debug_total, debug_view_gen,
    start_history_view,
};
