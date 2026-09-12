# EditChain History for VS Code

This extension opens one read-only history panel that stays up to date
automatically, combining Codex activity, imported Claude history, and Git.
The UI is a Rust/WASM renderer;
the TypeScript host starts the native service and handles VS Code integrations
such as JSON documents and native diff editors.

## Build

Prerequisites:

- the repository Rust toolchain and `wasm32-unknown-unknown` target;
- `wasm-bindgen-cli` 0.2.127;
- Node.js 20 or newer.

From the repository root:

```sh
cargo build --release -p editchain-node --bins --locked

cd extensions/vscode-editchain
npm ci
npm run build:renderer
npm run compile
```

`build:renderer` builds `crates/editchain-history-renderer` for wasm32 and writes
the single generated bundle under `media/rust-history/pkg/`.

Follow the [packaging instructions](#packaging) to install the extension, open
the project you want to explore, and invoke **EditChain: Open History Explorer**.
The settings are:

- `editchain-history.servicePath`: absolute path to the native service binary.
  Release and then debug builds under the open workspace are used when empty;
  set this explicitly when viewing a project outside the EditChain checkout.
- `editchain-history.chainDir`: EditChain data directory relative to the open
  workspace, defaulting to `.editchain`.

## Live Codex history

Open **EditChain: Open History Explorer** in a trusted workspace. Live updates
are enabled by default: the extension follows Git and imports growing Codex
rollouts while the panel is open. Codex capture requires the built
[codex-session-exporter](../../tools/codex-session-exporter/README.md).
**EditChain: Pause Live History** pauses observation for the current VS Code
session; **EditChain: Resume Live History** resumes it. Closing the panel stops
the collector, and opening it again restarts collection unless paused.
Set `editchain-history.live.enabled` to `false` to open static history by default.

- `live.sessionsPath` selects the sessions tree (default: `$CODEX_HOME/sessions`,
  or `~/.codex/sessions`). Use the same root as earlier imports so provider-relative
  cursor identities remain consistent.
- `live.codexHelperPath` selects `codex-session-exporter`, defaulting to the
  workspace helper release build, then PATH. It must support `--stream`.
  `live.cliPath` is retained for compatibility with the earlier prototype;
  the resident native service now owns live imports.
- Collection polls every 250 ms after the previous pass. It notices new,
  grown, replaced, or truncated rollouts. Recently modified sessions are
  processed first, in batches of at most 32 sources. Warm sources retain their
  byte cursor, hash and provider reducer. Large append bursts drain through
  bounded batches even without another file change.
- Import and publication are serialized. Complete source records and durable
  checkpoints remain the history authority; edits arriving during a pass trigger
  a later pass. Restarting reuses native checkpoints instead of duplicating nodes.
- Revisioned block changes update persistent native indexes and bounded WASM caches,
  and DOM rows. Surviving selection, disclosure, focus and pixel scroll anchors
  remain attached to item identities. Staying at the top follows incoming
  history. New branch and merge connections grow from their attachment points,
  with continuous drawing across row boundaries. Existing SVG segments keep
  their animation clocks through later deltas. Lane spacing stays at 14.76px;
  dense graphs scroll horizontally. Reduced motion shows the final geometry
  immediately. Manual Refresh establishes a new baseline.
- Live Codex items fold along connected causal paths within their native task.
  Grouping adds no graph rows: task controls annotate existing activities;
  a collapsed summary replaces that physical path in the same anchor row.
  Singletons remain ordinary activities. Completed historical paths start
  folded; live arrivals keep their own content visible even inside a collapsed
  task. Completion does not automatically fold open work. The activity-count
  button toggles the task path; each item's file/output chevron remains separate.
  Forks, merges, Git attachments and unresolved/error activity remain visible.
  Concurrent tasks retain chronological order without repeated header rows.
  Ordinary updates edit individual items and affected anchors, without resending
  a whole task. Search reveals the exact hidden item. See the
  [grouping contract](../../docs/realtime-grouping-research.md).

**Output → EditChain History** logs startup, per-delta source bytes, decoded
records, changed blocks and native timings. Automatic startup keeps the history
panel in focus; the Resume command also reveals the output channel. See
[the implementation and verification notes](../../docs/realtime-deltas.md) for
integrity assumptions, current boundaries and the actual-session Xvfb harness.

This first milestone observes local Codex sessions and the first workspace
folder's Git HEAD/refs, including shared refs in linked worktrees. It also
notices external writes to chain segments. Git commits and imported file-edit
evidence appear automatically; arbitrary uncommitted filesystem writes are not
captured as edit evidence. Claude live collection and hook activity signals are
follow-up work.

Latency includes Codex's rollout flush, the next collection pass, incremental
projection, and rendering. Initial bootstrap can take substantially longer
than the polling interval. The status bar shows startup, queued imports, and
update progress; its tooltip also shows collection failures and retries.
Rollout `.jsonl.zst` archives are outside the importer's supported live input.

### Prepare a large history

Run from the repository root before opening a large workspace:

```sh
./target/release/editchain prepare-view --workspace /absolute/workspace \
  --chain /absolute/workspace/.editchain
```

This creates `CHAIN/live-v1`, including admission state, graph, task disclosure,
row pages and search. Later runs advance its saved frontier additively.
After upgrading task grouping or graph checkpoint semantics, run this command once
to migrate the existing checkpoint; it reuses saved rows and canonical indexes. Schema
3 restores exact subagent spawn/completion connections and causal ordering for tied
timestamps. The
extension opens the saved viewport first and starts collection after it renders.
Prepared native opening plus 500 rows took 0.24–0.40 seconds on the local
2.42-million-operation samples; these timings exclude VS Code startup.
Initial preparation remains expensive and uses disk space. A cold Codex helper
still rebuilds its source reducer. See [measurements and recovery](../../docs/native-memory.md).

## Runtime architecture

```text
VS Code extension.ts
  ├─ starts editchain-vscode-service over framed stdio
  ├─ opens one "EditChain History" webview
  ├─ forwards GetWindow, LocateRows, and FindInHistory from the webview
  ├─ opens OpenLivePaged and serializes capture, disclosure and delta publication
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

The native service supports these request bodies:

- `Open`
- `OpenLive`
- `OpenLivePaged`
- `ToggleLive { snapshot_id, key, task }` (`task: true` for the task path,
  `false` for the physical item's own details)
- `SyncLive { epoch, after_revision, codex }`
- `Refresh`
- `GetWindow { snapshot_id, offset, limit, include_layout }`
- `LocateRows { snapshot_id, keys }` (at most 2000 presentation anchors)
- `FindInHistory { snapshot_id, query, top_k }`
- `GetNodeDetails`
- `ResolveObject`
- `GetFileDiff`

The generic webview bridge permits only `GetWindow`, `LocateRows`, and `FindInHistory` and
requires an exact one-key request envelope. Details and file diffs are explicit
host actions, so arbitrary service requests cannot be tunneled through the
webview.

Paged live `GetWindow` returns visible coordinates and native geometry together.
Static history uses a two-pass first paint: rows are requested without graph
layout, then the same page is hydrated with lane geometry. Normal history uses
the fixed Activity projection; live mode uses current item blocks and retained
graph intervals. Both hide nested-repository rows.
`FindInHistory` uses Tantivy BM25 (persisted for live checkpoints) and resolves candidates
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
npm run ui:vscode:live
```

The harness verifies the exact minimal request envelopes, virtual paging,
two-pass layout, row semantics, accessibility, selection, disclosure,
find-in-history, JSON/diff identities, and the absence of a canvas renderer.

The renderer VS Code suite expects Git history and the imported Claude session
in `test/fixtures/claude`, as prepared by CI. Use a disposable checkout for that
fixture import, set `EDITCHAIN_RENDERER_E2E_WORKSPACE` to its path, and set
`EDITCHAIN_RENDERER_E2E_SERVICE` to the freshly built native service. The suite
loads this extension's current generated renderer assets. Its initial viewport
must contain both Git commits and an agent work group; an arbitrary working
chain may not contain the required rows there.

The live suite creates and removes a temporary Git repository and Codex rollout.
It opens the normal History command with default settings, then uses the real
exporter and native release binaries to verify automatic collection, multiple
edits within one unfinished turn, Git commit observation, and pause/resume replay.

For repository-wide Rust formatting, clippy, tests, docs, and dependency
policy, run `./scripts/lint.sh` from the repository root.

## Packaging

```sh
npm run build:renderer
npm run compile
npm run package
```

Run **Extensions: Install from VSIX…** in VS Code and select the generated
`.vsix` file. Configure `editchain-history.servicePath` to point to the native
service build; the service binary is built separately from the extension.

The generated `media/rust-history/pkg` artifacts are committed and CI verifies
that regeneration is deterministic.
