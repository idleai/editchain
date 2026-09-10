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
- Canonical storage: core retains a sorted set of all byte variants per ID.
  Conflicted IDs are entirely absent from accepted history, including after
  merge or replay. Node reconciliation and the viewer use `editchain-store` for
  the same decoding, admission, and detail locations. Strict operation decoding
  reports trailing or unsupported bytes as undecodable while retaining source
  files. Read-only access creates no directories; missing segments are errors.
  Segment writers hold an exclusive lock and check sequence exhaustion.
  Render projection revision 45 invalidates caches using first-version admission.
- Git graph identity: `GitCommitKey` keeps repository and OID together through
  scheduling, parent edges, structural markers, and display keys. Git row keys
  are now `git:<repository>:<oid>`; full OID/detail fields retain their existing
  representation. Projection revision 46 invalidates earlier row-key caches.
- Repository catalog: worktree roots, Git directories, common directories, and
  legacy marker-derived IDs are explicit. Discovery retains failures and skips
  directory symlinks; reconciliation requires complete discovery and open
  results. Component-wise containment handles nested and prefix-related sibling
  repositories. Open diagnostics report unavailable repositories, and full-OID
  commit resolution rejects tree/blob objects without panicking. Projection
  revision 47 invalidates caches built with string-prefix nesting.

Remaining work, in dependency order:

1. Separate immutable Git reads from ref observations and expose history completeness.
2. Bind service windows, search, layout, and renderer requests to one snapshot.
3. Move complete Activity assembly and presentation-tree ownership into project.
4. Consolidate typed provider evidence, source lifecycle, and persistence
   checkpoints; preserve compatibility with existing normalized records.
5. Simplify search document identity and renderer request/cache state.
6. Retire migrated compatibility machinery and optimize measured repeated work.

Validation uses the existing crate, service, renderer, and extension suites.
Every completed code increment must pass `./scripts/lint.sh`. Regression tests
belong alongside the production contracts they exercise; no separate audit
harness is required.
