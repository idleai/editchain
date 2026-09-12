# Realtime operation deltas

## Implemented path

Live Codex history uses a persistent, lazily paged native workspace. A prepared
checkpoint opens without replaying canonical history. After provider bootstrap,
an ordinary append does not invoke the import CLI, replay the source prefix,
open a replacement snapshot, or sort and lay out the whole history.

```text
Codex rollout append
  -> retained source cursor and helper reducer
  -> staged blobs, operations and durable checkpoints
  -> canonical additions / conflict retractions
  -> changed logical items and exact ancestor dependencies
  -> revisioned keyed blocks
  -> additive durable checkpoint, native visible rank/select
  -> bounded viewport and keyed DOM animation
```

| Stage | Retained state and update behavior |
| --- | --- |
| Source | `editchain-import/src/source_read/live.rs` retains BLAKE3 state, file identity and a physical cursor. A pass accepts at most 512 records or 4 MiB; an incomplete final line remains pending. A backlog is drained even without a later file-stamp change. |
| Codex | `codex-session-exporter --stream` keeps the provider's `ThreadHistoryBuilder` and EditChain's occurrence projector alive. Each request identifies a source, generation and preceding ordinal. The helper only reduces the supplied new lines. |
| Durability | `realtime/collector.rs` takes the writer lock, checks external appends, stages through `ImportBatch`, and advances checkpoints after durable append. Failures discard speculative reducer state. |
| Canonical evidence | `IndexedTail` retains framing position and record locations; exact evidence stays in the durable segments. Exact duplicates make no semantic edit; conflicting identities retract the previously accepted operation. Segment rotation and incomplete framing are tested against full canonical replay. |
| Logical state | `editchain-project::live::LiveProjection` indexes occurrence proofs, logical item revisions, source coverage and turn removals. Changed witnesses invalidate their actual dependents. Immutable operation allocations are shared with the live reader and item inputs; single-user reverse dependencies stay inline. |
| Provider relationships | Indexed raw-prefix counts and lifecycle evidence feed the same endpoint resolver as Activity. Affected threads publish exact spawn/completion edge changes independently of content. A resolved spawn replaces the child's inherited Git base; completion adds the child terminal alongside the parent's continuation. |
| Git | The native tracker retains commit objects, refs and unresolved targets. Traversal stops at known commits. Successful Codex commit observations feed retained reconciliation; exact `BasedOn` and `ProducedBy` links feed ancestry. |
| Graph | Hidden operation ancestry is memoized with reverse dependencies. Bootstrap uses Activity's original compact lane planner: Git leftmost, causal session lanes, forks, merges and shared Git-anchor spines. Retained edge-boundary indexes update affected routes and preserve existing node lanes; native paging and WASM use the same implementation. |
| Presentation | Row payloads live in checksummed durable pages referenced by the checkpoint. Only requested windows and outgoing delta blocks receive graph geometry. Blob access is shared per workspace and refreshed after capture. |
| Ordering | Monotone causal clocks keep every present parent below its child, including tied or skewed provider timestamps. Only a new constraint and affected descendants move; physical row timestamps stay unchanged. Native weighted AVL pages own expanded and visible rank/select. Local disclosure changes its block; task folding visits its section. The paged renderer keeps an identity mapping over visible coordinates. |
| Publication | `OpenLivePaged` returns an epoch, revision and visible total without global metadata. `SyncLive` returns a bounded journal; affected pages are checkpointed before publication. Legacy `OpenLive` remains supported. |
| Transport | Paged Open sends a small control frame; viewport reads and affected delta blocks carry content. Legacy baseline serialization borrows metadata. Framing is isolated per native process generation. |
| Renderer | Native deltas locate stable anchors and fetch their replacement viewport while the old keyed DOM remains visible. Keyed DOM reconciliation retains rows, unchanged content cells, and existing SVG segments with their animation clocks. Animation reads positions in one phase and writes styles in another, only near the viewport. Graph width includes the rendered window's nodes and crossing edges. All lanes keep a 14.76px pitch and 4px node radius; dense graphs scroll horizontally with readable content instead of compressing their tracks. |

Live presentation shows each current logical item and its details directly.
The normal offline Activity view retains historical work-group contractions.
These have different grouping semantics; live mode is not a byte-for-byte
reconstruction of the offline Activity row list. All immutable occurrences
remain in the chain. Logical-item state is checked against the full projection
oracle after individual admissions, retractions and restorations.

Native thread/turn identities and rollback boundaries own task membership.
Only exact non-branching causal paths fold. Their summaries annotate existing
physical items; grouping adds no rows or synthetic graph nodes. Concurrent
sessions can interleave without creating repeated task headers. Native task
paths start folded offscreen. On the first viewport at the head, the latest
task's visible paths open by default. `ViewportLive` reports at most 256 actual
visible row identities and a viewport capacity, separately from page prefetch.
New/revised members in that viewport open their entire path, with the ribbon
and content sharing the same expanded state. Automatic paths stay open while
any member remains visible, even after completion or the ribbon leaving view.
Explicit opens and closes remain durable. Viewport membership checks inspect
only reported rows and the automatic group set. A fold transition remeasures
that path; subsequent +1 appends still visit only changed boundaries. A burst
limits candidate paths by viewport capacity, but an opened path is fully open
and content remains virtualized.
The host coalesces scroll reports behind its existing live publication barrier
and drops reports from obsolete snapshots. After a path folds offscreen,
scrolling back alone does not reopen it. The legacy full-topology client
retains its earlier policy.
Item details and task paths have separate controls. See the
[grouping contract](realtime-grouping-research.md) for path boundaries and
checkpoint migration.

Chronological graph connections stop at an item's first appearance. A later
tool result revises that same item but does not become another appearance:
`tool start -> nested command -> tool result -> next item` displays as
`tool -> command -> next item`. Exact references use a separate owner lookup,
so a `ProducedBy` link still targets its tool, including an older result from
the current incarnation. Removed incarnations cannot alias recreated items.
Both lookups invalidate only their indexed dependents. This topology repair
does not introduce any new grouping or fold boundaries.

Lane identities also retain their display columns. Shared Git routing spines
discovered after bootstrap receive additional columns without inserting a
column before existing activity. Bootstrap retains the established compact
Activity plan; as before, discovering the very first Git node in a history
that opened without Git reserves the new leftmost Git column.

## Protocol and recovery

`OpenResponse.live.paged` negotiates native paging: an epoch, revision, visible
total and empty block list. In legacy mode its total remains expanded and its
block metadata is complete. Content remains paged. Each `LiveDelta` contains `base_revision`,
`revision`, a new request `snapshot_id`, removed keys, upserted blocks, totals,
lane extent and work counters. Block metadata includes stable ordering,
disclosure spans and exact parent block keys. Parent row coordinates inside an
upsert are block-relative. Native pages use visible coordinates, with
`native_expanded` carrying disclosure; only offset-zero pages carry expansion
metadata. `LiveDelta.visible_total` supplies the new visible rank space.

`SyncLive` is an extension-host capability. The webview's read-only request
allowlist does not permit provider capture or helper execution. The host waits
for the first saved viewport before starting capture, and for `liveSettled`
before advancing each applied cursor. Task toggles and search-induced disclosure
share the publication queue; page/anchor reads remain available to finish it. Duplicate revisions are
acknowledged without reanimation; old window responses cannot overwrite newer
state. The journal retains up to 32 revisions or 16 MiB, retaining at least one
revision. Missing replay, renderer recreation, rejected topology or invalidated
native state triggers an explicit new baseline.

The extension polls every 250 ms after the previous pass. A retained warm pass
uses a shared helper process and native workspace; the source-reader and helper
LRUs can require an explicit cold bootstrap when an older source becomes active.
Opening History starts live collection by default. The **EditChain History**
output channel reports source paths, retry errors and per-delta work counters.
Pause prevents further capture; an already-running durable transaction is
allowed to settle. Resume starts collection again and reveals diagnostics.
Setting `editchain-history.live.enabled` to `false` opts into static history.

## Integrity and current boundaries

- Active Codex rollouts and canonical segments have an append contract. Inode
  replacement, truncation and detectable same-size rewriting require strict
  recapture. Arbitrary rewrite-and-append cannot be ruled out without rereading
  the old prefix. Sealed segments are immutable for a live epoch; reopen after
  deliberate historical replacement.
- Canonical history is loaded only during first preparation; prepared opens
  adopt its saved frontier and lazy indexes. A source's cold helper bootstrap
  still replays that source prefix. Native capture shares the read request loop; a slow cold helper can
  delay new paging requests, while already-rendered rows stay visible.
- The repository catalog is captured at open. Reopen after adding/removing a
  repository. Git tracking follows HEAD ancestry and exact recorded link targets.
- Live search is persisted during preparation and changes with affected blocks.
  It indexes displayed summaries. Full historical search remains available in
  normal history.
  Out-of-band Codex title-index changes are not yet materialized by the retained
  importer; this adapter follows rollout content.
- Claude live collection, app-server ownership, and activity-only hook signals
  remain the next provider integration. Unrecorded filesystem writes are not
  treated as agent edit evidence.

Checkpoint storage, prepared-open and +1 measurements, recovery and remaining
provider/transaction memory limits are described in [native-memory.md](native-memory.md).

## Verification and reproduction

Run the repository gate with `./scripts/lint.sh`. It checks formatting, every
workspace target/feature, Clippy, tests, documentation tests and dependency
policy. The isolated exporter has its own `cargo test --locked` run.

The added checks cover canonical-tail framing and replay, retained source
backlogs and partial lines, provider batch partitioning and duplicate replies,
logical state versus full replay, AVL coordinates versus a sorted oracle,
revision replay and gaps, search after append, exact ancestry, viewport anchors,
selection, DOM reuse and reduced motion.

Graph regression tests compare bootstrap node lanes, straight segments, bends
and muted geometry against the established Activity layout for forks, merges,
concurrent sessions and shared Git anchors. Warm updates cover lane retention,
new branches, moved merge boundaries, overlapping anchors and retractions.
The VS Code fixture also requires a straight session track beside Git lane zero.
`wdio.branching.conf.ts` exercises four wrapper/command/result cycles through
the real helper and live service, one record per update. It requires stable
item identities, mounted rows and lanes, no duplicate rows on completion,
and no false forks or repeated session endpoint chips within that path.
`wdio.subagents.conf.ts` imports three real-shaped child rollouts incrementally,
including equal-time startup records and a child arriving before the parent
continues. It checks exact spawn roots, all three completion edges plus the
main continuation, row-boundary continuity, causal row ordering and fixed lanes.
Native tests compare relationship deltas against the earlier Activity resolver
across arrival batches, missing records, conflicting spawns and proof repair.
The September 11 branching repair was also checked by reopening the existing
chain in an independent read-only service: the same 244 activity identities
had 50 in-sample fork points before the repair and zero afterward. The newest
236 root rows occupied one lane with zero in-sample forks. The check did not
restart the developer's service or rewrite stored ancestry.
Window framing preserves ordinary lane spacing and includes passing edges even
when their endpoints are outside the window. Large-history observation waits
for the initial delta to finish rendering before capturing nodes for reuse.

New visible connections grow from the parent attachment toward the new endpoint.
After the 360 ms row movement, connected SVG segments draw in sequence across
row boundaries over 520 ms. Independent branches run in parallel; nodes appear
as the connection reaches them. Unchanged segments stay mounted, including when
row text is revised during an animation. Reduced motion renders the final
geometry immediately. The browser checks +1 forks and merges, intermediate
stroke lengths, preserved animation objects during revisions, duplicate replay,
and lane 190 at both wide and narrow viewport sizes.

The deterministic VS Code recording uses the shipped CSS/WASM and a fixture
protocol host to show one-operation forks and merges at normal animation speed:

```sh
cd extensions/vscode-editchain
FPS=60 DISPLAY_NUM=115 ./scripts/ui-vscode-record.sh \
  .ui-out/graph-growth-fork-merge.mp4 ./test/vscode/wdio.growth.conf.ts
```

The manual scaling example constructs one million connected message operations
and visible blocks, opens them once, and appends exactly one operation:

```sh
cargo run --release -p editchain-node --example live-scale -- 1000000
```

A release run with the restored Activity graph measured a 103.4 s bootstrap
and a 43 ms +1 request:
50 chain bytes read, one record decoded, one presentation input, one changed
block, and a 1,553-byte response. This is native request latency, not an
end-to-end paint measurement. Work counters are also asserted with 1 and
10,000 existing operations. That historical run used the full in-memory baseline. Preparation still scales
with the corpus; current prepared-open and +1 measurements are in
[native-memory.md](native-memory.md).

The real VS Code fixture opens the normal History command without a live opt-in
and checks edits before turn completion, a stationary 600-line backlog, Git
commit arrival and its producing command edge, and pause/resume without
duplicate storage:

```sh
cd extensions/vscode-editchain
./scripts/ui-vscode-record.sh .ui-out/live-fixture.mp4 ./test/vscode/wdio.live.conf.ts
```

The actual-session harness reads this agent's actively appended rollout through
an isolated hard link and imports into a separate chain. It never writes fake
provider events. After `ready.json` appears, emit its unique marker in an
assistant progress message. The harness requires that message to reach a mounted
history row, observes animations after readiness, checks DOM reuse and checks
that no extra Open occurred. A separate source-tail probe timestamps when the
message becomes readable, distinguishing provider flush delay from UI delay.
The hard link is removed after the run; evidence and the recording remain local.

```sh
EDITCHAIN_OBSERVE_SOURCE=/absolute/path/to/active/rollout.jsonl \
DISPLAY_NUM=110 ./scripts/ui-vscode-record.sh \
  .ui-out/live-codex-retained-dom.mp4 ./test/vscode/wdio.observe.conf.ts
```

The September 11, 2026 full-history run observed this actual Codex session advancing
from 327,166 to 327,170 expanded rows across four new updates. The assistant marker
reached the renderer 375 ms after the independent source watcher saw it; the
mounted-row check completed within 646 ms. Those are single-run observations,
with a 50 ms source probe and 100 ms visibility poll, not a paint-time guarantee.
The update retained 419 existing row elements, observed at most 31 simultaneous
row animations and 14 growing SVG segments, and issued zero additional Open
requests. The probe observed intermediate stroke lengths 43 times. The marker
changed one block with no provider bootstrap. Initial full-history loading took
about 3 minutes 20 seconds; the incremental timings apply after that baseline.
The current window used ten lane centers at fixed 14.76px spacing, despite a
historical maximum of lane 190 elsewhere in the chain.

Local recording and evidence are under `extensions/vscode-editchain/.ui-out/`:

- `graph-growth-live-session.mp4`: the full 220.3-second actual-session recording.
- `graph-growth-live-excerpt.mp4`: the final actual-session updates without startup.
- `graph-growth-fork-merge.mp4`: deterministic +1 fork and merge in real VS Code.
- `graph-growth-demo.mp4`: the fork and merge at 60 fps, without test-runner startup/shutdown.
- `graph-growth-codex-git.mp4`: active-turn edits, Git and pause/resume in real VS Code.
- `graph-review-IEZEpi/evidence.json`: source timestamp, received deltas and DOM probe.
- `graph-review-IEZEpi/growth-summary.json` and `observed.png`: metrics and the visible result.

Validation passed: `./scripts/lint.sh` reported `RESULT: PASS`; the isolated
exporter passed 37 tests; the extension harness passed all 60 tests; and the
provider, fork/merge and actual-session VS Code suites passed. WASM-target Clippy also
passed for the browser-specific rendering code.
