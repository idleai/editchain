# Editchain Minimal Embedded CRDT Spec v0.2

author: GPT5.5 Pro  
transcript: https://chatgpt.com/share/6a4feb3a-75a8-83e8-9d39-94bb942fad58

Small-core Rust spec for an editchain that can run on embedded/IoT-style devices, isolated agents, editor plugins, and desktop gateways.

Non-negotiables: no SQLite, no Automerge/Yjs/Loro, no vector store, no model-runtime dependency, no core compaction. The edit chain itself is the CRDT.

## 1. Thesis

Editchain is a **CRDT state machine encoded as an append-only operation log**.

```text
replica_state = accepted immutable operations + canonical reducers
merge(A, B)  = reduce(accept(A.ops ∪ B.ops))
```

There are no merge conflicts by design. Concurrent operations are valid state. The core defines hard reconciliation rules for all operation kinds, so replicas with the same accepted op set always compute the same canonical state.

Invalid data is not a conflict. Example: same `OpId` with different encoded bytes is corruption/spoofing and is rejected or quarantined outside accepted state.

Logux is the loose inspiration: node-local IDs, immutable action-like records, metadata-separated identity, append-only logs, and offline sync. Editchain applies that shape to agent/human code-history state.

## 2. Core constraints

```text
Rust-first
no_std-capable
allocator-optional
small binary friendly
transport-agnostic
database-free
CRDT-library-free
LLM-runtime-free
importer-agnostic
```

The core must not depend on Claude, Codex, VS Code, GitHub, Tokio, SQLite, search engines, or serving runtimes. Those belong in gateways/adapters.

## 3. Minimal workspace

```text
crates/
  editchain-core/    # no_std schema, IDs, merge, canonical reducers
  editchain-codec/   # no_std frames, postcard, optional CRC/COBS
  editchain-sync/    # no_std sync state machine, no IO/runtime
  editchain-node/    # std segment files, CLI, JSON export
  editchain-import/  # std Claude/Codex adapters, optional
```

Do not split further until append/read/merge/export works.

## 4. State and merge

Canonical distributed state:

```rust
pub struct ChainState {
    pub ops: OpSet,          // grow-only set keyed by OpId
    pub blobs: BlobSet,      // optional local/content refs
    pub view: CanonicalView, // deterministic reducers over ops
}
```

Admission rules:

```text
same OpId + same bytes      => duplicate; ignore
same OpId + different bytes => invalid duplicate; quarantine/reject
new valid OpId              => accept
unknown valid kind          => accept as Unknown
missing parents             => accept as dangling facts
```

Merge rules:

```text
1. Decode incoming records.
2. Validate envelope and duplicate policy.
3. Insert accepted ops into OpSet.
4. Re-run affected reducers.
5. Produce deterministic CanonicalView.
```

No accepted operation mutates another operation. Undo, rejection, correction, redaction, and explanation are later immutable ops.

## 5. Operation identity

Cheap embedded identity is primary:

```rust
pub struct OpId {
    pub node: NodeId, // 64 or 128 bits
    pub boot: u32,   // reboot/epoch/reset discriminator
    pub seq: u64,    // monotonic per node+boot
}
```

Gateways may add proof hashes:

```text
op_hash   = blake3(encoded_op_bytes)
blob_hash = blake3(blob_bytes)
```

Hashes are useful for proof/export, not required for tiny-device merge.

## 6. Operation envelope

```rust
pub struct Op<'a> {
    pub id: OpId,
    pub parents: ParentSet,
    pub actor: ActorId,
    pub clock: Clock,
    pub scope: ScopeRef,
    pub tags: u64,
    pub kind: OpKind<'a>,
}

pub enum ParentSet { None, One(OpId), Two(OpId, OpId), Many(BlobRef) }
pub enum Clock { None, Lamport(u64), UnixMs(u64), Hybrid { ms: u64, ctr: u16 } }
pub enum ScopeRef { None, Chain(ChainId), Session(SessionId), Turn(TurnId), File(PathId) }
```

Tags are the zero-database filter surface:

```text
agent human file message tool command import reflection note error private large_payload
```

## 7. Operation kinds

```rust
pub enum OpKind<'a> {
    ChainStart(ChainStart),
    Actor(ActorOp<'a>),
    Message(MessageOp<'a>),
    Tool(ToolOp<'a>),
    Command(CommandOp<'a>),
    File(FileOp<'a>),
    Reflection(ReflectionOp<'a>),
    Import(ImportOp<'a>),
    Note(NoteOp<'a>),
    Error(ErrorOp<'a>),
    Unknown(UnknownOp<'a>),
}
```

No `Compact` kind. Reflection is the context mechanism. History is never replaced.

## 8. Canonical reducers

Reducers are part of core semantics, not optional projections.

| Kind | Hard reconciliation rule |
|---|---|
| `ChainStart` | First valid chain root by causal key is canonical; others remain facts. |
| `Actor` | Actor fields are causal/LWW registers. |
| `Message` | Grow-only relation, displayed in canonical causal order. |
| `Tool` | Lifecycle by `tool_call_id`: starts, deltas, finish states ordered deterministically. |
| `Command` | Lifecycle by `command_id`: starts, output chunks, finish states ordered deterministically. |
| `File` | File revision register per `PathId`; latest materializing revision wins by causal key. |
| `Reflection` | Reflection register per scope/window; latest valid reflection chain wins by causal key. |
| `Import` | Grow-only raw-source relation. |
| `Note` | Grow-only relationship graph: corrects, supersedes, rejects, redacts, explains. |
| `Error` | Grow-only diagnostic fact. |
| `Unknown` | Preserved opaque fact; ignored except for visibility/export. |

Canonical causal key:

```text
ancestry, then clock, then node, then boot, then seq
```

Concurrent operations are not conflicts. Ties have a stable winner for canonical state, while every operation remains queryable history.

## 9. File state

File history is CRDT state through immutable revision facts. It is not a separate text CRDT.

```rust
pub enum FileStage { Observed, Proposed, Applied, Saved, Deleted }

pub struct FileOp<'a> {
    pub path: PathId,
    pub stage: FileStage,
    pub base: Option<ContentId>,
    pub after: Option<ContentId>,
    pub edit: FileEdit<'a>,
}

pub enum FileEdit<'a> {
    None,
    ReplaceBytes { range: ByteRange, bytes: Payload<'a> },
    UnifiedDiff(Payload<'a>),
    Blob(BlobRef),
}
```

Canonical file reducer:

```text
for each PathId:
  candidates = FileOps with after-state or Deleted tombstone
  winner     = max(candidates, causal_key)
  file[path] = winner.after or deleted
```

A patch without `after` is audit history, not a materializing revision. VS Code capture and agent patch application should record `after` whenever possible.

Parallel human/agent edits are normal state. The canonical materialized file selects one by rule; audit and heatmap views can still inspect all revisions.

## 10. Reflection, not compaction

There is no core compaction.

Agents keep context small with sliding-window reflection ops:

```rust
pub struct ReflectionOp<'a> {
    pub scope: ScopeRef,
    pub covers: FrontierSet,
    pub window: WindowRef,
    pub summary: Payload<'a>,
    pub anchors: Payload<'a>, // optional OpIds, PathIds, symbols, tasks
}
```

Agent context assembly:

```text
latest reflection chain for scope
+ tail ops after covered frontier
+ currently relevant file/message/tool facts
```

Reflection never deletes covered ops and never invalidates proof history. Local byte pruning is storage policy outside core.

## 11. Payloads

```rust
pub enum Payload<'a> { Empty, Inline(&'a [u8]), Blob(BlobRef) }

pub struct BlobRef {
    pub id: ContentId,
    pub len: u32,
}

pub enum ContentId {
    Local { node: NodeId, seq: u64 },
    Hash128([u8; 16]),
    Hash256([u8; 32]),
}
```

Tiny devices may use local content IDs. Gateways can normalize to strong hashes for export.

## 12. Storage and lookup

Embedded storage is framed append-only records:

```text
page:   magic EC02 | page_seq | optional crc
record: varint_len | flags | encoded_op | optional crc
```

Power-loss rule:

```text
ignore partial trailing records
stop page scan at first invalid CRC
accepted earlier records remain valid
```

Desktop segment layout:

```text
.editchain/<chain>/000000.eclog
.editchain/<chain>/000001.eclog
.editchain/<chain>/blobs/<content-id>
```

Lookup is header scanning:

```text
scan frames -> read compact header -> match tags/scope/kind/actor/path -> decode payload if needed
```

Optional sidecars may map path/actor/scope/tag to offsets. They are local acceleration only, not synchronized, not proof material, and never required.

## 13. Sync

Transport is out of scope. Sync is a state machine over frontiers and op ranges.

```rust
pub enum SyncMsg<'a> {
    Hello { node: NodeId, protocol: u16, frontier: FrontierSet },
    Have { frontier: FrontierSet },
    Need { ranges: Payload<'a> },
    Ops { ops: Payload<'a> },
    Ack { frontier: FrontierSet },
    Error { code: u16 },
}

pub struct Frontier { pub node: NodeId, pub boot: u32, pub max_seq: u64 }
```

Tiny devices may sync monotonic ranges only. Gateways repair holes, expand imports, and export proofs.

## 14. Importers

Claude/Codex parsing is gateway-only:

```text
raw external record
-> Import(raw_ref/raw_hash)
-> normalized Message/Tool/Command/File/Reflection ops
```

Rules:

```text
preserve raw bytes or raw hash
unknown external records become Import/Unknown ops
never fail a whole import because one record is unknown
embedded devices never parse Claude/Codex formats
```

## 15. Dependencies

Core/no_std defaults:

```toml
serde = { version = "1", default-features = false, features = ["derive"] }
postcard = { version = "1", default-features = false }
heapless = { version = "0.9", default-features = false, optional = true }
```

Optional core features:

```toml
crc = { version = "3", default-features = false, optional = true }
blake3 = { version = "1", default-features = false, optional = true }
defmt = { version = "1", optional = true }
embedded-io = { version = "0.7", optional = true }
```

Std-only node/import features:

```toml
serde_json = "1"
clap = { version = "4", features = ["derive"] }
similar = "3"
notify = "8"
```

No Tokio in core. No SQLite in this spec.

## 16. First slice

Build only:

```text
OpId, Op, OpKind, Payload, ParentSet
canonical reducer trait and reducer table
binary frame encode/decode
append-only segment reader/writer
set-union merge with invalid-duplicate quarantine
header scan by tag/scope/kind/actor
MessageOp, FileOp, ReflectionOp
JSON debug export
golden tests for round-trip, merge, reducers, deterministic order
```

Do not build viewer, PR proof, rich indexing, model-runtime integration, or full importers until this works.

## 17. Core admission test

A feature belongs in core only if every answer is yes:

```text
Can a tiny device emit or preserve it?
Can it be encoded as an immutable op?
Can it merge by deterministic set-union admission?
Does the core define its reducer semantics?
Can richer desktop views be rebuilt later?
Does it avoid choosing a database/search/runtime?
Does it avoid rewriting history?
```

Otherwise it is an adapter, sidecar, or projection.

## Reference

- Logux core action-log architecture: https://logux.org/guide/architecture/core/
