# EditChain History — VS Code Extension

A read-only unified engineering history explorer: EditChain operations (imported
from Claude Code) overlaid with live Git history from the workspace's `.git`
repositories.

## Prerequisites

- **VS Code** 1.85+
- **Rust toolchain** (to build the native service binary)

## Build & install

```sh
# 1. Build the native Rust service (from the editchain repo root)
cargo build -p editchain-vscode-service

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
auto-detect `<workspace>/target/debug/editchain-vscode-service`.

### Open the viewer

Command palette (`Ctrl+Shift+P`) → **"EditChain: Open History Explorer"**.

The viewer shows a unified, paged history list (EditChain ops + git commits),
lexical search, and a click-to-inspect detail view.

## Configuration

| Setting | Default | Description |
|---|---|---|
| `editchain-history.servicePath` | `""` | Path to the Rust service binary. Empty = auto-detect from workspace `target/debug`. |
| `editchain-history.chainDir` | `.editchain` | Path to the EditChain directory relative to the workspace root. |

## Harness testing (text-only layout debugging)

The webview renderer (`media/main.js`) can be driven headlessly in Chromium so a
text-only agent can inspect the rendered layout without opening VS Code. The
harness loads the **same** renderer + stylesheet and reports geometry as text —
no screenshots.

### Fixture mode (deterministic scenarios)

```sh
npm run ui:dump    -- --scenario merge --viewport 1440x900   # full layout dump
npm run ui:inspect -- --scenario merge --selector ".row"     # one element's geometry
npm run ui:check   -- --scenario merge                       # textual checks only
```

Scenarios: `empty`, `linear`, `merge`, `mixed`, `filtered`, `error`, `large`.
Artifacts are written to `.ui-out/<scenario>/` (`summary.md`, `layout.txt`,
`layout.json`, `svg.json`, `console.txt`, `metrics.json`, `aria.yml`).

### Real-data mode (live Rust service)

```sh
npm run ui:real -- --workspace /path/to/repo --chain-dir .editchain
```

Spawns the actual `editchain-vscode-service`, opens the workspace, and renders
the real chain. Writes a full DOM tree with computed styles to `.ui-out/real/`
(`dom.json`, `dom.txt`, plus the same artifacts as fixture mode).

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