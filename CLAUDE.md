# CLAUDE.md

This file provides repository guidance to Claude Code.

## Quality

Run `./scripts/lint.sh` before declaring a code task complete. The quality policy
in `AGENTS.md` applies. Keep the checks, thresholds, and tests intact.

## Common commands

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo run --locked -p editchain -- --chain /path/to/chain import \
  --provider claude --input /path/to/sessions
cargo run --locked -p editchain -- --chain /path/to/chain import \
  --provider codex --input ~/.codex/sessions --workspace /path/to/repo \
  --codex-helper /path/to/codex-session-exporter
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
- `editchain-import`: deterministic, incremental Claude, Codex and human archive
  capture, cursors, and store-owned persistence adapters.
- `editchain-index`: rebuildable indexes, refresh and integrity checks;
  `editchain-index-pages` owns paged checkpoint storage.
- `editchain-sync`: operation/blob replication and a generic peer worker with
  caller-owned transport and sharing scope.
- `editchain-git`: repository discovery, history walking, commit/blob resolution,
  and file-change extraction.

Semantic application projections and shared host contracts are in app-core.
Graph geometry and rendering are in web-ui. Native history services, editor
capture, extensions and their debug configurations are in vscode-extension.
The session exporter and portable peer coordination are in Codex. See the
repository links in `README.md`.

## Storage and compatibility

The segment log and blob store are authoritative. Derived indexes can be rebuilt.
Keep the EC02 operation/page format compatible unless a task explicitly
authorizes a format migration. EC02 uses fixed u32 record lengths without a
checksum; EC03 is a separate, currently inactive checksummed format.
