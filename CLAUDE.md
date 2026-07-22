# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Quality

```sh
# Run the full lint suite (required before declaring a task complete)
./scripts/lint.sh
```

## Commands

```sh
# Build everything
cargo build

# Build a specific crate
cargo build -p editchain-node

# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p editchain-core

# Run a single test by name (supports globbing)
cargo test -p editchain-core --test state -- state_merge_basic
cargo test -p editchain-core --test clock -- clock_causal_ordering

# Run tests in a specific test file
cargo test -p editchain-query --test fusion

# Run the main CLI binary
cargo run --bin editchain -- init .editchain
cargo run --bin editchain -- dump .editchain

# Run the TUI (WIP)
cargo run -p editchain-tui -- ./outputs/cc-chain

# Import Claude Code sessions
cargo run --bin editchain -- import \
  --sessions-dir /path/to/cc-sessions \
  --workspace /path/to/repo \
  --chain ./outputs/cc-chain

# Search the chain
cargo run --bin editchain -- search ./outputs/cc-chain "query" \
  --mode hybrid --top 10 --kind message,tool,command

# Build release binary
cargo build --release --bin editchain

# Lint
cargo clippy --all-targets

# Check formatting
cargo fmt --check

# Generate docs
cargo doc --no-deps
```

## Architecture

Editchain is a CRDT-based edit chain for agent edit history. Ten crates in a Cargo workspace:

### Data flow (top to bottom)

```
Claude Code sessions (JSONL)
  │
  ▼
editchain-import ──► discover → reader → envelope → normalize ──┐
                                                                │
                                                                ▼
editchain-core ◄── Op { id, parents, clock, kind } ◄─────── normalized ops
  │  ▲                                                         │
  │  └── OpSet (grow-only), ChainState, CanonicalView          │
  │      Reducers: MessageReducer, FileReducer                 │
  │                                                             │
  ▼                                                             ▼
editchain-codec ──► postcard binary frames + EC02 page format   │
  │                                                             │
  ▼                                                             ▼
editchain-node ──► SegmentStore (file I/O), CLI (clap), daemon  │
                                                                │
                                                                ▼
editchain-index ──► Tantivy BM25 (RamDirectory) + flat f16 vector index
  │                                                             │
  ▼                                                             ▼
editchain-query ──► HybridSearch (RRF fusion), DAG graph expansion,
                    causal corridor, branch diversity MMR, extractive summarize
```

### Crate responsibilities

**`editchain-core`** (`no_std` compatible) — Schema and CRDT logic. Defines `Op`, `OpId`, `OpKind` (Message, Tool, Command, File, Reflection, Import, Note, Error, ChainStart), `OpSet` (grow-only set with quarantine for conflicting inserts), `ChainState`, `CanonicalView`, and reducers (`MessageReducer`, `FileReducer`). Causal ordering via `CausalKey` (clock → node → boot → seq). All types are `Serialize`/`Deserialize`.

**`editchain-codec`** (`no_std` compatible) — Binary serialization via postcard. Frame-level encoding (`encode_op`/`decode_op`) and page format (`Page` with records + CRC32C checksums). EC02 frame format; EC03 planned.

**`editchain-sync`** (`no_std` compatible) — Sync state machine types (`SyncMsg`, `SyncState`) without IO or runtime. Defines the reconciliation protocol between replicas.

**`editchain-node`** — The main binary crate. Contains:
- `SegmentStore`: file-backed page storage with append/read operations
- CLI via clap with subcommands: `init`, `append`, `dump`, `merge`, `search`, `tail`, `retrieve`, `import`, `serve`
- JSON export (`export.rs`)
- Daemon with Unix socket and ArcSwap snapshot publication (`daemon.rs`)

**`editchain-import`** — Claude Code session import pipeline:
1. `discover`: find JSONL session files in the sessions directory
2. `reader`: streaming line-by-line reader (BufRead)
3. `envelope`: parse raw JSONL lines into typed envelopes (message, tool, command, file)
4. `normalize`: convert envelopes to normalized editchain ops with deterministic IDs (`SourcePosition` packing)
5. `sink`: pluggable output sinks (`MemoryOpSink`, `MemoryBlobSink`, `MemoryCursorStore`)
6. `ids`: deterministic source lane derivation from workspace identity + file path hash

**`editchain-index`** — Search indexes:
- `chunker`: text chunking (768 token window, 96 token overlap, split on message/tool/command/file boundaries)
- `lexical`: Tantivy BM25 index in RamDirectory (no fsync; rebuild from chain on restart)
- `vector`: flat f16 vector index with RoaringBitmap filters for kind/session/path
- `snapshot`: query snapshot types (`QuerySnapshot`, `LexicalSnapshot`, etc.)

**`editchain-query`** — Query plane types and algorithms:
- `search`: request/response types (`SearchRequest`, `SearchResult`, `ScoredChunk`, filters, graph expansion params)
- `fusion`: Reciprocal Rank Fusion (`rrf_fuse`) with configurable k (default 60)
- `hybrid`: `HybridSearch` orchestrator running BM25 + vector in parallel then fusing via RRF
- `graph`: DAG-aware retrieval — causal cone expansion, causal corridor (`retrieve --why`), branch diversity MMR with graph-overlap penalty
- `summarize`: extractive summarization strategies — Extractive, Timeline, ContextPack

**`editchain-embed`** — Embedding model manifests and HTTP inference backend for Qwen3-Embedding-0.6B (512d). Requires a running SGLang server on port 8001.

**`editchain-agent`** — Agent harness for SWE-bench evaluation. Contains Docker environment setup, model configuration, trajectory recording.

**`editchain-tui`** — Ratatui terminal UI for DAG visualization (WIP). Split-pane view with DAG log + inspector. Vim-style navigation.

### Key design decisions

- **Tantivy in RamDirectory**: no fsync overhead; rebuild from chain on restart (~2s for 130K ops)
- **Exact flat vector index first**: f16 vectors (~98 MiB for 100K chunks at 512d); HNSW only if measured latency requires it later
- **RRF fusion**: avoids mixing incompatible BM25 and cosine scores; default k=60, candidates=200 per index
- **ArcSwap snapshots**: lock-free reader access; atomic publication in daemon mode
- **Unix socket daemon**: warm process keeps indexes + model ready; CLI stays clean
- **Qwen3-Embedding-0.6B at 512d**: balance of quality and inference cost; upgrade to -4B if GPU available

### Debugging

See [AGENTS.md](./AGENTS.md) for the interactive debugging contract using Microsoft DebugMCP + vGDB. The workspace is configured for shared VS Code debug sessions where both the developer and agent can control the same debug session through the VS Code UI and DebugMCP tools respectively.