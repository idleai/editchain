# EditChain

EditChain is a Rust engine for immutable human and agent history. Record chains,
operations, actors, sessions, file revisions, annotations, and reflections through
the `editchain-engine` facade and shared `editchain-core` types. Callers select a
chain directly; the engine has no product workspace model.

Experimental; built and run locally.

## Rust engine

See the [engine API guide](docs/engine-api.md) for the record contract, exact
stored bytes, conflict semantics, and dependency setup. A standalone example
records every shared record family:

```sh
cargo run --locked -p editchain-engine --example headless -- /path/to/chain
```

Running it again against the same chain replays the same identities and bytes
without adding duplicate records.

The [history import API](docs/import-api.md) exposes Claude, Codex and human
archive capture with resumable cursors, raw records and native identity mappings.
Inspect a capture batch, then accept it through durable storage adapters.
See [large-history write paths](docs/scaling.md) for streaming writers, durable
batches, replication caching, and costs proportional to history.

## CLI

The `editchain` package builds the standalone engine CLI:

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
Codex import invokes a caller-selected `codex-session-exporter` executable; its
[source and build guide](../codex/tools/codex-session-exporter/README.md) live in
Codex. Building and testing the engine uses recorded helper output and needs no
Codex checkout.

## Repository boundaries

This workspace contains nine engine packages: `editchain`, `editchain-engine`,
`editchain-core`, `editchain-store`, `editchain-index`, `editchain-index-pages`,
`editchain-import`, `editchain-git`, and `editchain-sync`. The generic peer worker
remains in `editchain-sync`; hosts supply transport and sharing scope.

Application components live in their owning repositories:

- [app-core](../app-core): application state, semantic history projections,
  shared host contracts, and the legacy peer state WASM adapter.
- [web-ui](../web-ui): graph geometry, browser rendering, and history styles.
- [vscode-extension](../vscode-extension): editor capture, native history service,
  both extension packages, VS Code integration checks, and installation helpers.
- [Codex](../codex): the session exporter and portable history peer coordination.

The existing history extension keeps its command, setting and archive contracts.
Its [build guide](../vscode-extension/extensions/vscode-editchain/README.md) and
[viewer import guide](../vscode-extension/docs/legacy-import.md) describe that host.

## Development

No sibling repositories, Node, or WASM target are required for this workspace.

```sh
cargo build --workspace --locked
cargo install cargo-deny --locked
./scripts/lint.sh
```

[Model history](MODELS.md)
