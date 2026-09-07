# EditChain History — VS Code Extension

A read-only unified engineering history explorer: EditChain operations imported
from Claude Code and Codex, overlaid with live Git history from the workspace's
`.git` repositories.

## Prerequisites

- **VS Code** 1.85+
- **Rust toolchain** (to build the native service binary)
- **Rust 1.97 + `wasm32-unknown-unknown` target + `wasm-bindgen-cli` 0.2.127** (to build the Rust/WASM history renderer assets — the production webview renderer)

## Build & install

```sh
# 1. Build the native Rust service (from the editchain repo root)
cargo build --release -p editchain-vscode-service

# 2. Build the extension
cd extensions/vscode-editchain
npm install
npm run compile        # compiles TS -> out/

# 2b. Build the Rust/WASM history renderer assets
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked
npm run build:gpu      # builds crates/editchain-gpu-preview for wasm32 and emits
                       # the deterministic production tree:
                       # media/rust-history/pkg/ (the generated wasm-bindgen glue)

# 3a. Package a .vsix and install it (run from INSIDE this folder)
npx @vscode/vsce package
code --install-extension editchain-history-0.1.0.vsix

# 3b. Or run from source: open this folder in VS Code and press F5
```

### Package contents & archive verification

A packaged `.vsix` must carry the full production webview payload plus the
native service it launches:

- **Rust loader**: `media/rust-history/loader.js` — the ONLY script the
  production webview loads.
- **Generated wasm-bindgen glue**: `media/rust-history/pkg/editchain_gpu_preview.js`
  and `media/rust-history/pkg/editchain_gpu_preview_bg.wasm`.
- **Stylesheets**: `media/main.css` (shared history scaffold) and
  `media/gpu-preview/gpu-preview.css` (status chrome + the inert
  canvas-overlay scaffold + `#gpu-rows` mirror; the Rust shell never creates a
  canvas).
- **TS host output**: `out/extension.js`.
- **Service binary**: the native `editchain-vscode-service` release build
  (from the repo root: `cargo build --release -p editchain-vscode-service`).
  The `.vsix` does not embed it — set `editchain-history.servicePath` to the
  binary, or leave it empty to prefer
  `<workspace>/target/release/editchain-vscode-service` (debug fallback).

Verify the archive before installing:

```sh
npx @vscode/vsce ls   # lists every file that would enter the .vsix
```

The listing must include `out/extension.js`, `media/rust-history/loader.js`,
`media/rust-history/pkg/editchain_gpu_preview.js`,
`media/rust-history/pkg/editchain_gpu_preview_bg.wasm`,
`media/main.css`, and `media/gpu-preview/gpu-preview.css`.

### Configure the service path

Set `editchain-history.servicePath` to the Rust binary, or leave it empty to
prefer `<workspace>/target/release/editchain-vscode-service` with the debug
build as a fallback.

### Open the viewer

Command palette (`Ctrl+Shift+P`) → **"EditChain: Open History Explorer"**.

The viewer shows a unified, paged history list (EditChain ops + git commits)
with in-place **Find-in-Chain** search over the real history view. Expand a Git
commit or a supported Claude/Codex edit activity to see its changed files in a
column-aligned VS Code Source Control-style list: Activity remains `change`,
Tags shows the colored `A`/`M`/`D`/`R`/`C` status (plus `recorded` when
applicable), and Content shows the file name with its dimmed directory.
Clicking a file row (or pressing Enter) opens VS Code's native diff editor. Git
diffs are exact immutable commit
content against the commit's first parent. Agent diffs use the strongest
evidence retained by the importer; a visible `recorded` label means the diff is
a snippet, hunk, or reconstruction that may not be a complete sequential file
snapshot. Older Codex chains that collapsed a multi-file event onto its first
path are repaired at read time from the byte-exact raw `FileChange` evidence.
Binary changes produce an explanatory warning instead of a text diff.

Enter or double-click on an ordinary selected history row opens its explicit
read-only raw JSON editor; file rows always use the native diff action.
The old filtering controls are intentionally absent while their replacement is
designed. The temporary fixed view shows all dated operation kinds, omits rows
whose timestamp is unknown (`timestamp_ms == 0`), hides nested Git
repositories/submodules, applies no summary/kind pattern, and splices hidden
intermediates for graph continuity.

Find-in-Chain is in-place: the service runs a Tantivy **BM25 lexical** search
and maps/dedupes every scored chunk to the real top-level row that renders it
under the **exact** fixed Activity filter (the same `hide_submodules` +
`ChainFilterDto` the view was fetched with), so the history DOM, presentation,
expansion, cache, and layout are never replaced — the find only scrolls and
highlights. A settled search auto-jumps to and highlights match 1, shows an
adjacent `i of N` counter (`N+` when the candidate cap truncated retrieval),
and keeps focus in the input: `ArrowDown`/`ArrowUp` and `Enter`/`Shift+Enter`
cycle next/previous with wrap, scrolling to reveal each current match. `Escape`
or emptying the input clears the session without refetching, leaving the scroll
where the last match was revealed. Folded/bundled hits map to their containing
top-level row without auto-expanding anything. Git hits carry their real
identity (`git_oid` lowercase hex, exact decimal `repository`, `kind: "git"`,
`is_submodule`), so opening a git match resolves by `ResolveObject` — never by
the synthetic index-only `op_id`. Rapid consecutive searches are
latest-query-wins. Only the viewport row is restored across recreated panels;
the extension always uses Activity presentation (`hide_trace=true`).

The result counter and **Previous/Next** chevrons are embedded inside the same
bordered search field, matching VS Code's built-in find controls. The chevrons
appear only after a non-empty result set has settled, and disappear for
pending, empty, error, cleared, or edited-query states. Each click steps through
the exact same wrap-around matches (`ArrowUp`/`ArrowDown` and
`Shift+Enter`/`Enter` are the keyboard equivalents) without ever stealing focus
from the search input or replacing the history chain.

All viewer-facing identifiers round-trip as exact JSON strings: op ids are
`node:boot:seq`, git OIDs are lowercase hex, and repository/session/actor ids
are exact decimal `u64` strings — never JSON numbers, so values above 2^53 are
not rounded by JavaScript. The service parses and validates these strings and
returns an `Error` envelope for invalid ids. Find-in-Chain responses use a
`FindInHistoryMatch` DTO — one per distinct visible top-level row (`node_key`,
absolute expanded-history `row` offset, best `score`, plus the `SearchHit`
identity fields `op_id`/`chunk_id`/`session_id`/`actor_id` and git
`git_oid`/`repository` as strings) with timestamps/counts kept numeric. The
flat `SearchHit` DTO remains only for the legacy `Search` request, which
production no longer issues. The read-only JSON editor's `ResolveObject` Ok
payload is a typed `ResolvedObject` DTO under the same rule: `repository` and
`changed_paths` are exact decimal strings, `oid`/`tree`/`parents` are lowercase
hex strings, and `imported_record` is a `node:boot:seq` string when present —
never raw u64 numbers or byte-array ID structures.

### Rust/WASM history renderer (sole renderer)

`editchain-history.open` opens **one** panel titled **"EditChain History"** and
the webview is owned end-to-end by the Rust/WASM runtime. There is **no second
panel and no side-by-side preview** — the single Rust/WASM panel is the only
shipped UI.

**Architecture boundary.** The Rust-owned webview is deliberately narrow:

- **TS VS Code host** (`src/extension.ts`): owns the single panel, the native
  service lifecycle, and the host-message bridge. The generic numeric request
  bridge forwards an **exact read-only allowlist** — `GetWindow`,
  `FindInHistory`, and the legacy `Search` — and rejects every other service
  envelope (including `GetFileDiff`, mutating, or unknown calls) visibly, so
  the panel can never mutate chain state. `Open` delivery is correlated to the
  panel instance (replayed exactly once after a recreated JS context). Ordinary
  row JSON and file-row diffs are separate, explicitly handled read-only host
  actions; diff content is revalidated and materialized by the native service
  before the host invokes `vscode.diff` with virtual read-only documents.
- **Rust history loader** (`media/rust-history/loader.js`): the ONLY bootstrap
  the production webview loads — a tiny static ES module (no eval, no inline
  code, no dynamic import strings). It initializes the generated wasm-bindgen
  module with the explicit wasm URL and calls the Rust shell's
  `startHistoryView()`. It owns no app state, events, DOM, frame assembly, or
  host-request logic; it only mirrors the Rust shell's debug exports as a
  read-only `window.__editchainGpuDebug` facade (`loader: 'rust-history'`) for
  harness/e2e runners.
- **Generated wasm-bindgen glue**: `media/rust-history/pkg/editchain_gpu_preview.js`
  + `_bg.wasm` (wasm-bindgen 0.2.127, `--target web`, `--no-typescript`).
- **Rust runtime** (`crates/editchain-gpu-preview`, compiled to
  `wasm32-unknown-unknown`):
  - `HistoryAppState` (`app/state.rs`) — the pure view state machine ported
    from the production controller: view/search generations, request
    correlation (including synchronous fixture-response reentrancy), the
    sparse window cache, virtual paging (`PAGE=500`, `BUFFER=400`,
    `ROW_H=34`), the fixed Activity filter, find-in-chain sessions, expansion,
    persistence, and render planning.
  - `RowSpec` (`app/rows.rs`) — the pure row presentation model: stable
    `data-key` identity for top-level and sub-op rows, the exact CSS classes
    and ARIA attributes (roving tabindex, `aria-selected`, `aria-expanded`,
    disclosure labels), summary/chrome/work-unit/bundle/promotion inputs,
    graph data (`lane`, `above`, `below`, `transitions`, `is_subop`,
    `is_bundle`), plus exact `openJson` and service-advertised `openDiff`
    identity envelopes.
  - web-sys DOM shell (`app/dom.rs`) — renders rows as real DOM/text nodes
    (never application `innerHTML` strings), the `role="grid"` table with
    `aria-rowcount`, per-row `role="row"` + gridcell roles, live-region
    announcements (`#status-live`, `#gpu-live`), labelled search
    controls, one aria-hidden `svg.graph-row-fragment` per hydrated row inside
    its `.graph-cell`, the inert `#gpu-canvas-host` container (no canvas is
    ever created), the `#gpu-rows` frame mirror, and scroll-window mutations.
  - Per-row SVG graph rendering (`app/dom.rs` + `row_graph_items`) — paints
    each row's graph from the pure `RowSpec` items (local-cell lane halves,
    cross-lane transition halves, the centered node dot, bundle glyphs) as
    real SVG nodes, so the graph scrolls inside the row DOM. The obsolete
    `GpuRenderer`/wgpu surface remains compiled only for its native geometry
    tests; the shell never instantiates it (`renderCount` > 0, `vertexCount`
    0, backend `svg`).
- **Accessibility**: the Rust shell owns the a11y surface — grid/row/gridcell
  roles, `aria-rowcount`, `aria-selected`/`aria-expanded`, roving tabindex
  (exactly one tabbable row), labelled controls, `aria-live` status regions,
  and `aria-busy` pending-search state.

**Graph geometry invariants (fixed after initial layout; no divider
autoscaling).** The nominal Pulse lane pitch is
`LANE_W * LANE_W_PULSE_SCALE` = 18 × 0.82 = **14.76 CSS px**. Ordinary lane
counts therefore start at `[14.76, 29.52, …]`. Dense topologies are compressed
once against the initial natural graph budget (down to `MIN_LANE_W` = 1.5 px,
with a matching dot-radius reduction; extreme counts are distributed across
that initial budget) so every lane remains addressable. After that initial
layout, lane X positions are immutable with respect to column width: dragging
the divider changes only the rendered column width, so rightmost lanes may
clip at the edge or trailing space may appear, but the topology never shifts.
The low-lane `rustSmoke` fixture re-asserts `[14.76, 29.52, …]` after a
viewport change. The real-chain visual matrix records the complete dense-lane
vector before and after narrow/wide divider drags and requires exact equality.

**Backend.** The Rust shell reports the deterministic per-row SVG backend
(`backend: 'svg'`); the `data-gpu-backend` attribute on the webview body is
accepted for scaffold compatibility but no wgpu/WebGL surface is created — the
graph lives inside the scrolling row DOM, so there is nothing to probe or
fall back between.

## Configuration

| Setting | Default | Description |
|---|---|---|
| `editchain-history.servicePath` | `""` | Path to the Rust service binary. Empty = prefer the workspace release build, then fall back to debug. |
| `editchain-history.chainDir` | `.editchain` | Path to the EditChain directory relative to the workspace root. |

## Harness testing

The Rust/WASM production webview is exercised headlessly in real Chrome by the
**Rust-owned adapter smoke test** below, and inside real VS Code by the e2e and
visual suites further down.

### Rust-owned adapter smoke test (real Chrome)

Drives the REAL Rust/WASM history adapter end-to-end in headless Chrome
through the Rust-only fixture page `test/harness/rust.html`:

```sh
CHROME_PATH=/path/to/chrome npm run test:rust-smoke
```

`rust.html` loads **neither** `media/main.js` **nor**
`media/gpu-preview/bootstrap.js` (both retired oracle files):
`media/rust-history/loader.js` initializes the generated wasm-bindgen module
and calls the Rust shell's `startHistoryView()`, which acquires
`window.acquireVsCodeApi()` (the fixture bridge supplies it), installs the
host-message listener, renders real `.row[data-row][data-key]` DOM with grid
ARIA into `#rows`, paints every hydrated row's own aria-hidden
`svg.graph-row-fragment` inside its `.graph-cell`, and mirrors frame rows into
`#gpu-rows` — no canvas surface is created anywhere.

The suite asserts: wasm starts cleanly; the synchronous Open/Ready handshake
and correlated numeric-id window replies drive `dataReady`; `role="grid"` +
`aria-rowcount` + per-row gridcell roles; the deterministic `svg` backend with
`renderCount` > 0 and `vertexCount` 0; zero canvases (the inert
`#gpu-canvas-host` stays empty and no foreign canvases exist); one
aria-hidden `svg.graph-row-fragment` per hydrated row; the `#gpu-rows` mirror;
the nominal low-lane Pulse pitch `[14.76, 29.52, …]` unchanged across a viewport
change; and the functional path through the Rust shell — fixed Activity
presentation with no profile mutation surface, find-in-chain submit/next/clear
with pending/busy ARIA, row selection + roving keyboard + raw-JSON identity,
chevron disclosure with `aria-expanded` and sub-op reveal, and the legacy
flat-list `Search` — plus a screenshot under `trace/rust-smoke.png`. It also
proves `media/main.js` and the gpu-preview bootstrap stay absent at runtime.

The runtime tests **skip** (never fail) when Chrome or the built
`media/rust-history/pkg` assets are missing, so the generic
`npm run test:harness` suite stays green without GPU build artifacts. CI
installs Chrome first and runs this suite explicitly with `CHROME_PATH` so it
**cannot silently skip**.

### Real VS Code harness (WebdriverIO)

Launches **real VS Code** (Extension Development Host) with the extension and
drives the webview end-to-end — validating activation, the native service spawn,
the message bridge, and the webview/panel lifecycle that the standalone harness
cannot.

```sh
npm run ui:vscode   # requires xvfb on headless servers (wrapped automatically)
```

- Config: `test/vscode/wdio.conf.ts` (points at the extension + service binary).
- Spec: `test/vscode/history.e2e.ts` — opens the webview via
  `workbench.getWebviewByTitle('EditChain History')`, switches into its iframe,
  asserts rows render, then **injects the same `window.__editchainDebug` probe**
  (`test/vscode/layoutProbe.js`) into the webview and runs the identical textual
  checks inside real VS Code. It also exercises inline selection/raw JSON,
  the fixed Activity presentation, scroll-through-history, and Find-in-Chain
  against the native service. The find test verifies the preserved real chain,
  counter, highlighted row, input focus, the visible Previous/Next chevron
  buttons (labels, enabled state, and real mouse clicks navigating forward,
  back, and wrapping around), then captures `e2e-find-in-chain-*.png`
  screenshots under `trace/`.
- Downloads VS Code + Chromedriver on first run into `.wdio-vscode-service/`
  (gitignored).

The Rust/WASM history renderer has its own real-VS-Code e2e:

```sh
npm run ui:vscode:gpu
```

- Config: `test/vscode/wdio.gpu.conf.ts`; spec: `test/vscode/gpu-preview.e2e.ts`.
- Runs in a real Extension Development Host against the native release service:
  opens the **default** `editchain-history.open` command and asserts exactly
  **one** panel titled "EditChain History" (no second/companion panel), the
  panel's rendered `#rows .row[data-key]` rows, the
  `window.__editchainGpuDebug` contract (backend `svg`, snapshot rows/total,
  `dataReady`, no `lastError`, `renderCount` > 0, `vertexCount` 0), zero
  canvases (the inert `#gpu-canvas-host` stays empty), one aria-hidden
  `svg.graph-row-fragment` per hydrated row, the `#gpu-rows` mirror matching
  the snapshot, and a deterministic `whenIdle` settle. It then drives a compact
  production path inside the Rust-backed panel — fixed Activity presentation,
  find-in-chain submit + next + clear, scrolling/paging, and inline
  selection + keyboard roving (raw JSON stays closed: the harness covers the
  exact `openJson` envelope) — writes `trace/e2e-history-gpu-contract.json`,
  and captures the single-panel webview frame
  (`trace/e2e-history-webview.png`). There is deliberately no second panel and
  no side-by-side capture — the single Rust/WASM panel is the only shipped UI.
- The GPU WDIO config resolves the repository and native service relative to
  itself. Override them with `EDITCHAIN_GPU_E2E_WORKSPACE` and
  `EDITCHAIN_GPU_E2E_SERVICE` when testing another checkout or binary.

The deterministic **visual state matrix** captures the default panel's rendered
states as clearly named screenshots plus a JSON/Markdown manifest:

```sh
npm run ui:vscode:visual
```

- Config: `test/vscode/wdio.visual.conf.ts`; spec:
  `test/vscode/visual-matrix.e2e.ts` (real Extension Development Host + native
  release service, same single "EditChain History" panel).
- States: `initial-activity`, `raw-profile`, `find-current`/`find-next`,
  `row-selected`, `keyboard-focus`, `bundle-expanded` (when available),
  `deep-scroll` (smooth animated scroll down and back up),
  `scroll-top-restored`, and `graph-narrow`/`graph-wide` with the
  lane-geometry invariant (`laneXAll` unchanged while the graph column
  resizes).
- Artifacts: `trace/visual-matrix/` — per-state webview (and full-workbench)
  PNGs plus `manifest.json` / `manifest.md` recording state names, observed
  metadata, and the renderer instance id. The suite only asserts wire-to-DOM
  contracts that already exist; the empty/error states are skipped by design
  (they would require mutating the real chain/service).

To **record the session as video** (useful for reviewing the rendered UI without
a display), run the recording wrapper — it starts Xvfb, captures the display
with ffmpeg, and runs the suite:

```sh
./scripts/ui-vscode-record.sh [out.mp4] [wdio-config]
./scripts/ui-vscode-record.sh .ui-out/vscode-visual-matrix.mp4 \
  ./test/vscode/wdio.visual.conf.ts     # visual matrix, animated scrolls
```

`out.mp4` defaults to `.ui-out/vscode-session.mp4` and the wdio config defaults
to `./test/vscode/wdio.conf.ts` (the scroll-through-history suite). Requires
`xvfb` and `ffmpeg`. The MP4 is **always** finished: ffmpeg is stopped with a
graceful SIGTERM and flushed even when the wdio suite fails, and the wrapper
exits with the wdio suite's own exit code.

### How it works

- `test/harness/rust.html` loads **only** `media/rust-history/loader.js` (plus
  fixtures + fixture bridge) — the Rust-owned production path, driven by
  `test/harness/rustSmoke.test.js`.
- `?bridge=fixture` (default) uses deterministic protocol fixtures;
  `?bridge=service` forwards requests to the real Rust service over framed stdio.
- `test/vscode/layoutProbe.js` exposes `window.__editchainDebug` with
  `whenIdle()`, `dumpLayout()`, `assertLayout()`, and `getMetrics()`.
- The probe runs textual checks (header present, no overflow, dot-row alignment,
  column alignment) that report expected/actual/delta — so layout regressions are
  debuggable as text.

## Notes

- The viewer is **read-only**: it never mutates Git, the worktree, or canonical
  EditChain storage.
- Find-in-Chain is **BM25 lexical** (a Tantivy index built lazily on first
  search; no embedding server required). Semantic vector/hybrid search exists
  in `editchain-query` and the `editchain-node` CLI but is not wired to the VS
  Code service; the legacy flat-list `Search` request remains for compatibility
  only.
- The Rust/WASM history renderer is the sole renderer: the webview is
  bootstrapped by `media/rust-history/loader.js` + the generated wasm-bindgen
  glue and owned by the Rust `HistoryAppState`/`RowSpec`/web-sys runtime,
  which paints each row's graph as an inline `svg.graph-row-fragment` (no wgpu
  canvas surface is created). The renderer never mutates Git, the worktree, or
  canonical EditChain storage.
- `Open` is unbounded: building the chain + git graph can take minutes on a
  large workspace. All other service calls carry a generous finite deadline
  (120s by default, ≥ the measured near-minute first window on large chains); a
  timed-out window/search shows a visible error and suspends background
  retries until the user clicks **Retry** or re-runs the open command (which
  restarts a crashed service and re-opens the chain).
