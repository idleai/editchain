//! Rust `HistoryApp`: the pure, target-independent application core for the
//! `EditChain` history webview.
//!
//! This module ports the production controller (`media/main.js`) view-state
//! logic into Rust so the webview can eventually be owned end-to-end by
//! WASM. Stage 1 (this pass) delivers the pure, natively-tested core only:
//!
//! - [`host`]   — protocol envelope parsing and host request building.
//! - [`state`]  — the full view state machine: view/search generations,
//!   request correlation (incl. synchronous fixture-response reentrancy), the
//!   sparse window cache, virtual paging decisions (`PAGE=500`, `BUFFER=400`,
//!   `ROW_H=34`), profile switching, find/search, expansion (sub-op reveal),
//!   persistence, and render planning.
//! - [`rows`]   — the pure row presentation model (`RowSpec`) ported from the
//!   production row renderer: identity/`data-key`, classes/ARIA/disclosure,
//!   summary/chrome/work-unit/bundle/promotion inputs, wgpu graph data, and
//!   the `openJson` envelope. No DOM is built here (see the module docs).
//! - [`dom`]    — the Rust-owned browser slice (3A): pure window/lane
//!   presentation helpers (native-tested) plus the web-sys DOM shell that
//!   renders `RowSpec`s into real `#rows` nodes with per-row SVG graph cells,
//!   owns scroll/paging and the Activity/Raw profile controls, and mirrors
//!   the render window for the debug facade. The obsolete fixed-viewport
//!   canvas overlay is no longer created.
//!
//! Everything here is pure `std` code and covered by native unit tests and
//! clippy with `-D warnings`; the wasm-only DOM shell compiles on
//! `wasm32-unknown-unknown` and is exercised by the browser smoke test
//! (`extensions/vscode-editchain/test/harness/rustSmoke.test.js`).

pub(crate) mod host;

pub(crate) mod rows;

pub(crate) mod state;

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) mod dom;
