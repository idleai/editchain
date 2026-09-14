//! Ordered reducer effects and the browser/host bridge.
//!
//! DOM work finishes under the shell borrow. Sends run after it is released,
//! with each host post deferred to a microtask so synchronous fixture responses
//! cannot recursively invoke an active wasm-bindgen callback.

use super::diagnostics::{js_value_text, record_error, sync_debug_props_locked};
use super::{ShellData, SHELL_DATA};
use crate::app::host::{self, Send};
use crate::app::state::{FrameBatch, Step};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use wasm_bindgen::prelude::*;

/// Narrow wasm-bindgen binding for VS Code API acquisition: the webview
/// host (and the harness fixture bridge) provide `acquireVsCodeApi` as a
/// window global. Every later API call goes through [`vscode_call`].
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = "acquireVsCodeApi", catch)]
    pub(super) fn acquire_vscode_api() -> Result<JsValue, JsValue>;
}

thread_local! {
    /// Pending host messages, parsed once at the bridge boundary.
    static MSG_QUEUE: RefCell<VecDeque<host::HostMessage>> = const { RefCell::new(VecDeque::new()) };
    /// True while the pump drains the queue (reentrancy guard for the
    /// synchronous fixture bridge dispatch inside `postMessage`).
    static PUMP_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static FRAME_QUEUED: Cell<bool> = const { Cell::new(false) };
    static CONTROL_QUEUED: Cell<bool> = const { Cell::new(false) };
    /// Host envelopes queued for deferred dispatch (see
    /// [`schedule_post_flush`]: wasm-bindgen `Closure`s reject recursive
    /// invocation, and the harness fixture bridge dispatches correlated
    /// responses synchronously inside `postMessage`, so posts must never
    /// happen inside a listener closure's own stack).
    static POST_QUEUE: RefCell<VecDeque<Value>> = const { RefCell::new(VecDeque::new()) };
    /// True while the deferred-post flush future is scheduled/running.
    static FLUSH_ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// True while a transition (or the message pump) holds the shell
    /// borrow. Synchronous DOM events such as `focusin` can fire inside
    /// one (e.g. `element.focus()` restores focus after a rebuild); nested
    /// transitions are skipped because the outer transition already owns
    /// the state change.
    static TRANSITION_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Parse one host message's structured-clone data into JSON.
fn message_value(data: &JsValue) -> Value {
    let string = js_sys::JSON::stringify(data)
        .map(|js| js.as_string().unwrap_or_default())
        .unwrap_or_default();
    serde_json::from_str(&string).unwrap_or(Value::Null)
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
pub(super) fn get_state_(api: &JsValue) -> JsValue {
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

/// Execute one host send (called with NO shell borrow held — the fixture
/// bridge may re-enter the listener synchronously).
pub(super) fn execute_send(send: &Send) {
    match send {
        Send::LiveViewport(viewport) => {
            post_envelope(&json!({ "type": "liveViewport", "viewport": viewport }));
        }
        Send::ToggleDisclosure { key, task } => {
            post_envelope(&json!({ "type": "toggleDisclosure", "key": key, "task": task }));
        }
        Send::LiveSettled { snapshot_id, error } => post_envelope(&json!({
            "type": "liveSettled", "snapshot_id": snapshot_id, "error": error,
        })),
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
pub(super) struct TransitionOutput {
    pub(super) sends: Vec<Send>,
    pub(super) save_state: Option<Value>,
}

/// Run one bounded transition: borrow the shell, capture the pre-window,
/// mutate state, apply DOM ops, and return the sends to post. The sends
/// execute with no borrow held (responses may re-enter synchronously), the
/// step's save-state persists after them, and a renderer frame is scheduled
/// once the DOM settled.
pub(super) fn run_transition(transition: impl FnOnce(&mut ShellData) -> TransitionOutput) {
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
pub(super) fn on_message_event(event: web_sys::Event) {
    let message: web_sys::MessageEvent = event.unchecked_into();
    let parsed = host::HostMessage::parse(&message_value(&message.data()));
    if let Some(message) = parsed {
        MSG_QUEUE.with(|queue| queue.borrow_mut().push_back(message));
        schedule_controls();
    }
}

fn native_control(message: &host::HostMessage) -> bool {
    matches!(
        message.id,
        host::Id::Delta | host::Id::Disclosure | host::Id::DisclosureDone
    )
}

/// A native revision only invalidates coordinates and starts a window request.
/// Let those requests run immediately; waiting for a paint here adds an entire
/// frame before the native service can begin. Never pass an earlier response.
fn schedule_controls() {
    if CONTROL_QUEUED.replace(true) {
        return;
    }
    wasm_bindgen_futures::spawn_local(async {
        CONTROL_QUEUED.set(false);
        let native = SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .is_some_and(|shell| shell.state.reconciles_rows())
        });
        let control = MSG_QUEUE.with(|queue| queue.borrow().front().is_some_and(native_control));
        if native && control {
            pump_messages(true);
        } else if MSG_QUEUE.with(|queue| !queue.borrow().is_empty()) {
            schedule_frame();
        }
    });
}

/// One presentation commit per frame, with a fallback for hidden webviews.
fn schedule_frame() {
    if FRAME_QUEUED.replace(true) {
        return;
    }
    wasm_bindgen_futures::spawn_local(async {
        if let Some(window) = web_sys::window() {
            let timeout = Cell::new(None);
            let animation = Cell::new(None);
            let frame = js_sys::Promise::new(&mut |resolve, _| {
                timeout.set(
                    window
                        .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 50)
                        .ok(),
                );
                animation.set(window.request_animation_frame(&resolve).ok());
                if timeout.get().is_none() && animation.get().is_none() {
                    drop(resolve.call0(&JsValue::UNDEFINED));
                }
            });
            let _completed = wasm_bindgen_futures::JsFuture::from(frame).await;
            if let Some(handle) = timeout.get() {
                window.clear_timeout_with_handle(handle);
            }
            if let Some(handle) = animation.get() {
                drop(window.cancel_animation_frame(handle));
            }
        }
        FRAME_QUEUED.set(false);
        pump_messages(false);
    });
}

/// Reduce every message in order, then reconcile the final window once.
fn pump_messages(controls_only: bool) {
    if PUMP_ACTIVE.with(Cell::get) || TRANSITION_ACTIVE.with(Cell::get) {
        return;
    }
    PUMP_ACTIVE.with(|cell| cell.set(true));
    TRANSITION_ACTIVE.with(|cell| cell.set(true));
    let mut output = SHELL_DATA.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(shell) = borrow.as_mut() else {
            return TransitionOutput::default();
        };
        let mut batch = FrameBatch::new(shell.dom.viewport());
        for _ in 0..64 {
            let next = MSG_QUEUE.with(|queue| {
                let mut queue = queue.borrow_mut();
                if controls_only && !queue.front().is_some_and(native_control) {
                    None
                } else {
                    queue.pop_front()
                }
            });
            let Some(parsed) = next else {
                break;
            };
            let mut step = Step::new();
            shell
                .state
                .handle_host_message(parsed, &batch.viewport, &mut step);
            batch.push(step);
        }
        let mut step = batch.finish(
            shell.state.render_top,
            shell.state.render_bottom,
            shell.state.live_window_pending(),
        );
        step.sends.retain(
            |send| !matches!(send, Send::Request { id, .. } if !shell.state.owns_request(*id)),
        );
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: step.sends,
            save_state: step.save_state,
        }
    });
    execute_sends(std::mem::take(&mut output.sends));
    if let Some(saved) = output.save_state {
        SHELL_DATA.with(|cell| {
            if let Some(shell) = cell.borrow().as_ref() {
                shell.persist(&saved);
            }
        });
    }
    PUMP_ACTIVE.with(|cell| cell.set(false));
    TRANSITION_ACTIVE.with(|cell| cell.set(false));
    after_transition();
    if MSG_QUEUE.with(|queue| !queue.borrow().is_empty()) {
        schedule_frame();
    }
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

impl ShellData {
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
