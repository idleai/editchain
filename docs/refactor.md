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

- Execution controls: helper stdout and stderr have independent byte limits
  (256 MiB and 1 MiB by default), with a two-minute deadline that includes pipe
  draining. Asynchronous pipe reads avoid leaving blocked reader threads when
  a descendant retains a handle. Failure or cancellation terminates the owned
  Unix process group or Windows job and reaps the helper. Source capture, prefix
  hashing, replay, and emission share a cooperative cancellation signal. The CLI
  maps interrupts to that signal and checks it before beginning persistence;
  an append already in progress finishes its durable checkpoint handoff. CLI
  arguments now form one typed request, removing the parallel test-only shape
  and two obsolete lint expectations. Native regressions cover output floods,
  exact output bounds, inherited pipes, cancellation after helper startup, and
  a real CLI interrupt without accepted source evidence. Tokio, process-wrap,
  and ctrlc provide the pipe/process/signal support; prior dependency versions
  are retained in the lockfile. Windows job behavior is not runtime-tested in
  the Linux suite.

- Provider evidence: versioned metadata records capture complete source extents
  and each lifecycle observation before final-item folding or turn removal.
  Projection resolves exact activation and completion endpoints across the
  complete admitted corpus, independent of source arrival order. Missing or
  ambiguous evidence leaves the relationship unresolved; extending a child
  source updates its terminal. Relations retain the supporting occurrence and
  extent IDs without modifying stored operations. Older normalized sources
  receive evidence once without replaying their content. Covered sources stop
  using legacy materialized topology notes, while older chains retain their
  compatibility path. Appending the metadata relationship preserves all prior
  Postcard tags, now pinned by fixed byte assertions. Revision 52 invalidates
  earlier projection caches. Import and projection regressions cover both
  arrival orders, terminal growth, ambiguity, missing records, removal, and
  restart replay.

- Codex semantic revisions: the named `codex-occurrences-v1` contract preserves
  every upsert and explicit turn removal at its witnessing physical record.
  Separate item namespaces prevent sibling lane shifts or cursor-dependent
  content reuse. Source-bound manifests list complete outputs and logical
  changes. Projection rebuilds active logical items, treats post-removal reuse
  as a new incarnation, and retains historical revisions. The compatibility
  view suppresses covered legacy content without changing its stored bytes;
  missing or ambiguous replacement evidence leaves logical state unresolved.
  Named semantic checkpoints track coverage independently of raw and metadata
  progress, allowing one-time normalization or reasoning backfill. Enabling
  reasoning preserves public operation IDs, and disabling capture retains
  previously captured evidence. Output order is explicit in the manifest.
  Revision 53 invalidates earlier projection caches. Regressions compare
  one-shot and multiple append boundaries, input order, logical removal and
  reuse, incomplete evidence, legacy migration, and option changes.

- Capture admission: typed sinks distinguish accepted, duplicate, and conflicting
  operation variants using core's canonical byte contract. Batches bound retained
  variants and their combined encoded size, including later reconciliation;
  exact duplicates remain admissible at capacity. Defaults are one million
  variants and 256 MiB of encoded evidence. The codec measures individual records
  before allocating their output, enforcing its 64 MiB limit. Failed capture or
  extension discards private checkpoints. Reports count retained variants and
  actual duplicate/conflict outcomes. Blob references check their 32-bit lengths
  before storage, and filesystem blob reuse verifies all existing bytes.
  Regressions cover two-file limit failure, exact retry, duplicate admission at
  capacity, conflict retention, and truncated or corrupted existing blobs.

- Claude materialization: the named `claude-blocks-v1` contract assigns content
  slots independently of preceding output counts. Reasoning backfill preserves
  public IDs and bytes; raw-only gaps trigger both content and relationship
  replay. Complete per-record manifests select the displayed replacement while
  all legacy operations remain stored. Both providers use the same manifest
  validation and coverage rules. The shared Claude content builder retains the
  legacy representation for compatibility callers and checks lane exhaustion.
  New derived payloads use the shared fallible 4096-byte spill policy. The unused
  inline-size option was removed because changing representation requires an
  explicit derivation version. Malformed Claude raw records retain their legacy
  inline encoding under the existing source and admission bounds. Revision 54
  invalidates earlier projection caches. Regressions cover reasoning replay,
  append boundaries, raw-only gaps, failed derived blob writes, legacy and
  incomplete replacements, and content beyond the old 16-bit lane capacity.

- Provider Git identity: Codex metadata accepts a neutral repository lookup
  supplied by its host. The CLI adapter uses the shared repository catalog,
  preserving marker-derived IDs for ordinary, nested, sibling, and linked
  worktrees. The importer no longer walks Git markers or duplicates repository
  ID hashing. Missing repositories produce no Git claim; incomplete discovery
  returns an error before the batch can persist its source checkpoint. Tests
  cover catalog identities, supplied/absent lookups, failure, and retry.
  A Git marker inside a skipped directory leaves that cwd unresolved instead
  of assigning the containing repository's identity.

- Search documents: a fallible Tantivy builder publishes an immutable index
  with real operation or repository-qualified Git identities. The service owns
  content selection and the exact snapshot association, including direct builds
  from cached workspaces. Per-chunk generations, synthetic Git operations and
  their side map, and panic-based index defaults are removed. Validated chunk
  options produce UTF-8 ranges directly. Search keeps public message/tool/command
  content and Git labels, adds reflection summaries and known current/old paths,
  and excludes raw JSON, private operations, file bodies, and auxiliary payloads
  before hydration. Full Git content uses canonical source payloads rather than
  display previews. An exact-term field makes full SHA-1/SHA-256 OIDs and known
  paths searchable alongside the existing BM25 prose fields; colon-bearing code
  identifiers retain Tantivy's quoted-query syntax.
  Find now limits distinct visible rows. Bounded continuation resolves hidden
  and repeated chunks and completes score ties for newest-row ordering, examining
  up to 16,384 candidates. The unchanged `more` field distinguishes exhausted
  results from additional visible matches or unexamined candidates. This bounds
  candidate resolution, not Tantivy's internal ranking work. Regressions cover
  identity domains, wide IDs, cached/direct publication, source payload selection,
  hidden and long documents, paging/ties, full OIDs and real paths, UTF-8 ranges,
  invalid limits, and snapshot mismatch. Search remains ephemeral, so existing
  rendered row caches and persisted source bytes do not require a version change.

- Renderer requests and paging: one registry owns request IDs, correlation,
  and the pending window before any effect can send it. IDs stop at JavaScript's
  exact integer bound without reuse; reset clears both ownership structures.
  Correlation envelopes and request diagnostics each retain at most 128 entries,
  preserving the pending window and latest query. Viewport and find paging share
  one snapshot-first planner and skip collapsed descendants when choosing the
  next missing visible row. A sparse cache removes every out-of-range row and
  enforces a 2,000-row publication limit, prioritizing requested visible rows
  before prefetch or collapsed payloads. This is a row-count bound; typed content
  and expansion-state work follow separately. Tests cover 1,000 unanswered
  requests, ID exhaustion, large collapsed spans, scrolling in both directions,
  viewport sizes beyond the cache budget, and the existing snapshot/find/layout
  races. Native renderer tests, WASM clippy, and the browser/host harness pass;
  generated production WASM assets are included.

- Renderer snapshot geometry: validated expansion spans replace the per-row
  visibility vector and repeated top-level prefix sums. Disclosure visits spans
  and stores visible intervals; rank/select and sparse paging cross collapsed
  gaps without walking hidden rows. Expanded row, visible row, and pixel types
  separate cache/find coordinates from DOM measurements. Page ingestion checks
  request bounds, progress, exact pixel coordinates, count totals, duplicate and
  crossing spans, and unchanged snapshot topology before publishing metadata or
  rows. Layout hydration retains disclosure and claims the request slot before
  viewport paging. Explicit Opening, RowsReady, LayoutReady, and Failed phases
  replace independently mutable readiness flags; terminal page failures wait
  for Retry. The existing native million-row regression measured 40 toggles at
  229.8 ms before and 0.010 ms after, with initial installation at 18.6 ms and
  4.0 ms respectively. These are local release measurements with two expandable
  spans, not browser or dense-tree benchmarks. Regressions cover every disclosure
  combination in a nested forest, distant page boundaries, malformed metadata,
  same-snapshot changes, provisional hydration, and constant storage for a plain
  range at the supported coordinate limit.

Remaining work, in dependency order:

1. Finish renderer find/selection ownership, content adapters, and shell lifetime boundaries.
2. Retire migrated compatibility machinery and optimize measured repeated work.

Validation uses the existing crate, service, renderer, and extension suites.
Every completed code increment must pass `./scripts/lint.sh`. Regression tests
belong alongside the production contracts they exercise; no separate audit
harness is required.
