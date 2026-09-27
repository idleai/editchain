# Engine API

Depend on `editchain-engine` to read and write a chain without the viewer, editor
integration, or a running service. Depend on `editchain-core` alone when only the
shared record schema is needed.

```toml
[dependencies]
editchain-engine = { path = "../editchain/crates/editchain-engine" }
```

`Engine::open(path)` opens or creates the caller-selected chain directory. It does
not allocate an identity or append a record. Paths locate storage; logical chain
IDs are supplied by producers and recorded using `ScopeRef::Chain(ChainId)`.
Product membership, workspace bindings, controller policy, and interpretation of
annotations belong to consumers.

## Shared records

Both crates expose `records`, which names the canonical envelope and its payloads:

| Shared name | Canonical type / operation kind | Identity |
| --- | --- | --- |
| `OperationRecord` | `Op` | `OpId { node, boot, seq }` |
| `ChainRecord` | `ChainStart` / `OpKind::ChainStart` | Recorded `ChainId` scope; operation ID identifies the initialization fact |
| `ActorRecord` | `ActorOp` / `OpKind::Actor` | Envelope `ActorId`; operation ID identifies each registration or metadata fact |
| `SessionRecord` | `SessionOp` / `OpKind::Session` | Explicit `SessionId`, independent of observation identity |
| `RevisionRecord` | `FileOp` / `OpKind::File` | Operation occurrence plus `PathId`; content IDs identify bytes |
| `AnnotationRecord` | `NoteOp` / `OpKind::Note` | Operation ID, with explicit target operation IDs |
| `ReflectionRecord` | `ReflectionOp` / `OpKind::Reflection` | Operation ID, with recorded scope, coverage, window, and anchors |

These names are re-exports, not a second serialized schema. Payloads remain inside
an `Op` carrying the original identity, causal parents, actor, clock, scope, and
tags. Equal file contents do not collapse distinct observations, edits, or saves.
Session labels do not establish identity or parentage; `SessionOp::parent` is an
explicit producer observation. Its metadata is opaque bytes or a blob reference,
so producers can retain complete native/provider IDs and original evidence.

Existing operation variants retain their field order and binary discriminants.
The session kind is appended after them. Readers built before this addition
cannot decode a session operation; existing storage readers count unsupported
records and retain their segment bytes. Existing records need no migration.

## Append and inspect

The [headless example](../crates/editchain-engine/examples/headless.rs) creates
every record family, including two revision occurrences with identical content:

```sh
cargo run --locked -p editchain-engine --example headless -- /path/to/chain
```

It prints `accepted=7 conflicts=0` in a fresh chain. Repeating the command uses the
same IDs and bytes, producing the same result. Real producers must allocate their
own stable IDs instead of copying the example's fixed identities.

| API | Behavior |
| --- | --- |
| `Engine::append(&Op)` | Encode a newly authored operation and durably retain it |
| `Engine::append_encoded(&[u8])` | Validate and retain original encoded bytes without re-encoding |
| `Engine::snapshot()` | Read accepted operations, evidence, and integrity counts |
| `ChainSnapshot::read(path)` | Read without creating files; a missing chain is empty |
| `snapshot.get(id)` / `operations()` | Accepted operations in deterministic operation-ID order |
| `snapshot.evidence().evidence()` | All distinct decodable byte representations, including conflicts |
| `snapshot.evidence().conflicts()` | Quarantined identities and every retained variant |
| `encode_op` / `decode_op` | The existing operation codec, re-exported for consumers |

An append returns one of three outcomes:

- `Accepted`: this ID has one known byte representation.
- `Duplicate`: these exact bytes were already retained; no new physical record
  is appended, including when the ID is conflicted.
- `Conflict`: another representation exists under this ID. Both are retained,
  and the entire ID is excluded from accepted history. Replaying an older variant
  never restores that ID to accepted history.

Byte equality is authoritative even when two encodings decode to equal values.
Use `append_encoded` for imported or replicated evidence. Re-encoding accepted
operations would lose original encodings and omit conflicts; replay the exact
evidence iterator when copying decodable history.

Each append acquires the existing writer lock and reads current admission state.
Competing writers receive `WouldBlock` and can retry. Success follows storage
synchronization; duplicate retries resynchronize retained evidence in case an
earlier write reached the filesystem but failed during sync. An error can leave
complete or partial bytes, so retry with the same identity and bytes. Subsequent
appends use a fresh segment rather than overwriting an incomplete tail.

Snapshots remain unchanged as later appends arrive. Read another snapshot for
new history. `stats()` reports accepted records, physical replays, quarantined
variants, incomplete tails, and undecodable records. Unsupported record bytes
remain in their segment files and are not included in the decoded evidence
iterator. That iterator alone is not a byte-for-byte archive of segment files.

## Exact content

`store_blob(bytes)` durably stores the complete bytes and returns a full BLAKE3
`BlobRef`. `resolve_blob`, `resolve_content`, and `resolve_payload` distinguish
`Found(bytes)`, `Missing`, `Corrupt`, and `Unresolvable`; filesystem errors remain
errors. Blob references also validate their declared length. Local and truncated
content IDs are unresolvable by this filesystem adapter.

Records may arrive before their blobs. Resolving content again observes a late
blob without reopening the engine or rewriting the operation. No text decoding,
newline normalization, truncation, or metadata interpretation occurs in these
APIs. Existing bytes that disagree with their content address are left untouched
and produce an error on a repeated write.

The facade performs a full canonical read per append or snapshot. For indexed
history, search, content, diff, Git, and operation context, use `Engine::queries()`
or `queries::ChainQueries::from_index(index)`. The [query guide](engine-queries.md)
describes record references, pagination, refresh, and explicit content gaps.
Import and replication implementations remain in their respective crates; the
facade's dependencies contain no viewer, node service, protocol, or presentation crate.
