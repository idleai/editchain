# EditChain

EditChain is a Rust engine for immutable records. It provides operation schemas,
storage, conflict handling, indexes, queries, content and replication. Callers
select a chain directly and supply their records and access rules.

Experimental; built and run locally.

## Engine API

See the [engine API guide](docs/engine-api.md) for record contracts, exact stored
bytes, conflict semantics and dependency setup. A standalone example records
every shared record family:

```sh
cargo run --locked -p editchain-engine --example headless -- /path/to/chain
```

Repeating it against the same chain replays identical records without adding
duplicates. See [large-history write paths](docs/scaling.md) for writer reuse,
durable batches, replication and full-history costs.

## CLI

The `editchain` package builds the standalone engine CLI:

```sh
cargo install --locked --path crates/editchain
editchain --chain /path/to/chain init
editchain --chain /path/to/chain append --input operations.jsonl
editchain --chain /path/to/chain history --output jsonl
```

The [CLI guide](docs/cli.md) covers append/export, queries, annotations,
reflections, subscriptions, integrity/rebuild, replication and shell contracts.

## Packages

This workspace contains nine packages: `editchain`, `editchain-engine`,
`editchain-core`, `editchain-store`, `editchain-index`, `editchain-index-pages`,
`editchain-git`, `editchain-sync` and `editchain-cli-support`. The CLI support
package shares bounded input, output formatting and exit codes with native
application tools. The peer worker in `editchain-sync` accepts
caller-supplied transport and sharing scope. Replication follows declared engine
content references; it does not interpret opaque payloads as additional grants.

## Development

No sibling repositories, Node or WASM target are required for this workspace.

```sh
cargo build --workspace --locked
cargo install cargo-deny --locked
./scripts/lint.sh
```

[Model history](MODELS.md)

## Package releases

Reusable crates and native CLI bundles publish through GitHub Releases after CI
passes. Cargo consumers use this repository's sparse index. See
[packaging and releases](docs/packaging.md) for publication, retries and testing
changes across repositories.
