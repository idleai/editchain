//! Rust/WebAssembly history renderer for the VS Code history view.
//!
//! The webview is owned end-to-end by Rust/WASM: it manages the host protocol,
//! virtualized rows, interaction state, DOM, and per-row SVG graph fragments.

#[cfg(any(target_arch = "wasm32", test))]
mod app;

#[cfg(target_arch = "wasm32")]
mod shell;

/// Re-export the wasm shell entry points so the `#[wasm_bindgen]` exports stay
/// reachable (and importable by the generated JS bindings).
#[cfg(target_arch = "wasm32")]
pub use shell::{
    debug_backend, debug_data_ready, debug_find_state, debug_generation, debug_graph_state,
    debug_in_flight_count, debug_lane_x_all, debug_metrics, debug_render_count,
    debug_renderer_instance_id, debug_row_at, debug_snapshot, debug_total, debug_view_gen,
    start_history_view,
};
