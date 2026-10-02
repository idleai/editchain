# EditChain

EditChain is a Rust engine for immutable human and agent history. Record chains,
operations, actors, sessions, file revisions, annotations, and reflections through
the viewer-independent `editchain-engine` facade and shared `editchain-core`
types. Callers select a chain directly; the engine has no product workspace model.

The existing VS Code view combines Claude Code and Codex sessions with Git
history. Browse agent activity, search recorded text, inspect file changes, and
open diffs alongside the repository's commits.

Experimental; built and run locally.

## Rust engine

See the [engine API guide](./docs/engine-api.md) for the record contract, exact
evidence and conflict semantics, and dependency setup. A standalone example
records every shared record family without starting the viewer or a host service:

```sh
cargo run --locked -p editchain-engine --example headless -- /path/to/chain
```

Running the example again against the same chain replays the same identities and
bytes without adding duplicate records.

The [history import API](./docs/import-api.md) exposes Claude, Codex and human
archive capture with resumable cursors, exact raw evidence and native identity
mappings. Inspect a capture batch, then accept it through durable storage adapters.

See [large-history write paths](docs/scaling.md) for streaming writers, durable
batches, replication caching, and the costs that remain proportional to history.

## CLI

The `editchain` package in `crates/editchain` builds the `editchain` binary directly
on the shared engine libraries. It has no dependency on the legacy node, viewer,
or host protocol crates.

```sh
cargo install --locked --path crates/editchain
editchain --chain /path/to/chain init
editchain --chain /path/to/chain import --provider claude --input /path/to/sessions
editchain --chain /path/to/chain import --provider codex --input /path/to/codex \
  --glob '**/*.jsonl' --workspace /original/repository --progress
editchain --chain /path/to/chain history --output jsonl
```

The [CLI guide](docs/cli.md) covers append/import/export, queries, annotations,
reflections, subscriptions, integrity/rebuild, replication, and shell contracts.

## VS Code extension

Follow the [build and installation guide](./extensions/vscode-editchain/README.md).
Set `editchain-history.servicePath` to the native service binary, open your
project, and run **EditChain: Open History Explorer** from the command palette.

Git history works immediately. Import sessions to add agent activity; the
extension reads it from the project's `.editchain` directory by default.

## Import sessions for the existing viewer

The existing viewer's importer adds host-specific Git reconciliation, human
editor materialization, and viewer checkpoints. It remains in `editchain-node`
as `editchain-legacy` until those features move to their destination repositories.
From the EditChain checkout:

```sh
cargo run --release -p editchain-node --bin editchain-legacy -- import \
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
cargo run --release -p editchain-node --bin editchain-legacy -- import \
  --provider claude \
  --sessions-dir /path/to/claude/project-sessions \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain-new

cargo run --release -p editchain-node --bin editchain-legacy -- import \
  --provider codex \
  --sessions-dir ~/.codex/sessions \
  --codex-helper /path/to/codex-session-exporter \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain-new

cargo run --release -p editchain-node --bin editchain-legacy -- import \
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

`editchain-engine` exposes the native library facade; `editchain-core` owns the
shared immutable schema. `crates/editchain` owns the standalone engine CLI.
`editchain-node` retains the existing native service and legacy viewer commands;
it is separate from the CLI's dependency graph. The extension hosts the Rust/WASM
view from `editchain-history-renderer`. New Idle sidebar/detail views live in
`idleai/vscode-extension` over app-core and web-ui. The temporary
`editchain-client-state` crate builds the legacy peer consumer's small Node/WASM
adapter independently of this renderer. See the
[assembly handoff](extensions/vscode-editchain/ASSEMBLY-MIGRATION.md) for removal conditions.

Run `./scripts/lint.sh` for Rust checks; it requires `cargo-deny`.

The legacy viewer shares portable semantic contracts with `idleai/app-core`.
Check that repository out beside this one as `../app-core` when building this
workspace. Engine crates remain independent of the client-state runtime. The
[history-state migration note](crates/editchain-history-renderer/HISTORY-STATE-MIGRATION.md)
describes the current adapters and their replacement owners.

[Extension tests](./extensions/vscode-editchain/README.md#tests) ·
[Model provenance](./MODELS.md)
