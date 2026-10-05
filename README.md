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

Our reusable crates are stored as `.crate` assets in this repository's GitHub
Releases. The `cargo-index` branch contains the Cargo sparse index; its entries
include immutable archive checksums. `.cargo/config.toml` registers the indexes.
Normal checks use the committed lockfile and need only this repository's source.

A successful `main` CI run starts the Release workflow. Release-plz calculates
versions and changelogs, and automation commits that metadata to `main`. The
entire CI workflow checks the version commit before any package is published.
Package archives, indexes and native bundles then publish from that exact commit;
there is no separate release PR. Concurrent changes to `main` are never overwritten.

Declare breaking changes in the feature PR, including the required minimum
versions in consumers. Release-plz uses commit messages and Rust API checks to
calculate the next version. To recover a failed publication, use **Re-run failed
jobs** on that Release run, retaining its verified commit even if `main` has
advanced. Dispatch **Release** on `main` to prepare current changes or resume a
current version commit. Existing versions and public archives remain immutable;
retries can complete unfinished drafts. A documentation-only change that does not alter packaged
contents does not create another package version.

Dependabot requires a secret reference for custom Cargo registries, including
public ones. Set the repository's Dependabot secret `PUBLIC_CARGO_REGISTRY_TOKEN`
to the literal value `anonymous`. This is a public marker, not an access token;
the GitHub indexes remain anonymously readable.

The native release workflow builds Linux x64, macOS x64/arm64 and Windows x64
bundles when releasing the native tools. It publishes the draft only after all
platform builds complete. `native-release.json` defines the binaries and test
support owned by this producer.


## Coordinated development

For ordinary local Rust work, add a temporary Cargo patch for the relevant
registry and pass it with `cargo --config /absolute/path/local.toml ...`.
Keep these overrides out of committed manifests and lockfiles. Full checks with
an unpublished producer can use `memos/scripts/check-integration.py` with
explicit `--producer` and `--consumer` checkout paths. It temporarily patches
Cargo, builds candidate native bundles when needed, runs the consumer's normal
check script and restores its dependency files. The manual **Unpublished package
integration** workflow in memos runs the same check for selected branches.
