#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPOSITORY_DIR="$(cd "$EXTENSION_DIR/../.." && pwd)"
CARGO_ROOT="${CARGO_HOME:-$HOME/.cargo}"
RUSTUP_ROOT="${RUSTUP_HOME:-$HOME/.rustup}"
SIBLING_ROOT="$(cd "$REPOSITORY_DIR/.." && pwd)"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${CARGO_ROOT}=/cargo --remap-path-prefix=${RUSTUP_ROOT}=/rustup --remap-path-prefix=${SIBLING_ROOT}=/workspace"

cargo build --manifest-path "$REPOSITORY_DIR/Cargo.toml" \
  --package editchain-client-state --target wasm32-unknown-unknown --release --locked
mkdir -p "$EXTENSION_DIR/media/client-state/pkg"
wasm-bindgen "$REPOSITORY_DIR/target/wasm32-unknown-unknown/release/editchain_client_state.wasm" \
  --target nodejs --out-dir "$EXTENSION_DIR/media/client-state/pkg" \
  --out-name editchain_client_state --no-typescript
