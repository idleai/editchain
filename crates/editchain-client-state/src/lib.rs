//! Temporary Node/WASM adapter for the legacy peer consumer until f18.
//! Shared join, retry and status policy remains in app-core's idle-history crate.

use idle_history::connection::{
    peer_status, Connection, ConnectionStatus, JoinState, PeerProgress,
};
use wasm_bindgen::prelude::*;

/// Join lifetime shared with app-core; the host retains credentials and transports.
#[wasm_bindgen]
#[derive(Debug, Default)]
pub struct SharedJoin(JoinState);

#[wasm_bindgen]
impl SharedJoin {
    /// Create a stopped sharing session.
    #[must_use]
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current token for asynchronous host work.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn generation(&self) -> u32 {
        self.0.generation()
    }

    /// Whether sharing has been enabled by a current approval.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn enabled(&self) -> bool {
        self.0.enabled()
    }

    /// Accept a completed join only in its original context.
    pub fn enable(&mut self, generation: u32) -> bool {
        self.0.enable(generation)
    }

    /// Check that pending host work still owns the join lifetime.
    #[must_use]
    pub fn is_current(&self, generation: u32) -> bool {
        self.0.is_current(generation)
    }

    /// Stop/suspend and retire all previous host callbacks.
    pub fn retire(&mut self) {
        self.0.retire();
    }
}

/// Shared reconnect policy, usable by the Node extension and browser hosts.
#[wasm_bindgen]
#[derive(Debug, Default)]
pub struct SharedConnection(Connection);

#[wasm_bindgen]
impl SharedConnection {
    /// Create a stopped connection.
    #[must_use]
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start an attempt and return its callback token.
    ///
    /// # Errors
    /// Fails when callback identities cannot be allocated without reuse.
    pub fn begin(&mut self) -> Result<u32, JsValue> {
        self.0
            .begin()
            .ok_or_else(|| JsValue::from_str("Connection identities exhausted"))
    }

    /// Current callback token.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn generation(&self) -> u32 {
        self.0.generation()
    }

    /// Shared presentable status.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn status(&self) -> String {
        self.0.status().label().to_owned()
    }

    /// Bounded retry delay; the host supplies its timer and optional jitter.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn retry_delay_ms(&self) -> u32 {
        self.0.retry_delay_ms()
    }

    /// Retire callbacks from a stopped transport.
    pub fn stop(&mut self) {
        self.0.stop();
    }

    /// Record a retryable failure for its owning attempt.
    pub fn waiting(&mut self, generation: u32) {
        let _accepted = self.0.update(generation, ConnectionStatus::Waiting);
    }

    /// A new invitation is required before retrying.
    pub fn expired(&mut self, generation: u32) {
        let _accepted = self.0.update(generation, ConnectionStatus::Expired);
    }

    /// Hosting is ready, or a peer inventory has been reconciled.
    pub fn ready(&mut self, generation: u32) {
        let _accepted = self.0.update(generation, ConnectionStatus::Live);
    }

    /// A transport is open but its peer has not been authenticated.
    pub fn authenticating(&mut self, generation: u32) {
        let _accepted = self.0.update(generation, ConnectionStatus::Authenticating);
    }

    /// Project validated native inventory progress into shared status.
    ///
    /// # Errors
    /// Rejects malformed progress without changing the current connection state.
    pub fn progress(&mut self, generation: u32, json: &str) -> Result<(), JsValue> {
        let progress: PeerProgress =
            serde_json::from_str(json).map_err(|error| JsValue::from_str(&error.to_string()))?;
        let _accepted = self.0.update(generation, peer_status(&progress));
        Ok(())
    }
}
