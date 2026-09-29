# Scaling large histories

Use `import --glob '**/*.jsonl'` for a directory or `import --manifest sources.json`
for mixed Claude, Codex, and human history. Both capture and commit one file at a
time through one writer, avoiding a full history scan for each file. For repeated
library writes, retain an [Engine::writer()](engine-api.md).

The defaults are **32 MiB segments, 16 MiB inline payloads**, and a **512 MiB
per-file capture budget**. The capture budget does not bound total writer memory.
See the [CLI guide](cli.md#imports-and-archives) for inputs, limits, and progress.

## Measured corpus

The corpus contains **507 files and 1,451,212 operations**. Both runs below use
the current 32/16 MiB settings; the older run uses EC02 storage.

| Measurement | EC02, 2026-09-28 | EC03, 2026-09-29 |
| --- | --- | --- |
| Full import command | 104.5 s | 174.4 s |
| Capture, normalization, helpers, blob handling | 62.1 s | 85.7 s |
| Admission, log writes, cursor commits | 40.4 s | 79.7 s |
| Stored logs | 109 segments, 3.44 GB | 108 segments, 3.55 GB |
| Blob files | 0 | 0 |

These are separate verification runs under competing host load, not a controlled
speed comparison. Phase counters exclude process startup and teardown.

EC02-to-EC03 migration took **179.4 seconds**, including complete verification.
Every migrated operation matched the fresh EC03 import byte for byte, including
IDs, flags, payloads, and references. Migration retains another 3.44 GB of original
segments; derived indexes are also outside the log totals. See [EC03](ec03.md)
for the migration contract.

The sibling `editchain-sessions-raw/evals/EC03-MIGRATION.md` links the current
validation artifacts. `evals/FINAL-IMPORT.md` and `evals/baseline.json` retain the
original EC02 measurements, including its 5.4-second unchanged-source retry.

## Durability and remaining costs

Imports persist blobs before referencing operations, reserve source identities
before appending, and advance cursors only after durable writes. A failed batch
may leave a committed prefix. Retry the same import: exact repeats add nothing;
conflicting variants remain stored. Indexes rebuild from retained evidence.

Opening a writer still scans retained operations, and its memory use grows with
history. Integrity, rebuild, export, replication inventories, and complete
operation metadata also scan substantial history. Full integrity took 19m 04s
in one concurrent EC03 verification with heavy paging; this is outside import
and migration time.

EC02 chains remain readable but require migration before new writes. Format
migration preserves existing payload storage choices. To change old 4 KiB
captures to the current inline cutoff, recapture into a fresh chain to avoid
[representation conflicts](import-api.md#compatibility-and-checks).
