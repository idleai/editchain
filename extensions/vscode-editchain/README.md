# EditChain History — VS Code Extension

A read-only unified engineering history explorer: EditChain operations imported
from Claude Code and Codex, overlaid with live Git history from the workspace's
`.git` repositories.

## Prerequisites

- **VS Code** 1.85+
- **Rust toolchain** (to build the native service binary)
- **Rust 1.97 + `wasm32-unknown-unknown` target + `wasm-bindgen-cli` 0.2.127** (Rust-owned history renderer — see below)

## Build & install

```sh
# 1. Build the native Rust service (from the editchain repo root)
cargo build --release -p editchain-vscode-service

# 2. Build the extension
cd extensions/vscode-editchain
npm install
npm run compile        # compiles TS -> out/

# 2b. (Rust-owned history renderer) Build the Rust/WASM assets
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked
npm run build:gpu      # builds crates/editchain-gpu-preview for wasm32 and emits
                       # deterministic dual output: media/rust-history/pkg/ (the
                       # PRODUCTION loader + wasm glue tree) and
                       # media/gpu-preview/pkg/ (the deprecated oracle tree)

# 3a. Package a .vsix and install it (run from INSIDE this folder)
npx @vscode/vsce package
code --install-extension editchain-history-0.1.0.vsix

# 3b. Or run from source: open this folder in VS Code and press F5
```

### Package contents & archive verification

A packaged `.vsix` must carry the full production webview payload plus the
native service it launches:

- **Rust loader**: `media/rust-history/loader.js` — the ONLY bootstrap the
  production webview loads.
- **Generated wasm-bindgen glue**: `media/rust-history/pkg/editchain_gpu_preview.js`
  and `media/rust-history/pkg/editchain_gpu_preview_bg.wasm` (plus the
  oracle's identical `media/gpu-preview/pkg/` copies used by the offscreen
  parity harnesses).
- **Stylesheets**: `media/main.css` (shared history scaffold) and
  `media/gpu-preview/gpu-preview.css` (canvas overlay + status chrome).
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
with in-place **Find-in-Chain** search over the real history view, plus an
explicit read-only raw JSON editor (Enter or double-click on a selected row).
The old filtering controls are intentionally absent while their replacement is
designed. The temporary fixed view shows all operation kinds and undated rows,
hides nested Git repositories/submodules, applies no summary/kind pattern, and
splices hidden intermediates for graph continuity.

Find-in-Chain is in-place: the service runs a Tantivy **BM25 lexical** search
and maps/dedupes every scored chunk to the real top-level row that renders it
under the **exact** active chain filter/profile (the same `hide_submodules` +
`ChainFilterDto` the view was fetched with), so the history DOM, profile,
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
latest-query-wins. Only the profile and viewport row are restored across
recreated panels; switching the Activity/Raw profile exits the find and
refetches from offset 0.

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

### Rust-owned history renderer (default)

`editchain-history.open` opens **one** panel titled **"EditChain History"** and
the webview is owned end-to-end by the Rust/WASM runtime. There is **no second
panel and no side-by-side preview**; `media/main.js` and
`media/gpu-preview/bootstrap.js` are the **deprecated test-only oracle** (see
below) and are not part of the production webview.

**Architecture boundary.** The Rust-owned webview is deliberately narrow:

- **TS VS Code host** (`src/extension.ts`): owns the single panel, the native
  service lifecycle, and the host-message bridge. The generic numeric request
  bridge forwards an **exact read-only allowlist** — `GetWindow`,
  `FindInHistory`, and the legacy `Search` — and rejects every other service
  envelope (including mutating or unknown calls) visibly, so the panel can
  never mutate chain state. `Open` delivery is correlated to the panel
  instance (replayed exactly once after a recreated JS context), and a row
  double-click uses the explicitly handled, read-only JSON viewer.
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
    `ROW_H=34`), Activity/Raw profile switching, find-in-chain sessions,
    expansion, persistence, and render planning.
  - `RowSpec` (`app/rows.rs`) — the pure row presentation model: stable
    `data-key` identity for top-level and sub-op rows, the exact CSS classes
    and ARIA attributes (roving tabindex, `aria-selected`, `aria-expanded`,
    disclosure labels), summary/chrome/work-unit/bundle/promotion inputs,
    wgpu graph data (`lane`, `above`, `below`, `transitions`, `is_subop`,
    `is_bundle`), and the exact `openJson` identity envelope.
  - web-sys DOM shell (`app/dom.rs`) — renders rows as real DOM/text nodes
    (never application `innerHTML` strings), the `role="grid"` table with
    `aria-rowcount`, per-row `role="row"` + gridcell roles, live-region
    announcements (`#status-live`, `#gpu-live`), labelled profile/search
    controls, the single transparent canvas under `#gpu-canvas-host`
    (`pointer-events: none`, over the `.graph-cell` column), the `#gpu-rows`
    frame mirror, and scroll-window mutations + profile-control state.
  - wgpu runtime (`browser.rs` + `shader.wgsl`) — instantiates `GpuRenderer`
    on WebGPU or the WebGL2 fallback selected by wgpu, and submits geometry
    from the serialized frame contract built directly from cached `RowSpec`
    data. `renderCount`/`vertexCount` debug exports let tests assert nonempty
    GPU geometry in addition to the rendered DOM.
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
`dividerResize.test.js` pins the exact centers across divider drags, while the
low-lane `rustSmoke` fixture re-asserts `[14.76, 29.52, …]` after a viewport
change. The real-chain visual matrix records the complete dense-lane vector
before and after narrow/wide divider drags and requires exact equality.

**Deprecated test-only oracle (not production).** `media/main.js` (the former
SVG/DOM controller) and `media/gpu-preview/bootstrap.js` (its GPU overlay
bridge) are retained **only** as the offscreen regression oracle:
`test/harness/index.html` and `test/harness/gpu.html` load them so the
deprecated CPU/SVG renderer can be compared against the GPU overlay (functional
parity, `npm run ui:gpu` scenario oracle, divider invariance). They are never
loaded or executed by the production webview, never act as its renderer, and
must not be described as production.

**Backend selection/fallback.** Inside VS Code the webview starts with
`data-gpu-backend="auto"` and probes GPU capability, preferring WebGPU and
falling back to WebGL. **WebGL is the deterministic CI baseline** (software
rasterization in headless Chromium); **WebGPU is capability-probed and
diagnostic only** — parity and smoke runs pin `--backend webgl` explicitly.

## Configuration

| Setting | Default | Description |
|---|---|---|
| `editchain-history.servicePath` | `""` | Path to the Rust service binary. Empty = prefer the workspace release build, then fall back to debug. |
| `editchain-history.chainDir` | `.editchain` | Path to the EditChain directory relative to the workspace root. |

## Harness testing (text-first layout debugging)

The legacy CPU/SVG renderer (`media/main.js`, the deprecated oracle side) can
be driven headlessly in Chromium so a text-only agent can inspect the rendered
layout without opening VS Code. These harnesses load the **same** oracle page
(`test/harness/index.html` + stylesheet) and report geometry as text first;
settled screenshots are also supported via `--shot` (see below). The Rust-owned
production webview is exercised headlessly by the **Rust-owned adapter smoke
test** (real Chrome) and in real VS Code by the e2e suites further down.

### Fixture mode (deterministic scenarios)

```sh
npm run ui:dump    -- --scenario merge --viewport 1440x900   # full layout dump
npm run ui:inspect -- --scenario merge --selector ".row"     # one element's geometry
npm run ui:check   -- --scenario merge                       # textual checks only
npm run ui:check   -- --scenario merge --shot shot.png       # ...plus a settled screenshot
```

Scenarios: `empty`, `linear`, `merge`, `mixed`, `filtered`, `undated`, `error`,
`warned`, `large`, `longsummary`, `combined` (expansion), `sessionBranch`
(a Codex session anchored below six newer Git commits), `fork` (lanes), and
`highLanes` (200 concurrent lanes — proves no 128-lane clipping). `check` mode
is **test-blocking**: it exits non-zero when any layout check fails or the page
reports errors.

Scenario interactions exercised by the harness block `check` mode:

- `combined` — clicks the combined op and verifies ALL bundled sub-ops reveal
  inline (not just the first one).
- `--search "query"` — runs the renderer's real Find-in-Chain path (input →
  Enter → in-place jump) and verifies the chain DOM is preserved, the counter
  reports `i of N` (e.g. `1 of 4`), focus stays in the search input, and
  double-clicking the highlighted row navigates to the JSON editor with the
  match's real identity (git hits by `git_oid`/`repository`, never the
  synthetic index-only `op_id`), e.g.
  `npm run ui:check -- --scenario mixed --search message`.
- `--search-race` — with `--scenario merge`, issues two rapid searches with the
  first response held, and verifies the LATEST query wins (search-epoch
  correlation): releasing the older query's late response must not replace the
  newer query's matches.
- `--resize` — resizes the viewport and asserts the renderer RECOMPUTES graph
  geometry (header, lane compression, SVG cell widths, rows) instead of just
  stretching the DOM, with lane dots staying inside their cells.
- `warned` — Open responses carrying `warnings`/`diagnostics` (e.g. missing
  blob payloads) render a non-blocking banner above the table while rows still
  load; `OPEN_WARNINGS_VISIBLE` asserts the warning is surfaced, never silently
  discarded.

Find-in-chain keyboard behaviour (ArrowDown/Up + Enter/Shift+Enter wrap with
focus retained, Escape/empty clearing without refetch — the revealed scroll
position is preserved — stale in-flight/edited queries never navigating,
expansion/profile preserved, and the legacy flat-list `Search` path still
rendering) is covered by `node --test test/harness/searchKeyboard.test.js`
against the same harness page.

Artifacts are written to `.ui-out/<scenario>/` (`summary.md`, `layout.txt`,
`layout.json`, `svg.json`, `console.txt`, `metrics.json`, `aria.yml`,
`expansion.json`, `search.json`, `search-race.json`,
`resize.json`).

`--shot PATH` captures a PNG only after the UI is settled (`whenIdle` resolves:
no in-flight requests, no placeholder rows, fonts loaded), so screenshots are
deterministic and never capture the loading state. The probe also asserts
visible nonzero-width Content/Date/Author/Commit cells (`CELL_GEOMETRY`) and
readable text contrast (`CONTRAST_READABLE`) at every viewport width, so the
visual acceptance criteria are checked numerically in `check` mode.

### Real-data mode (live Rust service)

```sh
npm run ui:real -- --workspace /path/to/repo --chain-dir .editchain
```

Spawns the actual `editchain-vscode-service`, opens the workspace, and renders
the real chain. Writes a full DOM tree with computed styles to `.ui-out/real/`
(`dom.json`, `dom.txt`, plus the same artifacts as fixture mode). Add
`--shot out.png` to capture a settled screenshot after `whenIdle` (same
deterministic readiness gate as fixture mode).

Use `--scroll-row N` to jump to a specific visible history row after the
full-height virtual-scroll spacer is ready. The run fails unless the settled
renderer window actually contains that row, and records the requested row,
pixel offset, and resulting `renderTop`/`renderBottom` in `summary.md`.
`--row-timeout MS` applies to the renderer's real `GetWindow` transport calls
as well as readiness checks, which is important for large imported chains.

### Graph harness (real service, structural checks)

The graph harness pages the **entire** rendered dataset through the real
service bridge using the exact `GetWindow` DTOs the renderer sends, then runs
structural checks over the data **and** the rendered DOM — built for ~150k-row
chains without accumulating full row DTOs:

```sh
npm run ui:graph -- --workspace /path/to/repo --chain-dir .editchain
```

Options: `--out DIR` (default `.ui-out/graph`), `--limit N` (probe page size,
default 2000; the renderer uses 500), `--viewport WxH`, `--row-timeout MS`.
`--row-timeout` is threaded through **every** page-side `GetWindow` request
(probe pages and the renderer's own scroll fetches, via the service transport
default): a hung service fails the run after the configured bound instead of
the old hardcoded 120s. `Open` remains explicitly unbounded (0 deadline) —
building the chain + git graph can take minutes on a large workspace. The
service binary comes from `SERVICE_PATH` or
`<workspace>/target/release/editchain-vscode-service`, with the debug build as
a fallback.

The probe's own failure detection is verified without a service via simulated
fixtures:

```sh
npm run ui:graph -- --self-test
```

`--self-test` runs an in-page simulated service and asserts that a
`chain_generation` change mid-scan is detected (the run fails with
`CHAIN_GENERATION_STABLE`) and that a hung `GetWindow` rejects after the
configured timeout instead of hanging. Artifacts land in `.ui-out/graph-self-test/`.

Checks (all exit non-zero on failure):

- Paging: page totals stable, paged rows match the reported total, and
  `chain_generation` is **identical across every paged response** — a change
  mid-scan (the chain was rewritten while being read) fails the run.
- Node keys: non-empty and unique across top-level and sub-op rows.
- Sub-ops: `<parent>::sub:<i>` key format, `parent_row`/expanded-slot layout,
  and the offset-0 `sub_op_counts` snapshot agree.
- Parents: every parent key resolves to a top-level node; no self/repeated
  parents.
- Relations: `parent_relations` reference drawn parents, carry a known kind,
  and are not duplicated — plus a per-kind count report (never hardcoded).
- Diagnostics: Open duplicate/quarantine counts are consistent
  (`records = accepted + duplicates + quarantined`) and their rates are
  reported.
- Lanes: all lane references within `[0, max_lane]`, vertical segments meet at
  every row boundary (page boundaries included), no lane reuse without a
  parent edge, tip/root boundaries clean, sub-op lanes inherit their parent,
  transitions anchored by dots or boundary geometry.
- Rendered rows: `data-row`/`data-key` integrity, uniform `ROW_H`, bounded DOM,
  one in-cell dot per top-level row, lane-consistent monotonic dot positions,
  and render/data coherence (rendered keys match the dataset at the same
  absolute slot; renderer `total`/`max_lane` match the dataset). Coherence is
  sampled at the initial viewport **and** after scrolling to the top, middle,
  and bottom of the chain (plus a top re-anchor after the full traversal),
  waiting for idle after each jump — a scroll path that leaks DOM rows or
  fails to re-anchor fails the run (`SCROLL_*` checks).

Artifacts (`--out`, default `.ui-out/graph/`):

- `graph.json` — aggregates: totals, relation-kind counts, duplicate rates,
  kind/lane histograms, paging summary.
- `dataset.ndjson` — one compact row per line (probe fields only; streamed in
  chunks so the full dataset is never materialized as a single blob).
- `checks.json` / `render.json` / `open.json` — checks, rendered DOM slice +
  renderer state + scroll samples, and the Open response (diagnostics).
- `summary.md` / `console.txt` / `service-stderr.txt`.

The harness reuses the real `editchain-vscode-service`, `serviceBridge.js`,
the legacy oracle `media/main.js`, and `media/main.css` — no simulated
renderer. The probe pages through the same bridge the renderer uses, so a
structural failure
reflects what the viewer would actually draw. Run against a small chain first
(e.g. a scratch workspace); the harness is memory-conscious but a full scan of
a very large chain still pages every row.

### Rust-owned adapter smoke test (real Chrome)

Drives the REAL Rust/WASM history adapter end-to-end in headless Chrome
(deterministic SwiftShader WebGL baseline) through the Rust-only fixture page
`test/harness/rust.html`:

```sh
CHROME_PATH=/path/to/chrome npm run test:rust-smoke
```

`rust.html` loads **neither** `media/main.js` **nor**
`media/gpu-preview/bootstrap.js`: `media/rust-history/loader.js` initializes
the generated wasm-bindgen module and calls the Rust shell's
`startHistoryView()`, which acquires `window.acquireVsCodeApi()` (the fixture
bridge supplies it), installs the host-message listener, renders real
`.row[data-row][data-key]` DOM with grid ARIA into `#rows`, creates the single
canvas under `#gpu-canvas-host`, and mirrors frame markers into `#gpu-rows`.

The suite asserts: wasm starts cleanly; the synchronous Open/Ready handshake
and correlated numeric-id window replies drive `dataReady`; `role="grid"` +
`aria-rowcount` + per-row gridcell roles; exactly one canvas in
`#gpu-canvas-host` and no foreign canvases; the `#gpu-rows` mirror; the
deterministic `webgl` backend with `renderCount`/`vertexCount` > 0; the nominal
low-lane Pulse pitch `[14.76, 29.52, …]` unchanged across a viewport change;
and the functional path through the Rust shell — Activity→Raw→Activity profile
switching with `hide_trace` flipping, find-in-chain submit/next/clear with
pending/busy ARIA, row selection + roving keyboard + raw-JSON identity,
chevron disclosure with `aria-expanded` and sub-op reveal, and the legacy
flat-list `Search` — plus a screenshot under `trace/rust-smoke.png`. It also
proves `media/main.js` and the gpu-preview bootstrap are absent at runtime.

Like the parity suites, the runtime tests **skip** (never fail) when Chrome or
the built `media/rust-history/pkg` assets are missing, so the generic
`npm run test:harness` suite stays green without GPU build artifacts. CI
installs Chrome first and runs this suite explicitly with `CHROME_PATH` so it
**cannot silently skip**.

### GPU renderer harness (fixture parity, offscreen regression oracle)

This harness is an **offscreen regression oracle**: it compares the deprecated
CPU/SVG harness page against the GPU harness page over a local static HTTP
server and is **not** the shipped VS Code UI — the shipped UI is the single
"EditChain History" panel (default `editchain-history.open`) rendered by the
Rust-owned webview, exercised by the real VS Code e2e below.

Drives **both** renderers against the **same deterministic fixture
scenario**: the CPU/SVG harness (`test/harness/index.html`) and the GPU
harness (`test/harness/gpu.html`, which loads the **same legacy scaffold** —
`media/main.css` + `media/main.js` + fixtures + fixture bridge — plus the
deprecated `media/gpu-preview` bootstrap + wasm over a local static HTTP
server). Each side
settles through its own debug `whenIdle` (no arbitrary sleeps), then the runner
compares normalized rows — absolute index, key/`node_key`, lane, sorted
above/below lane sets, directed transitions, and total — **and the shared
functional state**: profile, search mode, view-message state, table header,
group warnings, an overlapping render window with the same anchor (buffer
extent may differ with viewport height), and the last `GetWindow` `hide_trace`
flag. Scenarios declare an expected end state: `empty` and `error` expect the
full-pane empty/error message on both sides (and pass only if the message state
matches), while every other scenario expects rendered rows + nonempty wgpu
geometry (`renderCount`/`vertexCount` > 0, one canvas in `#gpu-canvas-host`
over the `.graph-cell` column, `#gpu-rows` mirror == snapshot rows).
The runner fails when common functional state is missing, not merely on a
geometry-prefix mismatch.

```sh
npm run ui:gpu -- --scenario merge --backend webgl --shot
```

Options: `--scenario` (default `merge`; the full fixture set from the CPU
harness, including `multigroup` for virtual paging across group boundaries),
`--backend webgl|webgpu` (default `webgl` — the deterministic CI baseline;
`webgpu` is diagnostic), `--viewport WxH`, `--out DIR`, `--shot` (settled
full-page screenshots). Artifacts land in `.ui-out/gpu-<scenario>/`:
`parity.json`, `metrics.json`, `console.txt`, `summary.md`, plus `dom.png` /
`gpu.png` with `--shot`. The run exits non-zero on page errors, missing
wasm/bootstrap artifacts, unexpected empty/error states, or any geometry or
functional-state mismatch — contract tests for the comparison helpers run in
the generic harness suite (`test/harness/gpuContract.test.js`).

Deterministic **browser functional parity** drives the SAME page behavior
through both sides — Activity→Raw→Activity request filters and
metadata gating, virtual scroll/paging on `large`/`workUnitsDeep`, find-in-chain
submit + next/prev + off-cache jump + clear, legacy flat-list `Search`,
row selection/keyboard roving/raw-JSON identity, work-unit/bundle expansion with
sub-op visibility, and the expected `empty`/`error` states — and asserts the GPU
DOM meets the exact shared semantics plus `renderCount`/`vertexCount` > 0:

```sh
CHROME_PATH=/path/to/chrome node --test test/harness/functionalParity.test.js
```

The suite reuses `fixtures.js` + `fixtureBridge.js` + `media/main.js` debug
hooks (`test/harness/functionalDriver.js`) and **skips** (never fails) when
Chrome or the built wasm assets are absent, so the generic harness suite stays
green without GPU assets. CI runs the focused contract + functional tests and
pins WebGL parity on `merge`, `fork`, `highLanes`, `multigroup`, `empty`, and
`error` in `.github/workflows/gpu-preview.yml`; WebGPU is never required.
Explicit WebGPU runs use the `gpu-<scenario>-webgpu` artifact directory so a
capability diagnostic cannot overwrite the deterministic WebGL baseline.

**Graph-lane divider invariance (fixed low-lane Pulse pitch).** The lane-pitch
regression oracle (`test/harness/dividerResize.test.js`) drives both oracle
pages and asserts that resizing the graph-column divider changes ONLY the
column width: `window.__editchainGraphAdapter.laneXAll()` and the rendered
dot `cx` stay at the fixed `[14.76, 29.52, 44.28, 59.04]` centers in every
divider state (this pins the bug fix: the Pulse pitch used to switch to the
unscaled 18px spacing after the first drag, and dragging the column narrower
than `numLanes × 14.76` used to re-distribute every lane centre
proportionally). It skips gracefully without Chrome/assets and runs explicitly
with real Chrome in CI.

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
  (`test/harness/layoutProbe.js`) into the webview and runs the identical textual
  checks inside real VS Code. It also exercises inline selection/raw JSON,
  Activity/Raw profile switching, scroll-through-history, and Find-in-Chain
  against the native service. The find test verifies the preserved real chain,
  counter, highlighted row, input focus, the visible Previous/Next chevron
  buttons (labels, enabled state, and real mouse clicks navigating forward,
  back, and wrapping around), then captures `e2e-find-in-chain-*.png`
  screenshots under `trace/`.
- Downloads VS Code + Chromedriver on first run into `.wdio-vscode-service/`
  (gitignored).

The Rust/WASM GPU renderer has its own real-VS-Code e2e:

```sh
npm run ui:vscode:gpu
```

- Config: `test/vscode/wdio.gpu.conf.ts`; spec: `test/vscode/gpu-preview.e2e.ts`.
- Runs in a real Extension Development Host against the native release service:
  opens the **default** `editchain-history.open` command and asserts exactly
  **one** panel titled "EditChain History" (no second/companion panel), the
  panel's rendered `#rows .row[data-key]` rows, the
  `window.__editchainGpuDebug` contract (backend `webgl`/`webgpu`, snapshot
  rows/total, `dataReady`, no `lastError`, `renderCount`/`vertexCount` > 0),
  the single transparent wgpu canvas in `#gpu-canvas-host` over the
  `.graph-cell` column with the `#gpu-rows` mirror matching the snapshot, and
  a deterministic `whenIdle` settle. It then drives a compact production path
  inside the GPU-backed panel — Activity→Raw→Activity profile switching,
  find-in-chain submit + next + clear, scrolling/paging, and inline selection
  + keyboard roving (raw JSON stays closed: the harness covers the exact
  `openJson` envelope) — writes `trace/e2e-history-gpu-contract.json`, and
  captures the single-panel webview frame
  (`trace/e2e-history-webview.png`). There is deliberately no side-by-side
  capture; CPU-vs-GPU row parity stays in the offscreen regression oracle.
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

- `test/harness/index.html` mounts `media/main.css` + `media/main.js` with a
  `vscode` shim in place of `acquireVsCodeApi()` — the deprecated CPU/SVG
  oracle page; `test/harness/gpu.html` adds the deprecated
  `media/gpu-preview/bootstrap.js` + wasm on top of the same scaffold.
- `test/harness/rust.html` loads **only** `media/rust-history/loader.js` (plus
  fixtures + fixture bridge) — the Rust-owned production path, driven by
  `test/harness/rustSmoke.test.js`.
- `?bridge=fixture` (default) uses deterministic protocol fixtures;
  `?bridge=service` forwards requests to the real Rust service over framed stdio.
- `test/harness/layoutProbe.js` exposes `window.__editchainDebug` with
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
- The Rust-owned history renderer is the default history view: the webview is
  bootstrapped by `media/rust-history/loader.js` + the generated wasm-bindgen
  glue and owned by the Rust `HistoryAppState`/`RowSpec`/web-sys/wgpu runtime.
  WebGL is the deterministic CI baseline (software rasterization in headless
  Chromium); WebGPU is capability-probed and diagnostic only. The renderer
  never mutates Git, the worktree, or canonical EditChain storage. The former
  `media/main.js` SVG controller + `media/gpu-preview/bootstrap.js` are
  retained only as the deprecated test-only oracle (the offscreen CPU
  reference side of the parity harnesses), never as production.
- `Open` is unbounded: building the chain + git graph can take minutes on a
  large workspace. All other service calls carry a generous finite deadline
  (120s by default, ≥ the measured near-minute first window on large chains;
  the graph harness's `--row-timeout` overrides this default); a timed-out
  window/search shows a visible error and suspends background retries until the
  user clicks **Retry** or re-runs the open command (which restarts a crashed
  service and re-opens the chain).
