# editchain-sync

Replicates operations and blobs between peers. Callers choose the chain,
authenticate and authorize peers, and supply storage, transport and sharing scope.

- `StoreReplica` connects `AppendLog`, `BlobStorage` and `ExportPolicy` adapters.
- `ExportScope::all` shares the whole store, including future receipts;
  `ExportScope::selected` shares only the chosen record variants and blob hashes.
- `PeerConnection` drives replication through a caller-supplied `Transport`.
- `Session` exposes messages directly for custom event loops.
- `Replica` and `SecurePeer` provide the existing filesystem and TLS integration.

Call `start()` once, feed inbound bytes to `receive()`, and call `tick()` for
catch-up. Transport must deliver ordered bytes. After a failure, create a new
connection; catch-up resumes from durable records.

Replication preserves exact bytes and conflicting variants. Repeated record or
blob receipts are idempotent, and acknowledgements follow durable storage.
Missing blobs are retried in later rounds; check `Progress::unavailable` even
when an inventory round is complete.

`StoreReplica` holds its append adapter's transaction for its lifetime. Hosts
needing concurrent local writes can implement `ReplicationStorage` with short
transactions, as `Replica` does.
