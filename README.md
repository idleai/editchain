# EditChain

EditChain brings agent sessions and Git history into one read-only view in
VS Code. It imports activity from Claude Code and Codex so you can follow the
conversation, commands, and file changes alongside the repository's commits.

This is an experimental project that you build and run locally.

## Explore your project's history

The **EditChain History** extension lets you:

- Browse agent activity and Git commits in a shared history graph.
- Expand work groups and commits to inspect their file changes.
- Search session text and Git messages, then jump to a matching activity.
- Inspect record details and open file changes in VS Code's diff editors.

Git history is available without importing sessions. Import Claude Code or
Codex sessions to add agent activity to the same view.

## Build and run the extension

You need VS Code, Node.js 20 or newer, and Rust installed through rustup.
The repository pins its Rust toolchain in
[`rust-toolchain.toml`](./rust-toolchain.toml).

From the repository root:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked
cargo build --release -p editchain-node --bins --locked

cd extensions/vscode-editchain
npm ci
npm run build:renderer
npm run compile
npm run package
```

In VS Code, run **Extensions: Install from VSIX…** and select the generated
`.vsix` file in `extensions/vscode-editchain`. Open the project you want to
explore, then run **EditChain: Open History Explorer** from the command palette.

Set **`editchain-history.servicePath`** to the absolute path of the native
service you built: `<editchain-checkout>/target/release/editchain-vscode-service`.
When this setting is empty, the extension looks for a release build, then a
debug build, under the open workspace's `target` directory.

The extension reads imported history from `.editchain` in the open workspace.
Use **`editchain-history.chainDir`** to select another directory. The
[extension guide](./extensions/vscode-editchain/README.md) covers configuration,
packaging, and tests in more detail.

## Import agent sessions

Run these commands from the EditChain repository root. Use the same project
directory that you open in VS Code, and write its chain to that project's
`.editchain` directory.

For Claude Code, point `--sessions-dir` at the project's session directory:

```sh
./target/release/editchain import \
  --provider claude \
  --sessions-dir /path/to/claude/project-sessions \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain
```

For Codex, first build the local
[session exporter](./tools/codex-session-exporter/README.md#build-and-run-contract-local-only).
It requires a sibling Codex source checkout; its setup is separate from the
main workspace. Then pass the helper executable to the importer:

```sh
./target/release/editchain import \
  --provider codex \
  --sessions-dir ~/.codex/sessions \
  --codex-helper ./tools/codex-session-exporter/target/release/codex-session-exporter \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain
```

Imports are incremental and can be repeated. Run each provider separately to
include both in one chain. Add `--dry-run` to preview an import without changing
stored history. After importing, refresh the history panel in VS Code.

Imports also prepare the display cache. To rebuild it separately, run:

```sh
./target/release/editchain prepare-view \
  --workspace /path/to/project \
  --chain .editchain
```

## How it works

EditChain stores imported activity as a durable operation log with associated
payloads and import checkpoints under `.editchain`. The native service combines
that history with Git and answers the viewer's window, search, detail, and diff
requests. Display snapshots and the search index are derived from those sources.

| Component | Role |
| --- | --- |
| [`editchain-node`](./crates/editchain-node/) | Builds the `editchain` import CLI and the `editchain-vscode-service` native backend. |
| [`editchain-history-renderer`](./crates/editchain-history-renderer/) | Renders the history view in Rust/WASM, including the graph, paging, search navigation, and selection. |
| [VS Code extension](./extensions/vscode-editchain/) | Hosts the renderer, connects it to the service over framed stdio, and opens details and diffs in VS Code. |

The viewer is read-only; the CLI writes imports. Keep the chain's operation
files, blobs, and checkpoints together when backing up imported history.

## Development

Run the repository's Rust checks from the root:

```sh
cargo install cargo-deny --locked
./scripts/lint.sh
```

For extension and browser tests, see the
[extension test guide](./extensions/vscode-editchain/README.md#tests).
[Implementation notes](./docs/refactor.md) describe storage, import, and API
contracts. [MODELS.md](./MODELS.md) records the project's model provenance.
