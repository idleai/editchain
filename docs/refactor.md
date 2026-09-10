# Incremental history refactor

The refactor separates durable source evidence, canonical graph facts, and the
displayed Activity snapshot. Each increment retains stored operation identities
and EC02 bytes, with explicit adapters or version changes where a contract must
change. Existing quality gates remain required.

Completed increments:

- Raw capture: Claude normalization returns storage errors. Failed blob writes
  leave the accepted cursor unchanged, and retry retains the complete record.
- EC02 scanning: node and service share the borrowed codec scanner. It reports
  page boundaries, record flags, exact byte locations, and distinct incomplete,
  invalid, unsupported, and oversized-input outcomes. Complete records survive
  an incomplete final write. The checked encoder and reader use a 64 MiB record
  limit; large payloads belong in the blob store. EC02 has no checksum, so a
  plausible truncated record cannot be distinguished from every form of damaged
  length data. Fixed byte fixtures protect the current page/message encoding.

Remaining work, in dependency order:

1. Unify canonical admission and read-only store access, including deterministic
   conflict evidence and consistent viewer/reconciliation behavior.
2. Establish repository-qualified graph identity and a shared repository catalog.
3. Bind service windows, search, layout, and renderer requests to one snapshot.
4. Move complete Activity assembly and presentation-tree ownership into project.
5. Consolidate typed provider evidence, source lifecycle, and persistence
   checkpoints; preserve compatibility with existing normalized records.
6. Simplify search document identity and renderer request/cache state.
7. Retire migrated compatibility machinery and optimize measured repeated work.

Validation uses the existing crate, service, renderer, and extension suites.
Every completed code increment must pass `./scripts/lint.sh`. Regression tests
belong alongside the production contracts they exercise; no separate audit
harness is required.
