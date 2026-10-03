# Scaling large histories

Retain an [Engine::writer()](engine-api.md) across repeated writes. Opening a
writer scans retained operations to build admission state; reopening it for
every batch repeats that work. Writer memory grows with retained identities.

The default segment target is 32 MiB. Producers choose inline or blob payloads;
the standard inline cutoff is 16 MiB. Changing the representation changes the
encoded record, so reuse an ID only with identical bytes.

Persist blobs before operations that reference them. A failed append can leave
a durable prefix. Retry the same immutable records: exact repeats add nothing,
while conflicting variants remain stored and excluded from accepted history.
Only acknowledge an external input checkpoint after durable storage succeeds.

Queries retain their index checkpoint across refreshes. Late operations can
have older IDs than a previous page cursor, so use refresh results or a fresh
snapshot to reconcile a live consumer. Refresh results are not durable cursors.

Integrity, rebuild, export and replication inventories inspect substantial
history. Plan those scans separately from steady-state append costs. EC02 chains
remain readable and require [storage migration](ec03.md) before new writes;
that migration preserves payload representations and retains original segments.
