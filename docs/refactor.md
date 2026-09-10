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
- Git observations: history captures ref labels once and retains traversal,
  decode, shallow-boundary, and truncation outcomes with available commits.
  Prefix lookup distinguishes absence, ambiguity, wrong object kind, and read
  failure; an incomplete repository set cannot establish global uniqueness.
  Projection revision 48 invalidates caches that omitted history diagnostics.
- Protocol boundary: shared classification types live in core, so protocol no
  longer depends on projection algorithms. Errors retain machine-readable codes
  inside the existing envelope; clients accept both structured and legacy string
  errors. Requests validate page, search, query, and exact-coordinate limits.
  The stdio reader rejects frames above 8 MiB before allocating their payloads.
- Opened source lifetime: cached rows remain fixed. Lazy projection and search
  compare the pinned source version before and after reading, and report
  `StaleSnapshot` on a change. A complete computed backend replaces the cached
  backend atomically; both windows and search then use it. Revision 49 includes
  ref labels, shallow metadata, and blob/object availability inventories in
  cache identity. Window read failures remain errors, and cached offset and
  expansion indices are checked before use.
- Snapshot protocol: version 2 negotiates an opaque identity in Open and
  requires it on window, search, detail, object, and diff requests. Typed result
  decoding checks both the requested identity and the active view; search jumps
  also verify the destination row key. The host carries each row action's
  original identity. Explicit refresh bypasses derived caches and retires old
  tokens even when the cache fingerprint is unchanged. Old services receive
  the unchanged Open request and produce a visible version error. Rust, browser,
  and host regressions cover stale responses and refresh ownership.
- Graph stages: scheduling, filtering, and Activity contraction use typed
  operation/Git keys and a common parent contract. Derived parents preserve
  complete source envelopes for every row kind, including mixed-domain edges
  and more than two parents. An immutable resolved table supplies layout and
  structural labels, retaining supporting note/source IDs. Projection inputs
  are read-only through their public API. Hidden ancestry uses an iterative
  traversal; a 20,000-record regression protects it. Revision 50 invalidates
  caches from the earlier parent-rewrite implementation.
- Activity ownership: project now builds the complete view in one pass order.
  An immutable presentation tree supplies descendants, direct-child summaries,
  expansion intervals, and row offsets. The same retained graph supplies
  provisional rows and lazy layout. Find resolves source identities through
  the view's lazy ownership map; omitted sources retain explicit reasons.
  Service supplies display content and file details through an adapter. The
  former service snapshot, parallel expansion state, and hierarchy builder are
  removed. Revision 51 binds derived caches to this view contract.
- Source time: Claude, Codex, title capture, and Git reconciliation share a
  neutral RFC 3339 parser using the already-locked `time` dependency. Calendar,
  digit, separator, offset, and pre-epoch validation replaces unchecked date
  arithmetic. Fractions retain their millisecond precision. Invalid source time
  stays unknown while capture preserves the complete raw record. Stored clocks
  remain unchanged; corrected parsing applies to newly captured evidence.

- Source capture: both providers use a shared read plan over one private file
  copy. Accepted-prefix hashing, new complete records, partial-line status,
  metadata replay, and Codex helper input share those captured bytes. Live
  growth is deferred to the next import. A rewritten source proposes a checked
  generation increment; emission failures leave the previous generation and
  cursor unchanged. The legacy reader now rejects changed accepted bytes too.
  Configurable defaults bound a source to 512 MiB, a record to 64 MiB, and a
  generation to one million complete records. Capturing adds temporary disk IO;
  unchanged-source cost and aggregate memory still need measurement. Segment
  writers also release their lock explicitly at drop, even when a concurrently
  spawned process temporarily retains a duplicate descriptor.

- Persistence handoff: capture returns an import batch with private proposed
  checkpoints. Failed capture or append cannot stage those checkpoints in the
  caller's store. The CLI acquires the writer lock before reading any cursor,
  appends distinct canonical variants in pages targeting 4 MiB, and commits
  checkpoints only after durable append. Its completion report follows that
  commit and includes actual duplicate/conflict admission. Conflicting variants
  remain durable evidence. Source-prefix reservations written before append
  prevent ID reuse if a failed append is followed by another source rewrite;
  unchanged retries retain their exact operation envelopes. A versioned
  checkpoint journal binds each accepted
  prefix to its generation and completes interrupted metadata writes on reopen,
  including when the source changes again before restart. A crash before the
  journal replays the same immutable operations. Legacy cursor/generation files
  remain readable; cursors gain the paired field on their next successful import,
  and the retained generation map continues to support explicit cursor reset.

Remaining work, in dependency order:

1. Consolidate typed provider evidence, source lifecycle, and persistence
   checkpoints; preserve compatibility with existing normalized records.
2. Simplify search document identity and renderer request/cache state.
3. Retire migrated compatibility machinery and optimize measured repeated work.

Validation uses the existing crate, service, renderer, and extension suites.
Every completed code increment must pass `./scripts/lint.sh`. Regression tests
belong alongside the production contracts they exercise; no separate audit
harness is required.
