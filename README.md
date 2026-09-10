# editchain

CRDT-based edit chain built from agent session history, browsed through the **EditChain History** VS Code extension. WIP / experiment.

## VS Code extension

The primary UI lives in [`extensions/vscode-editchain/`](./extensions/vscode-editchain/): a read-only unified history explorer that overlays EditChain operations with live Git history. The history view is **Rust/WASM — the sole renderer**: `editchain-history.open` opens one panel titled **"EditChain History"** bootstrapped by the tiny `media/rust-history/loader.js` + generated wasm-bindgen glue, with the `editchain-history-renderer` crate owning the view state (`HistoryAppState`), row model (`RowSpec`), and the web-sys DOM/accessibility surface (each row's graph is an inline `svg.graph-row-fragment`; no canvas overlay). The webview loads no other scripts, and the renderer is exercised headlessly by `test/harness/rust.html` (rustSmoke) and in real VS Code by the e2e/visual suites — full details in the extension README.

Build the native service and the extension:

```sh
cargo build -p editchain-node --bin editchain-vscode-service
cd extensions/vscode-editchain
npm install
npm run compile
```

Then open the folder in VS Code and press F5, or package a `.vsix` — full instructions in [`extensions/vscode-editchain/README.md`](./extensions/vscode-editchain/README.md). Open the viewer via the command palette → **"EditChain: Open History Explorer"**.

## Ingestion CLI

The native CLI keeps only the two workflows that feed the extension: importing
session history and preparing its immutable render snapshot.

```sh
cargo build
cargo run --bin editchain -- import \
  --sessions-dir /path/to/cc-sessions --workspace /path/to/repo --chain ./outputs/cc-chain
cargo build --manifest-path tools/codex-session-exporter/Cargo.toml
cargo run --bin editchain -- import --provider codex \
  --sessions-dir ~/.codex/sessions --workspace /path/to/repo --chain ./outputs/codex-chain \
  --codex-helper ./tools/codex-session-exporter/target/debug/codex-session-exporter
cargo run --bin editchain -- prepare-view \
  --workspace /path/to/repo --chain .editchain
```

Codex import currently uses an isolated local bridge against a sibling `codex` checkout; see [the exporter contract](./tools/codex-session-exporter/README.md). The bridge boundary is versioned so it can later move behind a native Codex command without coupling the chain or viewer schemas to Codex internals.

Non-dry imports persist content-addressed blobs and per-source cursors under the chain directory (`blobs/`, `cursors/`); `--dry-run` keeps both stores in memory. Claude and Codex are explicit, additive provider passes: importing one never recursively imports records embedded by the other. Cursor identity is the provider plus the exact path relative to its sessions root, so copying an unchanged source tree between an archive root and a live root does not replay it. Legacy absolute-path cursors migrate once while retaining their original operation-ID node. A cursor (and any persisted rewrite generation) is committed only after its operation page is appended and synced — including a directory sync so a brand-new segment file's entry is durable first — and a cursor is also committed when an empty or truncated-to-empty source staged one with zero ops. A newline-unterminated EOF record stays pending until the source completes it on a later import.

**Durability is at-least-once, not atomic.** If the process crashes after the append but before the cursor commit, the next import re-reads the same sources and appends an exact replay of the same ops (same deterministic ids); opening the chain canonicalizes exact replays through the core `OpSet`. No atomicity is claimed across the log and the cursor files — a crash between them can leave the log ahead of the cursors, never behind. Within a cursor-store commit, a persisted rewrite generation is written before any cursor file that depends on it, so a crash or write error between the two can leave a generation with no cursor, but never a cursor ahead of its generation.

**Rewrites.** For both providers, every cursor stores direct BLAKE3 over exactly the accepted byte prefix. Before an append, the importer re-hashes that prefix; truncation, same-size replacement, and a changed prefix followed by growth therefore start a new deterministic boot generation. The per-source generation counter is persisted with the cursor, so rewritten sources never collide with earlier operation IDs or abort unrelated files. File size alone never establishes continuity.

**Safe reset.** Deleting a source's cursor file re-imports that source and appends replay records to the physical log (deduplicated by `OpSet` on open). Its provider-independent generation counter is retained, so a rewritten source stays in its current ID space. Deleting the whole `cursors/` directory also resets the generation counters: a rewritten source can then reuse earlier IDs with different bytes. All versions of a conflicted ID are retained as evidence and excluded from accepted history. The viewer and import reconciliation share this rule through `editchain-store`. Missing or corrupt blobs, undecodable records, and incomplete segment tails are reported in the `Open` diagnostics.

## Models used

See [MODELS.md](./MODELS.md) for the model timeline and the source-task/hybrid workflow provenance.
