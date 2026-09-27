# Rebuildable chain indexes

`ChainIndex` provides queries and incremental updates over persisted chain records
and their referenced blobs. Its checkpoint lives in `<chain>/index-v1` and can
be rebuilt without changing the original records or blobs.

```rust
use editchain_core::{ActorId, OpId};
use editchain_index::{ChainIndex, IndexKey};

fn by_actor(chain: &std::path::Path, actor: ActorId) -> std::io::Result<Vec<OpId>> {
    let index = ChainIndex::open(chain)?;
    index.lookup(IndexKey::Actor(actor), None, 100)
}
```

`open` requires an existing chain directory and catches up from its checkpoint.
Only one index handle can own the checkpoint; record and blob writers can keep
appending independently.

## API

| Method | Purpose |
| --- | --- |
| `get` | Read one accepted operation. |
| `operations`, `lookup` | Page through accepted IDs, optionally by actor, scope, file, parent, or content. |
| `record_variants` | Read distinct encoded records and their locations, including conflicting variants. |
| `content` | Check referenced content: available, missing, corrupt, or unresolvable. |
| `refresh` | Report new operations, conflict retractions, and content availability changes. |
| `verify_integrity` | Compare the checkpoint with a full replay and verify referenced blobs. |
| `rebuild`, `rebuild_at` | Reconstruct an open index, or recover a checkpoint that cannot be opened. |

## Refresh and recovery

Queries reflect the last successful refresh. Call `refresh` to read new records
and retry unresolved content, including blobs that arrive after their records.
Pending references survive reopening; idle polls do not replay old history.

Pages use operation-ID order with an exclusive `after` cursor. Use refresh
changes to discover late imports with older IDs. After reopening, query the
current index again: refresh deltas are not a durable subscription log.

A failed source read leaves the previous query state intact. After a checkpoint
write error, reopen before retrying. Available blobs and sealed records are
assumed immutable during refresh; use `verify_integrity` to audit them fully.
Pause writers for a stable audit. Integrity checks report record, blob, and
index problems without modifying either source or index.

Existing page collection APIs remain available through re-exports from
`editchain-index-pages`. See the [crate API documentation](src/lib.rs) and
[storage contracts](../editchain-store/README.md) for details.
