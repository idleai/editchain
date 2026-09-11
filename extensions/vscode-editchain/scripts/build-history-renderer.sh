#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPOSITORY_DIR="$(cd "$EXTENSION_DIR/../.." && pwd)"
WASM_FILE="$REPOSITORY_DIR/target/wasm32-unknown-unknown/release/editchain_history_renderer.wasm"

# The wasm-bindgen artifact feeds exactly ONE deterministic consumer tree:
# media/rust-history/pkg — PRODUCTION. The Rust-owned history webview loads
# media/rust-history/loader.js (and NOTHING else): the loader imports this
# generated wasm-bindgen module and calls the Rust shell's startHistoryView(),
# which owns the DOM, accessibility, and the renderer. This tree is what the
# shipped panel and the rustSmoke harness/e2e exercise.
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
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${CARGO_HOME_BASE}=/cargo --remap-path-prefix=${RUSTUP_HOME_BASE}=/rustup"

cargo build \
  --manifest-path "$REPOSITORY_DIR/Cargo.toml" \
  --package editchain-history-renderer \
  --target wasm32-unknown-unknown \
  --release \
  --locked

# Deterministic single output: the regeneration check in
# .github/workflows/history-renderer.yml verifies this tree reproduces the
# committed artifacts exactly.
mkdir -p "$EXTENSION_DIR/media/rust-history/pkg"
wasm-bindgen "$WASM_FILE" \
  --target web \
  --out-dir "$EXTENSION_DIR/media/rust-history/pkg" \
  --out-name editchain_history_renderer \
  --no-typescript

echo "History renderer assets written to $EXTENSION_DIR/media/rust-history/pkg (production)"
