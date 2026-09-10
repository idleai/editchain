# CLAUDE.md

This file provides repository guidance to Claude Code.

## Quality

Run the canonical suite before declaring a code task complete:

```sh
./scripts/lint.sh
```

The repository quality policy in `AGENTS.md` applies. Do not weaken checks,
thresholds, or tests to make the suite pass.

## Common commands

```sh
# Build and test the Rust workspace
cargo build --workspace
cargo test --workspace

# Build the native service used by VS Code
cargo build -p editchain-vscode-service

# Import Claude Code sessions
cargo run --bin editchain -- import \
  --sessions-dir /path/to/sessions \
  --workspace /path/to/repo \
  --chain /path/to/repo/.editchain

# Import Codex rollouts through the local exporter bridge
cargo run --bin editchain -- import --provider codex \
  --sessions-dir ~/.codex/sessions \
  --workspace /path/to/repo \
  --chain /path/to/repo/.editchain \
  --codex-helper ./tools/codex-session-exporter/target/debug/codex-session-exporter

# Pregenerate the fixed Activity-view snapshot
cargo run --bin editchain -- prepare-view \
  --workspace /path/to/repo --chain .editchain

# Build and test the extension
cd extensions/vscode-editchain
npm ci
npm run build:renderer
npm run compile
npm run test:harness
```

## Architecture

EditChain is a read-only VS Code history explorer backed by a native Rust
service. The Cargo workspace has ten crates:

```text
Claude/Codex JSONL
       │
       ▼
editchain-import ──► editchain-node (import + prepare-view CLI)
       │                         │
       ▼                         ▼
editchain-core ──► editchain-codec ──► .editchain segment/blob storage
                                              │
Git repository ──► editchain-git              │
       │                                      │
       └──────────────┬───────────────────────┘
                      ▼
             editchain-project
                      │
             editchain-index (BM25)
                      │
             editchain-protocol
                      │
             editchain-vscode-service
                      │ framed stdio
                      ▼
             VS Code extension host
                      │
                      ▼
        editchain-history-renderer (Rust/WASM SVG renderer)
```

- `editchain-core`: operation schema, identifiers, causal ordering, `OpSet`,
  chain state, and reducers.
- `editchain-codec`: postcard operation frames and bounded EC02 page scanning.
  EC02 uses fixed u32 record lengths and has no checksum; EC03 is a separate,
  currently inactive checksummed format.
- `editchain-store`: canonical read-only chain access, record locations,
  integrity diagnostics, and exclusive durable segment writers.
- `editchain-import`: deterministic, incremental Claude Code and Codex
  importers, cursors, and content-addressed blob storage.
- `editchain-node`: ingestion CLI and persistence coordination. Its commands are
  `import` and `prepare-view`.
- `editchain-git`: repository catalog with explicit worktree/Git/common paths,
  scoped discovery diagnostics, history walking, commit/blob
  resolution, and file-change extraction used by the extension.
- `editchain-project`: the unified EditChain/Git Activity projection, graph
  layout, work-unit grouping, and presentation taxonomy.
- `editchain-index`: in-memory Tantivy BM25 index used by Find in History.
- `editchain-protocol`: six stdio request DTOs: `Open`, `GetWindow`,
  `FindInHistory`, `GetNodeDetails`, `ResolveObject`, and `GetFileDiff`.
- `editchain-vscode-service`: native stateful stdio server, lazy BM25 search,
  fixed Activity windows, details/diff resolution, and render snapshots.
- `editchain-history-renderer`: the Rust/WASM history renderer.
  It owns state, virtual scrolling, accessibility, and per-row SVG graph
  fragments. It contains no wgpu, WebGPU, WebGL, shader, or canvas backend.

The TypeScript extension is intentionally thin. It starts the service, hosts
one panel, forwards only `GetWindow` and `FindInHistory` from the webview, and
handles node details and native diffs as host-side actions.

## Storage and compatibility

The segment log and blob store are authoritative. Render snapshots and the
Tantivy index are derived and may be rebuilt. Keep the EC02 operation/page
format and snapshot schema compatible unless a task explicitly authorizes a
format migration.

## Debugging

See `AGENTS.md` for the shared DebugMCP/vGDB contract and the required VS Code
launch configuration.
