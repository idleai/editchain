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

For locally archived human edits, use `--provider human` and point
`--sessions-dir` at the archive file or directory. Enable the archive in
`settings.json`; `directory` defaults to `human-history` under the extension's
global storage and accepts absolute, `~`/`~/`, or (single-folder) relative paths:

```json
{
  "editchain-history.tracking.jsonl.enabled": true,
  "editchain-history.tracking.jsonl.directory": "~/editchain-human-history"
}
```

Archiving requires a trusted workspace and `editchain-history.tracking.enabled`.
See the
[human-history archive](./docs/vscode-human-work.md#portable-human-history-archive).

Imports are incremental and idempotent. The human importer validates each
archive over the byte length captured at discovery and then replays that same
prefix, so an archive still being appended to imports only its captured bytes
and the rest waits for the next import. Add `--dry-run` to preview changes, and
refresh the history panel after importing.

### Rebuild a fresh chain

Claude, Codex, and human raw sources can all be re-imported, so a chain can be
rebuilt from scratch after a breaking format change. Keep the raw sources outside
the chain directory and replace only the chain.

```sh
# 1. Import each provider into a new chain.
cargo run --release -p editchain-node -- import \
  --provider claude \
  --sessions-dir /path/to/claude/project-sessions \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain-new

cargo run --release -p editchain-node -- import \
  --provider codex \
  --sessions-dir ~/.codex/sessions \
  --codex-helper /path/to/codex-session-exporter \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain-new

cargo run --release -p editchain-node -- import \
  --provider human \
  --sessions-dir /path/to/human-history \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain-new
```

Keep `--workspace` at the original project path: the archive records the original
absolute workspace, and the import does not relocate it. Then point the extension
at the new chain with `editchain-history.chainDir` (or move it into place) and run
**EditChain: Open History Explorer**. Importing does not delete the archives, so
the old chain can be removed once the new one is verified.

## Development

`editchain-node` builds the CLI and native service. The extension hosts the
Rust/WASM view from `editchain-history-renderer`.

Run `./scripts/lint.sh` for Rust checks; it requires `cargo-deny`.

[Extension tests](./extensions/vscode-editchain/README.md#tests) ·
[Implementation notes](./docs/refactor.md) ·
[Model provenance](./MODELS.md)
