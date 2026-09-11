//! Browser registrations that can be replaced during the shell's lifetime.
//!
//! Owners unregister callbacks before dropping them. Application listeners
//! installed once at startup remain retained for the full webview lifetime.

use wasm_bindgen::prelude::*;

#[derive(Debug)]
pub(super) struct EventListener {
    target: web_sys::EventTarget,
    event: &'static str,
    callback: Closure<dyn FnMut(web_sys::Event)>,
}

impl EventListener {
    pub(super) fn new(
        target: web_sys::EventTarget,
        event: &'static str,
        callback: impl FnMut(web_sys::Event) + 'static,
    ) -> Result<Self, JsValue> {
        let callback = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(callback));
        target.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref())?;
        Ok(Self {
            target,
            event,
            callback,
        })
    }
}

impl Drop for EventListener {
    fn drop(&mut self) {
        drop(self.target.remove_event_listener_with_callback(
            self.event,
            self.callback.as_ref().unchecked_ref(),
        ));
    }
}

#[derive(Debug, Clone, Copy)]
enum TimerKind {
    Interval,
    Timeout,
}

#[derive(Debug)]
pub(super) struct Timer {
    window: web_sys::Window,
    handle: i32,
    kind: TimerKind,
    _callback: Closure<dyn FnMut()>,
}

impl Timer {
    pub(super) fn interval(ms: i32, callback: impl FnMut() + 'static) -> Result<Self, JsValue> {
        Self::new(TimerKind::Interval, ms, callback)
    }

    pub(super) fn timeout(ms: i32, callback: impl FnMut() + 'static) -> Result<Self, JsValue> {
        Self::new(TimerKind::Timeout, ms, callback)
    }

    fn new(kind: TimerKind, ms: i32, callback: impl FnMut() + 'static) -> Result<Self, JsValue> {
        let window =
            web_sys::window().ok_or_else(|| JsValue::from_str("browser window is unavailable"))?;
        let callback = Closure::<dyn FnMut()>::wrap(Box::new(callback));
        let function = callback.as_ref().unchecked_ref();
        let handle = match kind {
            TimerKind::Interval => {
                window.set_interval_with_callback_and_timeout_and_arguments_0(function, ms)?
            }
            TimerKind::Timeout => {
                window.set_timeout_with_callback_and_timeout_and_arguments_0(function, ms)?
            }
        };
        Ok(Self {
            window,
            handle,
            kind,
            _callback: callback,
        })
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        match self.kind {
            TimerKind::Interval => self.window.clear_interval_with_handle(self.handle),
            TimerKind::Timeout => self.window.clear_timeout_with_handle(self.handle),
        }
    }
}

#[derive(Debug)]
pub(super) struct ResizeSubscription {
    observer: web_sys::ResizeObserver,
    _callback: Closure<dyn FnMut(Vec<web_sys::ResizeObserverEntry>, web_sys::ResizeObserver)>,
}

impl ResizeSubscription {
    pub(super) fn new(
        target: &web_sys::Element,
        mut callback: impl FnMut() + 'static,
    ) -> Result<Self, JsValue> {
        let callback = Closure::<
            dyn FnMut(Vec<web_sys::ResizeObserverEntry>, web_sys::ResizeObserver),
        >::wrap(Box::new(move |_, _| callback()));
        let observer = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref())?;
        observer.observe(target);
        Ok(Self {
            observer,
            _callback: callback,
        })
    }
}

impl Drop for ResizeSubscription {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}
