# EditChain storage adapters

`editchain-store` durably stores history records and blobs. The CLI, importers,
and replication services can use it without a viewer.

The [chain index API](../editchain-index/README.md) builds resumable checkpoints,
secondary lookups, content-availability deltas, and integrity/rebuild operations
over these records and blobs. Shared page primitives live below both crates in
`editchain-index-pages`; source records and blobs never depend on a derived checkpoint.

The filesystem implementation remains compatible with existing EC02 segments
and BLAKE3 blob filenames. It preserves supplied operation identities and bytes.

```rust
use editchain_store::{BlobStorage, BlobStore, LogStore, SegmentStore};

fn retain(chain: &std::path::Path, encoded_operation: &[u8], content: &[u8])
    -> std::io::Result<()>
{
    let mut log = LogStore::new(SegmentStore::open(chain)?);
    let _admission = log.append_encoded(encoded_operation)?;
    let mut blobs = BlobStore::new(chain.join("blobs"))?;
    let _reference = blobs.put(content)?;
    Ok(())
}
```

## Contracts

- `AppendLog` exposes exact record replay, append and a durability fence. The
  adapter must own an exclusive writer transaction across replay and append.
  `SegmentStore` implements this with the existing lifetime file lock. Remote
  adapters must provide equivalent transaction ownership themselves.
- `LogStore<L>` adds operation admission to any `AppendLog`: exact replays are
  duplicates; different byte representations under one identity are conflicts.
  Conflicted identities stay quarantined, and every distinct representation is
  retained. Its snapshots preserve complete undecodable records in the backend
  and count them, as well as incomplete tails, explicitly.
- `BlobSource` provides verified, read-only `read_content` and `read_blob` calls.
  `Found`, `Missing`, `Corrupt`, and `Unresolvable` are separate outcomes; backend
  I/O failures return an error. `BlobStorage::put` returns a full BLAKE3 `BlobRef`
  only after durable persistence. Existing `BlobStore`/`BlobReader` convenience
  methods remain available; the adapter methods preserve I/O errors.

Successful writes persist contents and publication metadata. Directory creation
also synchronizes ancestry, so an acknowledged new chain or blob directory does
not rely on an unsynced parent entry. Immutable blobs use exclusive publication;
metadata replacement uses a unique temporary file per writer. Concurrent blob
writers verify any winning publication before acknowledging the same content.
Existing inconsistent blob bytes are retained and reported as an error.

A failed append/publication has an **unknown commit outcome**. Retry with exactly
the same bytes. Readable evidence can precede an acknowledgement, so duplicate
admission re-establishes durability. Conflict admission also syncs its existing
evidence before appending a variant. The legacy import, capture, and replication
retry paths use the same durability fence before acknowledging existing records.

There is no atomic transaction across a record and its referenced blobs. Either
may arrive first. Missing blobs do not alter operation admission, identities or
encoded references, and an existing reader observes a later arrival without
reopening. The filesystem adapter requires working file/directory synchronization
and hard links; it returns errors when those guarantees cannot be provided.

## Segment packing and recovery

Small writes share a segment across writer reopenings.
`SegmentOptions::max_segment_bytes` defaults to 16 MiB. Pages stay together, so
one large page can exceed this target; individual records are limited to 64 MiB.

Older chains start a fresh segment before adopting this layout, recorded in
`.segment-layout`, to preserve existing sharing cutoffs. To force a durable
boundary, rotate and write a page, which can be empty. Replication does this for
"from now" sharing.

Writers check the last segment on opening and after a failed append. Partial
writes stay untouched, and writing resumes in a new segment. Complete records,
including conflict evidence, remain readable with their original bytes and
locations. Invalid framing and missing segments produce errors. Existing small
files are not compacted.

EC02 has no checksum or commit marker: recovery handles incomplete writes but
cannot detect all corruption or make a whole page atomic. Admission still reads
the log on each call.

## Verification

Tests interrupt writes at every byte boundary and check that recovery preserves
exact evidence, identities, locations, and conflict quarantine. They also cover
durable retries, concurrent writes, missing or corrupt blobs, late blob arrivals,
and segment rotation. Existing engine, import, replication, and incremental-reader
tests check compatibility.
