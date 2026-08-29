# EditChain History — VS Code Extension

A read-only unified engineering history explorer: EditChain operations imported
from Claude Code and Codex, overlaid with live Git history from the workspace's
`.git` repositories.

## Prerequisites

- **VS Code** 1.85+
- **Rust toolchain** (to build the native service binary)

## Build & install

```sh
# 1. Build the native Rust service (from the editchain repo root)
cargo build --release -p editchain-vscode-service

# 2. Build the extension
cd extensions/vscode-editchain
npm install
npm run compile        # compiles TS -> out/

# 3a. Package a .vsix and install it (run from INSIDE this folder)
npx @vscode/vsce package
code --install-extension editchain-history-0.1.0.vsix

# 3b. Or run from source: open this folder in VS Code and press F5
```

### Configure the service path

Set `editchain-history.servicePath` to the Rust binary, or leave it empty to
prefer `<workspace>/target/release/editchain-vscode-service` with the debug
build as a fallback.

### Open the viewer

Command palette (`Ctrl+Shift+P`) → **"EditChain: Open History Explorer"**.

The viewer shows a unified, paged history list (EditChain ops + git commits),
lexical search, and a click-to-inspect detail view. The old filtering controls
are intentionally absent while their replacement is designed. The temporary
fixed view shows all operation kinds and undated rows, hides nested Git
repositories/submodules, applies no summary/kind pattern, and splices hidden
intermediates for graph continuity. Git search hits carry their real identity
(`git_oid` lowercase hex, exact decimal `repository`, `kind: "git"`,
`is_submodule`), so clicking a git result navigates by `ResolveObject` — never
by the synthetic index-only `op_id`. Rapid consecutive searches are
latest-query-wins. Only the viewport row is restored across recreated panels.

All viewer-facing identifiers round-trip as exact JSON strings: op ids are
`node:boot:seq`, git OIDs are lowercase hex, and repository/session/actor ids
are exact decimal `u64` strings — never JSON numbers, so values above 2^53 are
not rounded by JavaScript. The service parses and validates these strings and
returns an `Error` envelope for invalid ids. Search responses use a flat
`SearchHit` DTO (`op_id`, `chunk_id`, `session_id`, `actor_id`, plus git
`git_oid`/`repository` as strings) with timestamps/counts kept numeric. The
read-only JSON editor's `ResolveObject` Ok payload is a typed `ResolvedObject`
DTO under the same rule: `repository` and `changed_paths` are exact decimal
strings, `oid`/`tree`/`parents` are lowercase hex strings, and
`imported_record` is a `node:boot:seq` string when present — never raw u64
numbers or byte-array ID structures.

## Configuration

| Setting | Default | Description |
|---|---|---|
| `editchain-history.servicePath` | `""` | Path to the Rust service binary. Empty = prefer the workspace release build, then fall back to debug. |
| `editchain-history.chainDir` | `.editchain` | Path to the EditChain directory relative to the workspace root. |

## Harness testing (text-first layout debugging)

The webview renderer (`media/main.js`) can be driven headlessly in Chromium so a
text-only agent can inspect the rendered layout without opening VS Code. The
harness loads the **same** renderer + stylesheet and reports geometry as text
first; settled screenshots are also supported via `--shot` (see below).

### Fixture mode (deterministic scenarios)

```sh
npm run ui:dump    -- --scenario merge --viewport 1440x900   # full layout dump
npm run ui:inspect -- --scenario merge --selector ".row"     # one element's geometry
npm run ui:check   -- --scenario merge                       # textual checks only
npm run ui:check   -- --scenario merge --shot shot.png       # ...plus a settled screenshot
```

Scenarios: `empty`, `linear`, `merge`, `mixed`, `filtered`, `undated`, `error`,
`warned`, `large`, `longsummary`, `combined` (expansion), `fork` (lanes), and
`highLanes` (200 concurrent lanes — proves no 128-lane clipping). `check` mode
is **test-blocking**: it exits non-zero when any layout check fails or the page
reports errors.

Scenario interactions exercised by the harness block `check` mode:

- `combined` — clicks the combined op and verifies ALL bundled sub-ops reveal
  inline (not just the first one).
- `--search "query"` — runs the renderer's real search path (input → Enter →
  result list → click) and verifies the results render and navigate to the JSON
  editor, e.g. `npm run ui:check -- --scenario mixed --search message`.
- `--search-race` — with `--scenario merge`, issues two rapid searches with the
  first response held, and verifies the LATEST query wins (search-epoch
  correlation): releasing the older query's late response must not replace the
  newer query's results.
- `--resize` — resizes the viewport and asserts the renderer RECOMPUTES graph
  geometry (header, lane compression, SVG cell widths, rows) instead of just
  stretching the DOM, with lane dots staying inside their cells.
- `warned` — Open responses carrying `warnings`/`diagnostics` (e.g. missing
  blob payloads) render a non-blocking banner above the table while rows still
  load; `OPEN_WARNINGS_VISIBLE` asserts the warning is surfaced, never silently
  discarded.

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
production `media/main.js`, and `media/main.css` — no simulated renderer. The
probe pages through the same bridge the renderer uses, so a structural failure
reflects what the viewer would actually draw. Run against a small chain first
(e.g. a scratch workspace); the harness is memory-conscious but a full scan of
a very large chain still pages every row.

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
  checks inside real VS Code.
- Downloads VS Code + Chromedriver on first run into `.wdio-vscode-service/`
  (gitignored).

To **record the session as video** (useful for reviewing the rendered UI without
a display), run the recording wrapper — it starts Xvfb, captures the display
with ffmpeg, and runs the suite:

```sh
./scripts/ui-vscode-record.sh [out.mp4]   # default: .ui-out/vscode-session.mp4
```

Requires `xvfb` and `ffmpeg`. The output is an h264 MP4 of the full VS Code
session, including the scroll-through-history test.

### How it works

- `test/harness/index.html` mounts `media/main.css` + `media/main.js` with a
  `vscode` shim in place of `acquireVsCodeApi()`.
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
- Search is **lexical-only** by default (no embedding server required). Vector
  search can be added later.
- `Open` is unbounded: building the chain + git graph can take minutes on a
  large workspace. All other service calls carry a generous finite deadline
  (120s by default, ≥ the measured near-minute first window on large chains;
  the graph harness's `--row-timeout` overrides this default); a timed-out
  window/search shows a visible error and suspends background retries until the
  user clicks **Retry** or re-runs the open command (which restarts a crashed
  service and re-opens the chain).
