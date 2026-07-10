# Editchain Seed Implementation Plan

## Overview

Build the minimal embedded CRDT spec (v0.2) as a Rust workspace with `no_std` core,
binary codec, set-union merge, canonical reducers, segment storage, and golden tests.
This follows the "first slice" defined in spec section 16.

## Architecture

```
crates/
  editchain-core/    # no_std schema, IDs, merge, canonical reducers
  editchain-codec/   # no_std binary frames via postcard, optional CRC
  editchain-sync/    # no_std sync state machine (frontiers, msg types)
  editchain-node/    # std: segment files, CLI, JSON export
  editchain-import/  # std: Claude/Codex adapters (future)
```

First slice builds core → codec → node (minimal CLI). Sync and import are stubbed.

## Phase 1 — Workspace + Core Types

**Files:** `Cargo.toml` (workspace root), `crates/editchain-core/Cargo.toml`, `crates/editchain-core/src/lib.rs`

**Types to define:**
- `NodeId` — newtype over `u64` or `u128`
- `OpId` — `{ node: NodeId, boot: u32, seq: u64 }`
- `ActorId` — newtype over `u64`
- `Clock` — enum: `None`, `Lamport(u64)`, `UnixMs(u64)`, `Hybrid { ms: u64, ctr: u16 }`
- `ParentSet` — enum: `None`, `One(OpId)`, `Two(OpId, OpId)`, `Many(BlobRef)`
- `ScopeRef` — enum: `None`, `Chain(ChainId)`, `Session(SessionId)`, `Turn(TurnId)`, `File(PathId)`
- `ContentId` — enum: `Local { node: NodeId, seq: u64 }`, `Hash128([u8;16])`, `Hash256([u8;32])`
- `BlobRef` — `{ id: ContentId, len: u32 }`
- `Payload<'a>` — enum: `Empty`, `Inline(&'a [u8])`, `Blob(BlobRef)`
- Tag bitflags (`u64`) with named constants

All types derive `Serialize`/`Deserialize` via serde with `no_std` compatible derives.

## Phase 2 — Operation Kinds

**File:** `crates/editchain-core/src/op.rs`

**Types:**
- `Op<'a>` — full envelope: id, parents, actor, clock, scope, tags, kind
- `OpKind<'a>` — enum with all 11 variants from spec section 7
- `ChainStart` — chain metadata
- `ActorOp<'a>` — actor registration/metadata
- `MessageOp<'a>` — agent/human message content
- `ToolOp<'a>` — tool call lifecycle (start/delta/finish)
- `CommandOp<'a>` — command lifecycle (start/output/finish)
- `FileOp<'a>` — file revision facts with path, stage, edit
- `FileStage` — enum: Observed/Proposed/Applied/Saved/Deleted
- `FileEdit<'a>` — enum: None/ReplaceBytes/UnifiedDiff/Blob
- `ReflectionOp<'a>` — context summary with scope/covers/window/summary/anchors
- `ImportOp<'a>` — raw external record reference
- `NoteOp<'a>` — relationship graph (corrects/supersedes/rejects/redacts/explains)
- `ErrorOp<'a>` — diagnostic fact
- `UnknownOp<'a>` — opaque preserved fact

## Phase 3 — Canonical State + Reducers

**File:** `crates/editchain-core/src/state.rs`

**Types:**
- `OpSet` — grow-only set keyed by OpId (backed by something like a BTreeMap or sorted Vec for no_std)
- `BlobSet` — content-addressed blob references
- `CanonicalView` — deterministic reduced view of ops
- `ChainState` — { ops: OpSet, blobs: BlobSet, view: CanonicalView }

**Reducer trait + table:**
```rust
pub trait Reducer {
    fn reduce(&mut self, op: &Op) -> Result<(), ReduceError>;
}
```

One reducer per OpKind variant. Canonical causal key ordering:
```
ancestry → clock → node → boot → seq
```

**Admission rules (spec section 4):**
1. Same OpId + same bytes → duplicate; ignore
2. Same OpId + different bytes → invalid duplicate; quarantine/reject
3. New valid OpId → accept
4. Unknown valid kind → accept as Unknown
5. Missing parents → accept as dangling facts

**Merge function:** decode → validate → insert → re-run affected reducers → produce CanonicalView

## Phase 4 — Binary Codec

**Files:** `crates/editchain-codec/Cargo.toml`, `crates/editchain-codec/src/lib.rs`

**Features:**
- Frame encoding via postcard (no_std compatible)
- Optional CRC32 per record and per page
- Page format: magic (`EC02`) | page_seq | records | optional CRC
- Record format: varint_len | flags | encoded_op | optional CRC
- Power-loss tolerance: ignore partial trailing records; stop at first invalid CRC

## Phase 5 — Segment Storage (Node)

**Files:** `crates/editchain-node/Cargo.toml`, `crates/editchain-node/src/lib.rs`

**Features:**
- Append-only segment writer (`*.eclog` files)
- Segment reader with header scan by tag/scope/kind/actor/path
- Directory layout: `.editchain/<chain>/000000.eclog`
- Blob storage directory for large payloads

## Phase 6 — CLI Tool

**File:** `crates/editchain-node/src/bin/editchain.rs`

**Commands (first slice):**
```text
editchain init <chain>          # create new chain directory
editchain append <chain> <op>   # append an operation (JSON input)
editchain dump <chain>          # dump chain as JSON lines
editchain merge <a> <b>         # merge two chains into stdout JSON
```

## Phase 7 — Golden Tests

**File:** `crates/editchain-core/tests/golden.rs`

**Test cases:**
1. Round-trip: encode Op → bytes → decode Op → assert equal for all variants
2. Merge concurrent ops from two nodes → deterministic canonical view
3. File revision register: multiple FileOps for same path → latest wins by causal key
4. Duplicate detection: same OpId + same bytes → ignored; different bytes → quarantined
5. Causal ordering: ops with different ancestry/clocks order deterministically
6. Reflection chain ordering across nodes

## Implementation Order

```
Phase 1 ─► Phase 2 ─► Phase 3 ─► Phase 4 ─► Phase 5 ─► Phase 6 ─► Phase 7
(core      (op kinds) (state +    (codec)    (segment    (CLI)      (golden 
 types)                reducers)              storage)              tests)
```

Each phase produces compilable code with tests before moving to the next.

## Key Design Decisions

1. **no_std core**: core/codec/sync crates use `#![no_std]` with optional `extern crate alloc`. Node and import crates are std-only.

2. **No external CRDT libs**: set-union merge is hand-written. No Automerge/Yjs/Loro.

3. **Postcard for binary**: minimal wire format; serde-compatible with our types.

4. **JSON for debug only**: serde_json in node crate for human-readable export; never in core.

5. **Lifetime parameter on Op**: borrowed payloads for zero-copy decode where possible.

6. **Tag bitflags**: zero-database filter surface via bitwise operations on a u64.

7. **No compaction in core**: Reflection ops manage context; storage pruning is policy outside core.

## Rust Toolchain Setup

First step before any code: install Rust via rustup.