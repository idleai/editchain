//! Rust `HistoryApp`: the pure, target-independent application core for the
//! `EditChain` history webview.
//!
//! This module ports the legacy JS controller's view-state logic into Rust;
//! the webview is now owned end-to-end by WASM. The core is pure and
//! natively tested:
//!
//! - [`host`]   — protocol envelope parsing and host request building.
//! - [`state`]  — the full view state machine: view/search generations,
//!   request correlation (incl. synchronous fixture-response reentrancy), the
//!   sparse window cache, virtual paging decisions (`PAGE=500`, `BUFFER=400`,
//!   `ROW_H=34`), find/search, expansion (sub-op reveal), persistence, and
//!   render planning for the fixed Activity view.
//! - [`rows`]   — the pure row presentation model (`RowSpec`) ported from the
//!   legacy row renderer: identity/`data-key`, classes/ARIA/disclosure,
//!   summary/chrome/work-unit/bundle/promotion inputs, graph data, and
//!   the `openJson` envelope. No DOM is built here (see the module docs).
//! - [`dom`]    — the Rust-owned browser slice (3A): pure window/lane
//!   presentation helpers (native-tested) plus the web-sys DOM shell that
//!   renders `RowSpec`s into real `#rows` nodes with per-row SVG graph cells,
//!   owns scroll/paging and search controls, and mirrors the render window for
//!   the debug facade. The obsolete fixed-viewport
//!   canvas overlay is no longer created.
//!
//! Everything here is pure `std` code and covered by native unit tests and
//! clippy with `-D warnings`; the wasm-only DOM shell compiles on
//! `wasm32-unknown-unknown` and is exercised by the browser smoke test
//! (`extensions/vscode-editchain/test/harness/rustSmoke.test.js`).

use serde::Deserialize;

/// Renderer-side form of the protocol's reusable chain presentation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChainState {
    /// De-emphasized node and child-owned graph edge.
    Muted,
    /// Ordinary graph and row presentation; also the compatibility fallback.
    #[default]
    #[serde(other)]
    Active,
}

impl ChainState {
    /// Parse the compact wire value without allocating.
    #[must_use]
    pub(crate) fn from_wire(value: &str) -> Self {
        if value == "muted" {
            Self::Muted
        } else {
            Self::Active
        }
    }

    /// Whether muted graph styling applies.
    #[must_use]
    pub(crate) const fn is_muted(self) -> bool {
        matches!(self, Self::Muted)
    }
}

mod cache;
mod requests;

pub(crate) mod host;

pub(crate) mod rows;

pub(crate) mod state;

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) mod dom;
