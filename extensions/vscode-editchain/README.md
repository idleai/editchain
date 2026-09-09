# EditChain History for VS Code

This extension opens one read-only history panel that combines imported
Claude/Codex activity with live Git history. The UI is a Rust/WASM renderer;
the TypeScript host starts the native service and handles VS Code integrations
such as JSON documents and native diff editors.

## Build

Prerequisites:

- the repository Rust toolchain and `wasm32-unknown-unknown` target;
- `wasm-bindgen-cli` 0.2.127;
- Node.js 20 or newer.

From the repository root:

```sh
cargo build -p editchain-vscode-service

cd extensions/vscode-editchain
npm ci
npm run build:renderer
npm run compile
```

`build:renderer` builds `crates/editchain-history-renderer` for wasm32 and writes
the single generated bundle under `media/rust-history/pkg/`.

Run the extension with F5 from the repository and invoke **EditChain: Open
History Explorer**. The settings are:

- `editchain-history.servicePath`: native service binary; release and then
  debug builds under the workspace are used when empty.
- `editchain-history.chainDir`: EditChain data directory relative to the open
  workspace, defaulting to `.editchain`.

## Runtime architecture

```text
VS Code extension.ts
  ├─ starts editchain-vscode-service over framed stdio
  ├─ opens one "EditChain History" webview
  ├─ forwards GetWindow and FindInHistory from the webview
  └─ handles openJson/openDiff through service-validated identities

media/rust-history/loader.js
  └─ initializes the generated wasm-bindgen module

editchain-history-renderer (Rust/WASM)
  ├─ history state, request correlation, and virtual paging
  ├─ semantic row DOM and accessibility
  ├─ find-in-history navigation and expansion
  └─ per-row inline SVG graph fragments
```

There is no wgpu/WebGPU/WebGL renderer, shader, canvas overlay, hidden frame
mirror, alternate view, or side-by-side preview. `media/main.css` is the
only production stylesheet and `media/rust-history/loader.js` is the only
handwritten renderer script loaded by the panel.

## Service protocol

The native service supports six request bodies:

- `Open`
- `GetWindow { offset, limit, include_layout }`
- `FindInHistory { query, top_k }`
- `GetNodeDetails`
- `ResolveObject`
- `GetFileDiff`

The generic webview bridge permits only `GetWindow` and `FindInHistory` and
requires an exact one-key request envelope. Details and file diffs are explicit
host actions, so arbitrary service requests cannot be tunneled through the
webview.

`GetWindow` uses a two-pass first paint: rows are requested without global
layout, then the same page is hydrated with stable lane geometry. The service
always serves the fixed Activity projection and hides nested-repository rows.
`FindInHistory` runs the in-memory Tantivy BM25 index and resolves candidates
back to visible top-level row coordinates.

The `.editchain` segment log and blob store are authoritative. The render
snapshot under `.editchain/render/` and BM25 index are derived and rebuildable.

## Tests

```sh
# TypeScript host and deterministic fixture contracts
npm run compile
npm run test:harness

# Focused real-Chrome Rust/WASM harness
npm run test:rust-smoke

# Real VS Code suites (Xvfb on Linux)
npm run ui:vscode
npm run ui:vscode:renderer
npm run ui:vscode:visual
```

The harness verifies the exact minimal request envelopes, virtual paging,
two-pass layout, row semantics, accessibility, selection, disclosure,
find-in-history, JSON/diff identities, and the absence of a canvas renderer.

For repository-wide Rust formatting, clippy, tests, docs, and dependency
policy, run `./scripts/lint.sh` from the repository root.

## Packaging

```sh
npm run build:renderer
npm run compile
npm run package
```

The generated `media/rust-history/pkg` artifacts are committed and CI verifies
that regeneration is deterministic.
