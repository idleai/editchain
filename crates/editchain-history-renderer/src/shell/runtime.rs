//! Ordered reducer effects and the browser/host bridge.
//!
//! DOM work finishes under the shell borrow. Sends run after it is released,
//! with each host post deferred to a microtask so synchronous fixture responses
//! cannot recursively invoke an active wasm-bindgen callback.

use super::diagnostics::{js_value_text, record_error, sync_debug_props_locked};
use super::{ShellData, SHELL_DATA};
use crate::app::host::{self, Send};
use crate::app::state::Step;
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
    MSG_QUEUE.with(|queue| queue.borrow_mut().push_back(message.data()));
    pump_messages();
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
