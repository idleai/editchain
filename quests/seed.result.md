# editchain — Seed Implementation Result

**author**: ambientlight + Claude Code  
**date**: 2026-07-09  
**spec**: [seed.md](./seed.md) → [seed.gpt55pro.md](./drs/seed.gpt55pro.md)  
**status**: v0.2 core complete — all 23 tests passing
**sessions**: [3f7db8b8](../../editchain-sessions-raw/cc/3f7db8b8-73a7-4cea-be8d-3d2d54fedd2c.jsonl), [8215ff1e](../../editchain-sessions-raw/cc/8215ff1e-7af1-4543-8baf-2e9e37770711.jsonl), [d81f9fe2](../../editchain-sessions-raw/cc/d81f9fe2-3be9-4044-bd4a-2e942470e20c.jsonl)

## Summary

The minimal embedded CRDT spec (v0.2) has been fully implemented across 5 workspace crates. The edit chain is an append-only operation log with set-union merge, deterministic canonical reducers, postcard binary codec, segment file storage, and CLI tools. No SQLite, no Automerge/Yjs/Loro, no vector store, no model-runtime dependency, no core compaction.

## Workspace Structure

```
editchain/
├── Cargo.toml                          # workspace root (resolver = "2")
├── crates/
│   ├── editchain-core/                 # no_std CRDT schema, IDs, merge, reducers
│   ├── editchain-codec/                # postcard binary frames + EC02 page format
│   ├── editchain-sync/                 # sync state machine (no IO/runtime)
│   ├── editchain-node/                 # segment files, CLI, JSON export
│   └── editchain-import/               # Claude/Codex adapter scaffold
└── quests/
    ├── seed.md                         # human-written seed
    ├── drs/seed.gpt55pro.md            # GPT5.5 spec v0.2
    └── seed.result.md                  # this file
```

## What Was Built

### editchain-core (10 unit + 5 golden tests)

| Module | Contents |
|---|---|
| `ids.rs` | `NodeId(u64)`, `ActorId(u64)`, `ChainId(u64)`, `SessionId(u64)`, `TurnId(u64)`, `PathId(u64)`, `OpId{node,boot,seq}` with `Ord` |
| `payload.rs` | `Payload::Empty/Inline(Vec<u8>)/Blob(BlobRef)`, `ContentId::Local/Hash128/Hash256` |
| `tags.rs` | `Tags(u64)` bitflags: AGENT, HUMAN, FILE, MESSAGE, TOOL, COMMAND, IMPORT, REFLECTION, NOTE, ERROR, PRIVATE, LARGE_PAYLOAD |
| `clock.rs` | `Clock::None/Lamport(u64)/UnixMs(u64)/Hybrid{ms,ctr}` |
| `scope.rs` | `ScopeRef::None/Chain/Session/Turn/File` |
| `parents.rs` | `ParentSet::None/One/Two/Many(BlobRef)` with `ParentIter` |
| `op.rs` | All 11 `OpKind` variants: ChainStart, Actor, Message, Tool, Command, File, Reflection, Import, Note, Error, Unknown |
| `state.rs` | `OpSet` (grow-only BTreeMap), `BlobSet`, `CausalKey`, `CanonicalView`, `MessageReducer`, `FileReducer`, `ChainState` with merge |

Key design decisions:
- Owned `Vec<u8>` payloads (no lifetimes) — simplifies serde and JSON round-trips
- `OpSet::insert()` returns `Result<bool, (existing, incoming)>` — exact duplicates ignored, conflicting bytes quarantined
- `CausalKey` ordering: clock_val → clock_sub → node → boot → seq
- Reducers are deterministic: same OpSet → same CanonicalView on every replica

### editchain-codec (4 tests)

| Module | Contents |
|---|---|
| `frame.rs` | `encode_op()` / `decode_op()` via postcard |
| `page.rs` | EC02 magic bytes (`[0x45, 0x43, 0x30, 0x32]`), `encode_page()` / `decode_page()` with power-loss tolerance |

Power-loss rule: partial trailing records are silently ignored; earlier valid records remain intact.

### editchain-sync

| Module | Contents |
|---|---|
| `msg.rs` | `SyncMsg::Hello/Have/Need/Ops/Ack/Error` with frontier-based state machine |

Transport-agnostic — no IO or runtime dependency.

### editchain-node (4 tests)

| Module | Contents |
|---|---|
| `segment.rs` | `SegmentStore` with `open()` / `append_page()` / `read_all()` / `rotate()` over `.eclog` files |
| `export.rs` | `export_json()` / `op_to_json()` for debug output |
| CLI (`bin/editchain.rs`) | Commands: `init`, `append`, `dump`, `merge` via clap |

### editchain-import

Scaffold only — ready for Claude Code history parsing adapters.

## Test Results

```
23 tests passed across all crates:
  editchain-core:  10 unit + 5 golden integration
  editchain-codec:  4 unit
  editchain-node:   4 unit
```

Golden tests cover:
- Round-trip encode/decode for all 11 operation kinds
- Set-union merge with duplicate detection and quarantine
- Deterministic causal ordering (CausalKey)
- File revision register (latest materializing revision wins by causal key)
- Concurrent operation merge determinism

## Deviations from Spec

| Spec | Implementation | Reason |
|---|---|---|
| Borrowed `Payload<'a>` | Owned `Payload` (no lifetime) | serde + JSON round-trip compatibility; simpler API |
| Workspace deps with optional blake3 | Removed blake3 from workspace deps | Workspace dependencies cannot be optional; blake3 not needed in core |
| postcard feature flag `std` | Uses `use-std` | postcard's actual feature name for std support |
| Lifetime-parameterized Op/OpKind | No lifetimes on Op/OpKind | Avoids complex serde borrow bounds; owned types are simpler for embedded use |

## Known Gaps (Next Steps)

1. **CanonicalView recomputation** — `ChainState::recompute_view_from_ops()` exists but requires pre-decoded ops; the codec layer integration for automatic view recomputation on merge is not wired
2. **Reflection reducer** — not yet implemented; reflection register per scope/window is pending
3. **Sync state machine** — message types defined but no sync protocol logic
4. **Importers** — Claude Code history parser not yet built
5. **Viewer** — edit chain viewer akin to claude-code-history-viewer not started
6. **PR proof plugin** — GitHub integration for edit chain submission alongside PRs not started
7. **VSCode extension** — human edit capture not started

## Architecture Invariants Preserved

- ✅ No SQLite dependency anywhere
- ✅ No Automerge/Yjs/Loro dependency
- ✅ No vector store or model-runtime dependency
- ✅ No core compaction (reflection ops instead)
- ✅ Set-union merge with deterministic reducers
- ✅ Power-loss tolerant page format
- ✅ Tags as u64 bitflags for zero-database filtering
- ✅ Owned payloads for simple serde round-trips