#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPOSITORY_DIR="$(cd "$EXTENSION_DIR/../.." && pwd)"
WASM_WORKSPACE="$(python3 "$SCRIPT_DIR/stage-rust-workspace.py" "$REPOSITORY_DIR")"
WASM_FILE="$WASM_WORKSPACE/target/wasm32-unknown-unknown/release/editchain_history_renderer.wasm"

# The web bindings feed media/rust-history/pkg. The Rust-owned history webview loads
# media/rust-history/loader.js (and NOTHING else): the loader imports this
# generated wasm-bindgen module and calls the Rust shell's startHistoryView(),
# which owns the DOM, accessibility, and the renderer. This tree is what the
# shipped panel and the rustSmoke harness/e2e exercise. The independent Node
# adapter exposes app-core's portable connection state to the legacy peer host.
if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "wasm-bindgen-cli 0.2.127 is required (cargo install wasm-bindgen-cli --version 0.2.127 --locked)" >&2
  exit 1
fi

# rustc bakes absolute build paths (CARGO_HOME registry sources and RUSTUP_HOME
# std sources) into panic-location strings, so the wasm bytes would otherwise
# differ between machines (e.g. CI vs local) and the committed-artifact
# regeneration check would fail. Remap both roots to fixed prefixes so the
# generated pkg tree is byte-identical everywhere.
CARGO_HOME_BASE="${CARGO_HOME:-$HOME/.cargo}"
RUSTUP_HOME_BASE="${RUSTUP_HOME:-$HOME/.rustup}"
GRAPH_SIBLING_ROOT="$(cd "$REPOSITORY_DIR/.." && pwd)"
ORIGINAL_RUSTFLAGS="${RUSTFLAGS:-}"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${CARGO_HOME_BASE}=/cargo --remap-path-prefix=${RUSTUP_HOME_BASE}=/rustup --remap-path-prefix=${GRAPH_SIBLING_ROOT}=/workspace --remap-path-prefix=${WASM_WORKSPACE}=/workspace"

cargo build \
  --manifest-path "$WASM_WORKSPACE/Cargo.toml" \
  --target-dir "$WASM_WORKSPACE/target" \
  --package editchain-history-renderer \
  --target wasm32-unknown-unknown \
  --release \
  --locked

# Deterministic web and Node outputs: the regeneration check in
# .github/workflows/history-renderer.yml verifies both trees reproduce the
# committed artifacts exactly.
mkdir -p "$EXTENSION_DIR/media/rust-history/pkg"
wasm-bindgen "$WASM_FILE" \
  --target web \
  --out-dir "$EXTENSION_DIR/media/rust-history/pkg" \
  --out-name editchain_history_renderer \
  --no-typescript

RUSTFLAGS="$ORIGINAL_RUSTFLAGS" bash "$SCRIPT_DIR/build-client-state.sh"

echo "History and client-state assets written to media/rust-history/pkg and media/client-state/pkg"
