# EditChain History — VS Code Extension

A read-only unified engineering history explorer: EditChain operations (imported
from Claude Code) overlaid with live Git history from the workspace's `.git`
repositories.

## Prerequisites

- **VS Code** 1.85+
- **Rust toolchain** (to build the native service binary)

## Build & install

### 1. Build the native Rust service

```sh
# From the editchain repo root
cargo build -p editchain-vscode-service
```

This produces `target/debug/editchain-vscode-service` (or
`target/release/...` with `--release`).

### 2. Build the extension

```sh
cd extensions/vscode-editchain
npm install
npm run compile        # compiles TS -> out/
```

### 3. Install into VS Code

**Option A — package a `.vsix` and install it:**

> ⚠️ `vsce package` must be run from **inside** `extensions/vscode-editchain`
> (where the extension's `package.json` lives) — **not** from the editchain repo
> root, which has no `package.json`.

```sh
cd extensions/vscode-editchain
npx @vscode/vsce package          # produces editchain-history-0.1.0.vsix
code --install-extension editchain-history-0.1.0.vsix
```

**Option B — run from source (F5 / Extension Development Host):**

Open the `extensions/vscode-editchain` folder in VS Code and press `F5`.
This launches an Extension Development Host with the extension loaded.

### 4. Configure the service path

The extension needs to find the Rust service binary. Set it in VS Code settings:

```json
{
  "editchain-history.servicePath": "/absolute/path/to/target/debug/editchain-vscode-service"
}
```

If unset, it defaults to `<workspace>/target/debug/editchain-vscode-service`.

### 5. Open the viewer

Run the command palette (`Ctrl+Shift+P`) → **"EditChain: Open History Explorer"**.

The viewer shows:
- **History list** — EditChain ops + git commits in one unified, paged list
  (git commits are green-bordered).
- **Search** — type a query and press Enter for lexical search across both
  EditChain and Git history.
- **Inspector** — click a row to see its details (message, refs, changed paths).

## Configuration

| Setting | Default | Description |
|---|---|---|
| `editchain-history.servicePath` | `""` | Path to the Rust service binary. Empty = auto-detect from workspace `target/debug`. |
| `editchain-history.chainDir` | `.editchain` | Path to the EditChain directory relative to the workspace root. |

## Notes

- The viewer is **read-only**: it never mutates Git, the worktree, or canonical
  EditChain storage.
- Search is **lexical-only** by default (no embedding server required). Vector
  search can be added later.