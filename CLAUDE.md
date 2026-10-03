# CLAUDE.md

This file provides repository guidance to Claude Code.

## Quality

Run `./scripts/lint.sh` before declaring a code task complete. The quality policy
in `AGENTS.md` applies. Keep the checks, thresholds, and tests intact.

## Common commands

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo run --locked -p editchain -- --chain /path/to/chain init
cargo run --locked -p editchain -- --chain /path/to/chain append \
  --input operations.jsonl
cargo run --locked -p editchain -- --chain /path/to/chain history --output jsonl
```

## Architecture

This repository contains the engine and standalone CLI. It builds and tests
without sibling application checkouts.

- `editchain`: argument parsing, terminal/pipe I/O, output formats, exit codes,
  and process lifecycle over shared engine APIs. See `docs/cli.md`.
- `editchain-engine`: record facade and factual query API.
- `editchain-core`: immutable schema, identifiers, causal ordering, `OpSet`,
  chain state, and reducers.
- `editchain-store`: canonical read-only chain access, record locations,
  integrity diagnostics, content-addressed blobs, and exclusive segment writers.
  Its `format` API owns postcard operation frames and EC02/EC03 encoding.
- `editchain-index`: rebuildable indexes, refresh and integrity checks;
  `editchain-index-pages` owns paged checkpoint storage.
- `editchain-sync`: operation/blob replication and a generic peer worker with
  caller-owned transport and sharing scope.
- `editchain-git`: repository discovery, history walking, commit/blob resolution,
  and file-change extraction.

Consumers supply records and opaque payloads through the engine APIs. The engine
does not discover application sources, parse provider formats or interpret
application payloads. Replication resolves only schema-declared content fields.

## Storage and compatibility

The segment log and blob store are authoritative. Derived indexes can be rebuilt.
EC03 wire version 2 and operation schema 3 are the current write format. Preserve
read compatibility for older encodings. Wire migration and semantic conversion
are separate operations; see `docs/ec03.md` and `docs/operations.md` for the exact
contracts. Never infer content grants by interpreting opaque payload bytes.
