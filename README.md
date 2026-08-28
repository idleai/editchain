# editchain

CRDT-based edit chain built from agent session history, browsed through the **EditChain History** VS Code extension. WIP / experiment.

## VS Code extension

The primary UI lives in [`extensions/vscode-editchain/`](./extensions/vscode-editchain/): a read-only unified history explorer that overlays EditChain operations with live Git history.

Build the native service and the extension:

```sh
cargo build -p editchain-vscode-service
cd extensions/vscode-editchain
npm install
npm run compile
```

Then open the folder in VS Code and press F5, or package a `.vsix` — full instructions in [`extensions/vscode-editchain/README.md`](./extensions/vscode-editchain/README.md). Open the viewer via the command palette → **"EditChain: Open History Explorer"**.

## CLI

The native CLI initializes chains and imports/searches history:

```sh
cargo build
cargo run --bin editchain -- init my-chain
cargo run --bin editchain -- import \
  --sessions-dir /path/to/cc-sessions --workspace /path/to/repo --chain ./outputs/cc-chain
cargo build --manifest-path tools/codex-session-exporter/Cargo.toml
cargo run --bin editchain -- import --provider codex \
  --sessions-dir ~/.codex/sessions --workspace /path/to/repo --chain ./outputs/codex-chain \
  --codex-helper ./tools/codex-session-exporter/target/debug/codex-session-exporter
cargo run --bin editchain -- search ./outputs/cc-chain "query" --mode hybrid --top 10
cargo run --bin editchain -- retrieve ./outputs/cc-chain --op "<op-id>"
```

Codex import currently uses an isolated local bridge against a sibling `codex` checkout; see [the exporter contract](./tools/codex-session-exporter/README.md). The bridge boundary is versioned so it can later move behind a native Codex command without coupling the chain or viewer schemas to Codex internals.

Non-dry imports persist content-addressed blobs and per-source cursors under the chain directory (`blobs/`, `cursors/`); `--dry-run` keeps both stores in memory. A cursor (and any persisted rewrite generation) is committed only after its operation page is appended and synced — including a directory sync so a brand-new segment file's entry is durable first — and a cursor is also committed when an empty or truncated-to-empty source staged one with zero ops. A newline-unterminated EOF record stays pending until the source completes it on a later import.

**Durability is at-least-once, not atomic.** If the process crashes after the append but before the cursor commit, the next import re-reads the same sources and appends an exact replay of the same ops (same deterministic ids); opening the chain canonicalizes exact replays through the core `OpSet`. No atomicity is claimed across the log and the cursor files — a crash between them can leave the log ahead of the cursors, never behind. Within a cursor-store commit, a persisted rewrite generation is written before any cursor file that depends on it, so a crash or write error between the two can leave a generation with no cursor, but never a cursor ahead of its generation.

**Rewrites.** Codex sources whose files were truncated or rewritten since the last import are re-imported whole under a new deterministic boot generation (a per-source counter persisted with the cursors), so rewritten sources never collide with their previous generation's op ids and never abort unrelated files. Persisted rewrite generations are Codex-only: the Claude importer detects truncation the same way but re-imports at its fixed boot-1 id space and persists no generation counter (behavior unchanged in this task). With the current cursor design only size decreases are detectable: an exact same-size rewrite is indistinguishable from an unchanged file and is skipped (documented residual; deleting the source's cursor file re-imports it).

**Safe reset.** Deleting a source's cursor file re-imports that source and appends replay records to the physical log (deduplicated by `OpSet` on open). For Codex sources the generation counter is retained, so a rewritten source stays in its current id space. Deleting the whole `cursors/` directory also resets the generation counters — a re-imported rewritten source then reuses the original boot-0 id space and can shadow earlier lines with identical ids. Opening a chain canonicalizes decoded records through the core `OpSet` (exact replays ignored, same-id conflicts quarantined), then the viewer resolves persisted blobs before projection and search, while missing or corrupt legacy blobs remain visible as explicit hydration diagnostics in the `Open` response.

## Models used

See [MODELS.md](./MODELS.md) for the model timeline and the source-task/hybrid workflow provenance.
