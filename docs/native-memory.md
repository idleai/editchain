# Resumable live history

The extension now opens a persistent native checkpoint and requests its viewport.
It no longer loads every operation or transfers the full graph to WASM when a
prepared live view opens. New canonical records update only their affected
indexes, blocks and graph boundaries; the next checkpoint reuses unchanged pages.

## Measurements

The scale fixture starts with the same 2,421,697 accepted operations used in the
previous memory investigation. Native timings include the length-prefixed stdio
transport. The new path uses `OpenLivePaged`; historical full-topology results
use `OpenLive` and are retained below as the comparison, not as equivalent work.

| Measurement | Original `20c451a` | First memory refactor | Prepared native paging |
| --- | ---: | ---: | ---: |
| Open response | 184.23 s | 145.16 s | 0.066 s |
| Open plus first 500 rows | not measured | not measured | 0.399 s |
| Retained native RSS after sampled windows | 20.24 GiB | 10.69 GiB | 60.2 MiB |
| Open frame | 483,770,705 bytes | 483,770,705 bytes | 617 bytes |
| Historical records decoded during Open | all | all | 0 |

On a fresh process against that prepared fixture, a single appended message took
0.377 s including durable checkpoint publication. The counters were **one record,
55 chain bytes, one presentation input and one changed block**; the response was
1,698 bytes and retained native RSS was 64.7 MiB. Restarting again returned the
new row without replaying any records. All seven sampled windows matched the
post-append view across that restart, excluding the ephemeral snapshot ID.

These are individual local runs with a warm OS page cache, not a p95 guarantee or
VS Code first-paint timings. The first pre-generation took 5m38s, peaked at
12.70 GiB RSS and wrote about 15 GB of derived storage. Preparation remains an
offline, corpus-sized job. The paged total is the current **visible** count
(329,952 before the append); the old 560,416 figure is the fully expanded count.
No history was deleted to achieve the smaller visible count.

The actual workspace was also prepared and its current Codex source caught up.
A subsequent fresh process opened its 2,425,608-operation checkpoint and first
500 rows in 0.243 s, with 31.9 MiB retained native RSS. Those operations include
the new real session records; this is separate from the frozen-input comparison.

## Storage and update path

`CHAIN/live-v1` contains a checksummed, append-only page store, a durable row file,
a persistent Tantivy summary index and an atomically replaced checkpoint root.
The canonical `.eclog` segments and blobs remain authoritative.

The checkpoint includes canonical admission/quarantine and the segment frontier,
logical-item reducers and occurrence proofs, reverse dependencies, ancestry,
task paths, graph lanes/routes, weighted ordering and disclosure. Adaptive
hash-trie buckets and AVL nodes are lazily loaded. Mutation dirties only the
visited paths; a new root references old pages directly. Decoded index pages
are released after reads and successful transactions. Immutable operation
allocations are shared while hydrated. Row payloads are read only for requested
windows or affected delta blocks, and their checksums are verified.

Rows and search changes become durable before publishing the index root. A
crash before root replacement leaves the previous checkpoint usable; appended
canonical evidence is replayed from its old frontier. Source identity, size and
modification fences reject replaced/truncated segments. Opening stats the sealed
segments but does not decode their contents. Schema/workspace and repository
identity checks prevent adopting incompatible state. The cache has one exclusive
writer; a second native runtime for the same chain is rejected.

Exact operation admission still compares encoded bytes at durable record
locations, including after restart. Checksums do not replace canonical conflict
classification. Conflicting IDs remain quarantined, and a corrupt lazy page
aborts the request instead of behaving like a missing dependency.

## Viewport and publication

`OpenLivePaged` returns an epoch, revision and visible count with an empty global
block list. Native rank/select, disclosure and graph decoration serve bounded
`GetWindow` requests. `LocateRows` resolves continuity keys in the current
visible coordinates, including the actual identity of a detail row.

Task and local detail toggles run natively. Offscreen history starts folded;
the latest task and visible arrivals open their paths by default. Explicit
choices persist, and folds preserve graph junctions. Search is built during
preparation and updated on each block edit. Finding a folded member exposes
it through a revisioned update.

The host serializes capture, toggles and search-induced disclosure publication.
It waits for the renderer to acknowledge the replacement viewport before moving
its cursor. Read-only window/anchor requests remain available during that
handoff. A cancelled search cannot strand the barrier; after disclosure, the
renderer repeats its current query against the published coordinates.

The renderer retains only viewport content and an identity coordinate mapping.
It keeps the old keyed DOM while locating anchors and loading the replacement
page, including at a distant scroll position. Lane-header sizing changes after
that page arrives. Existing row movement, growing connections, stable lane
spacing, selection and reduced-motion behavior remain in use. Capture starts
after the first saved viewport acknowledges readiness.

## Preparation and recovery

```sh
cargo build --release -p editchain-node --bin editchain --bin editchain-vscode-service
./target/release/editchain prepare-view --workspace /absolute/workspace \
  --chain /absolute/workspace/.editchain
```

`prepare-view` advances an existing valid checkpoint additively. Successful CLI
imports also attempt to advance it. A first open without a checkpoint still
prepares one synchronously, so pre-generation is required for a predictable
large-workspace opening budget.

Task-path schema 2 has an explicit migration from schema 1: close History and
run `prepare-view` once. It rebuilds only derived task membership/disclosure,
reusing canonical reducers, stored rows, search and graph lanes. The directory
remains `CHAIN/live-v1`; the root carries the schema version. A normal Open
rejects an old schema with preparation instructions instead of running migration.

Graph schema 3 repairs the missing provider spawn/completion relationships and
timestamp-only ordering in schemas 1–2. `prepare-view` reuses canonical operations,
row pages, search and existing lane identities while rebuilding relationship
indexes, causal ordering and affected task paths. Ordinary opening never runs this
migration. On the 2,425,608-operation workspace, the one-time repair took 205.32 s
and peaked at 7,309,564 KiB RSS; this offline preparation remains expensive.
Prepared native opening plus 500 rows then took 0.243 s with 31,244 KiB retained
RSS (35,060 KiB peak), decoded no old chain records, and idle sync did no work.
Evidence is in `.ui-out/subagent-repair-20260912T183902Z` under the extension.

Disclosure schema 4 defaults offscreen task paths to folded. Upgrading schema 3
through import or `prepare-view`
remeasures saved disclosure and visible rank weights; it does not rebuild
canonical admission, provider relationships, graph lanes, row content or search.
Legacy group choices reset once because they did not distinguish automatic
opening from a user choice. Explicit opens made with schema 4 persist. The latest
task now opens on the first head viewport, and visible arrivals open their whole
path. Automatic paths close only after all members leave view, or on restart
before the new viewport is known. Added optional fields track automatic opens
and explicit closes; existing schema-4 checkpoints need no preparation or global
reset. Old per-row exposure is cleared by its bounded saved key set on reopen.
Viewport membership checks inspect reported rows and automatic groups. Only an
actual open/close transition visits the affected path; +1 appends to an open
path remain incremental, and the webview still fetches bounded pages.
On the 2,431,272-operation workspace this disclosure-only upgrade took 17.02 s
and peaked at 1,165,796 KiB RSS. Prepared native opening plus 500 visible rows
then took 0.440 s with 35,792 KiB retained RSS (39,992 KiB peak); no historical
chain records were replayed and idle sync did no work. These timings exclude
VS Code startup and cold provider-helper reconstruction. Evidence is under the
extension's `.ui-out/offscreen-folding-20260912T193233Z`.

For unsupported schema changes, corruption or deliberate history replacement,
close History, remove only the derived `CHAIN/live-v1` directory and run
`prepare-view` again. Preserve authoritative segments, blobs and import cursors.

## Task-path grouping verification (2026-09-12)

The prepared 2,425,608-operation workspace was migrated from synthetic task
headers to physical causal paths. The one-time migration reused canonical
reducers, stored row pages and graph lanes: 83.56 seconds, peak 3,680,412 KiB
RSS (3.51 GiB). Ordinary opening never performs this migration.

After preparation, native Open plus the first 500 visible rows took 0.245
seconds. Retained RSS after representative windows was 31,676 KiB (30.9 MiB),
peak 35,456 KiB (34.6 MiB). Open decoded zero historical chain records and sent
a 569-byte frame with no full topology. The idle sync read zero chain records
and changed zero blocks. This measures the native service, excluding VS Code
startup and provider warming. New disclosure defaults produced 114,375 visible
rows; explicit/search disclosure can change that count.

The VS Code task-path fixture verifies real item/commit identities, connected
folded capsules, independent item details, fresh content in a folded task,
search of both hidden interiors and the physical summary anchor, lane pitch,
and growing arrival strokes. A second capture checks actual session/subagent
branches in the prepared workspace. Artifacts are in
`extensions/vscode-editchain/.ui-out/task-paths-20260912T163202Z`.

## Limits

- This persists the native history state, **not** the upstream Codex
  `ThreadHistoryBuilder` or source-reader hash state. A restarted/evicted provider
  reducer still replays that source prefix. Already imported records are fenced
  by durable provider cursors; new work is incremental after bootstrap.
- A separate actual-session harness started with no import cursors for its
  isolated rollout path. Its first capture imported a 77 MB source backlog,
  changing 11,727 blocks, and native RSS reached approximately 3.6 GiB. This is
  not the one-operation path. Pre-generating native history does not itself
  pre-import a newly introduced provider source or impose a universal RSS cap.
- Native reads and capture currently share a serialized request loop. A cold
  provider bootstrap can delay subsequent paging, while the saved viewport
  remains visible. The warmed real-session marker appeared within 922 ms of
  the source watcher seeing it; this is a single-run observation.
- Large rollbacks/conflicts or explicit task expansion can touch many dependent
  pages. Transaction memory follows that affected set, not a fixed byte ceiling.
  Allocator retention after a large transaction can exceed steady-state paging.
- Page and row files are append-only; automatic compaction is not implemented.
  Rebuilding a derived checkpoint is the current way to reclaim obsolete pages.
  Legacy `OpenLive` still exports all metadata for compatibility and retains its
  full-topology startup cost. The extension defaults to the paged capability.

## Verification and artifacts

The restart test covers prepared opening, a distant page, one operation after
segment rotation, persisted search, conflict retraction, quarantine after a
second restart and sealed-prefix replacement. Index tests cover page reuse,
unpublished suffixes and corrupt lazy reads. A renderer test covers distant
anchor/selection restoration and acknowledgement ordering; host tests cover
capture startup, disclosure serialization and cancelled-search publication.

`./scripts/lint.sh` reported `RESULT: PASS` for formatting, workspace builds,
Clippy, tests, doctests and dependency policy. The extension harness passed 61
tests. Real VS Code grouping, branching and actual-session observation passed.
The actual-session recording retained 404 row elements, observed 74 simultaneous
growing connections and 274 intermediate stroke samples, with no extra Open.

One narrow `#[expect(clippy::panic, reason = ...)]` in
`crates/editchain-index/src/page.rs` permits a private typed IO fault to unwind
through borrowed lazy-page APIs. The native request boundary converts that fault
to a normal error and invalidates the epoch; unrelated panics resume unwinding.
No quality policy or threshold was weakened.

Local evidence lives under `extensions/vscode-editchain/.ui-out/`:

- `checkpoint-scale/{open,append,restart}/`: stdio timings and RSS samples.
- `checkpoint-scale/{ready,evidence}.json`: actual-session observation.
- `checkpoint-workspace-{prepare.log,capture.json,final/}`: workspace preparation.
- `checkpoint-grouping.mp4`: 26.10 s, 1,987,342 bytes.
- `checkpoint-branching.mp4`: 26.32 s, 7,188,701 bytes.
- `checkpoint-real-session.mp4`: 177.28 s, 22,530,875 bytes, including cold capture.

Reproduce native measurements against an already prepared chain:

```sh
python3 scripts/measure-live-memory.py --paged /absolute/service \
  /absolute/workspace /absolute/chain /absolute/output
```

The driver captures no provider data by default. Its optional `--append-command`
executes an explicit fixture appender before SyncLive; use only an owned copy.
RSS excludes the webview and exporter process. Earlier full-topology comparison
artifacts remain in `/tmp/editchain-memory/{before,final}/`.
