# EditChain

EditChain combines Claude Code and Codex sessions with Git history in one
read-only VS Code view. Browse agent activity, search recorded text, inspect
file changes, and open diffs alongside the repository's commits.

Experimental; built and run locally.

## VS Code extension

Follow the [build and installation guide](./extensions/vscode-editchain/README.md).
Set `editchain-history.servicePath` to the native service binary, open your
project, and run **EditChain: Open History Explorer** from the command palette.

Git history works immediately. Import sessions to add agent activity; the
extension reads it from the project's `.editchain` directory by default.

## Import sessions

From the EditChain checkout:

```sh
cargo run --release -p editchain-node -- import \
  --provider claude \
  --sessions-dir /path/to/claude/project-sessions \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain
```

For Codex, use `--provider codex`, point `--sessions-dir` at `~/.codex/sessions`,
and pass `--codex-helper` with the
[local session exporter](./tools/codex-session-exporter/README.md).
The exporter requires a sibling Codex source checkout.

Imports are incremental. Add `--dry-run` to preview changes, and refresh the
history panel after importing.

## Development

`editchain-node` builds the CLI and native service. The extension hosts the
Rust/WASM view from `editchain-history-renderer`.

Run `./scripts/lint.sh` for Rust checks; it requires `cargo-deny`.

[Extension tests](./extensions/vscode-editchain/README.md#tests) ·
[Implementation notes](./docs/refactor.md) ·
[Model provenance](./MODELS.md)
