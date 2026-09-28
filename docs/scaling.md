# Scaling large histories

Large imports reuse one writer and batch disk writes. This avoids rescanning
the accumulated history for every operation while preserving original bytes,
stable identities, and duplicate/conflict rules.

Use `import --glob '**/*.jsonl'` for a directory or `import --manifest sources.json`
for multiple Claude, Codex, and human sources. Both capture and commit one file at
a time through the same writer. See the [CLI guide](cli.md#imports-and-archives)
for examples, limits, progress, and timing reports.

For repeated library writes, keep an [Engine::writer()](engine-api.md) open.
CLI append streams, archive restore, and `StoreReplica` also reuse writer state.
Operation and blob writes share synchronization work across batches.

## Durability and retries

Imports persist blobs before their referencing operations. They reserve source
identities before appending operations and advance source cursors only after
those operations are durable. Indexes remain rebuildable from stored evidence.

A failed batch may leave a committed prefix. Retry the same import: exact repeats
add no history, while conflicting variants remain stored. Failed writes discard
uncertain cached state so retries check storage again. Batching preserves the
existing storage format and replication access rules. See the
[import API](import-api.md#cursors-and-retries) for the persistence contract.

## Accepted evaluation baseline

The 2026-09-28 run used **32 MiB segments, 16 MiB inline payloads**, and a
**512 MiB per-file capture budget**. It imported **1,451,212 operations from
507 files** in one CLI process.

| Measurement | Result |
| --- | --- |
| Full import | 104.5 s |
| Capture, normalization, helpers, and blob handling | 62.1 s |
| Operation admission, log writes, and cursor commits | 40.4 s |
| Setup and other overhead | 2.0 s |
| Unchanged re-import | 5.4 s; zero new operations |
| Stored chain | 109 segments, 3.44 GB, 0 blob files |

Validation passed **18 cases with 2,207 assertions**, including full-chain
integrity, index rebuild, import state, and unchanged-source retry. Operation
identities and resolved payload content matched the prior baseline. Index
construction and validation are outside the import time; full-chain archive
restore and replication were not repeated in this run.

These settings are now the runtime defaults. Existing logs remain readable;
recapture history from the old 4 KiB cutoff into a fresh chain to avoid
[representation conflicts](import-api.md#compatibility-and-checks). The sibling
repository's `editchain-sessions-raw/evals/FINAL-IMPORT.md` and `evals/baseline.json`
record the measured configuration, exact executables, and detailed results.

## Remaining costs

Opening a writer still scans existing operations, and its memory use grows with
the retained evidence. Per-file capture limits do not bound total writer memory.
Integrity checks, rebuilds, exports, and full replication inventories still visit
the selected history. Complete operation metadata also scans incoming
relationships. Parsing, hashing, encoding, helper execution, and disk
synchronization still contribute to import time.
