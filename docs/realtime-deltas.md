# Realtime operation deltas

## Implemented path

Live Codex history now uses a resident native workspace. After bootstrap, an
ordinary append does not invoke the import CLI, replay the source prefix, open
a replacement snapshot, or sort and lay out the whole history.

```text
Codex rollout append
  -> retained source cursor and helper reducer
  -> staged blobs, operations and durable checkpoints
  -> canonical additions / conflict retractions
  -> changed logical items and exact ancestor dependencies
  -> revisioned keyed blocks
  -> retained rank index, row cache and DOM animation
```

| Stage | Retained state and update behavior |
| --- | --- |
| Source | `editchain-import/src/source_read/live.rs` retains BLAKE3 state, file identity and a physical cursor. A pass accepts at most 512 records or 4 MiB; an incomplete final line remains pending. A backlog is drained even without a later file-stamp change. |
| Codex | `codex-session-exporter --stream` keeps the provider's `ThreadHistoryBuilder` and EditChain's occurrence projector alive. Each request identifies a source, generation and preceding ordinal. The helper only reduces the supplied new lines. |
| Durability | `realtime/collector.rs` takes the writer lock, checks external appends, stages through `ImportBatch`, and advances checkpoints after durable append. Failures discard speculative reducer state. |
| Canonical evidence | `CanonicalTail` retains framing position and the byte-admission index. Exact duplicates make no semantic edit; conflicting identities retract the previously accepted operation. Segment rotation and incomplete framing are tested against full canonical replay. |
| Logical state | `editchain-project::live::LiveProjection` indexes occurrence proofs, logical item revisions, source coverage and turn removals. Changed witnesses invalidate their actual dependents. |
| Git | The native tracker retains commit objects, refs and unresolved targets. Traversal stops at known commits. Successful Codex commit observations feed retained reconciliation; exact `BasedOn` and `ProducedBy` links feed ancestry. |
| Graph | Hidden operation ancestry is memoized with reverse dependencies. Bootstrap uses Activity's original compact lane planner: Git leftmost, causal session lanes, forks, merges and shared Git-anchor spines. Retained edge-boundary indexes update affected routes and preserve existing node lanes; native paging and WASM use the same implementation. |
| Ordering | Both processes use the shared weighted AVL `RankTree`. Insertion, removal and rank/select avoid shifting every later row coordinate. Disclosure changes only the owning block. |
| Publication | `OpenLive` establishes the baseline once. `SyncLive` returns `LiveUpdate` with a bounded replay journal. Ordinary updates never prepare a render snapshot. |
| Transport | The extension assembles each length-prefixed frame in one buffer, copying each incoming byte once. Large baselines no longer repeatedly copy their entire received prefix. Framing is isolated per native process generation. |
| Renderer | Block edits relocate the bounded cache by stable identity. Keyed DOM reconciliation retains rows, unchanged content cells, and existing SVG segments with their animation clocks. Animation reads positions in one phase and writes styles in another, only near the viewport. Graph width includes the rendered window's nodes and crossing edges. All lanes keep a 14.76px pitch and 4px node radius; dense graphs scroll horizontally with readable content instead of compressing their tracks. |

Live presentation shows each current logical item and its details directly.
The normal offline Activity view retains historical work-group contractions.
These have different grouping semantics; live mode is not a byte-for-byte
reconstruction of the offline Activity row list. All immutable occurrences
remain in the chain. Logical-item state is checked against the full projection
oracle after individual admissions, retractions and restorations.

The missing conversational work grouping is a regression in the default live
experience. The [progressive grouping proposal](realtime-grouping-research.md)
compares alternatives for keeping arrivals exposed while folding older work,
with group membership updated independently of item content and disclosure.

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

`OpenResponse.live` negotiates an epoch, revision, expanded total and block
metadata. Content remains paged. Each `LiveDelta` contains `base_revision`,
`revision`, a new request `snapshot_id`, removed keys, upserted blocks, totals,
lane extent and work counters. Block metadata includes stable ordering,
disclosure spans and exact parent block keys. Parent row coordinates inside an
upsert are block-relative.

`SyncLive` is an extension-host capability. The webview's read-only request
allowlist does not permit provider capture or helper execution. The host waits
for `liveSettled` before advancing its applied cursor. Duplicate revisions are
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
- Initial canonical load and a source's cold bootstrap remain whole-input work.
  The native request loop serializes capture with reads; a slow cold helper can
  delay new paging requests, while already-rendered rows stay visible.
- The repository catalog is captured at open. Reopen after adding/removing a
  repository. Git tracking follows HEAD ancestry and exact recorded link targets.
- Live search retains its index across edits and currently indexes displayed
  block summaries. Full historical search remains available in normal history.
  Out-of-band Codex title-index changes are not yet materialized by the retained
  importer; this adapter follows rollout content.
- Claude live collection, app-server ownership, and activity-only hook signals
  remain the next provider integration. Unrecorded filesystem writes are not
  treated as agent edit evidence.

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
10,000 existing operations. Bootstrap cost and retained memory still scale with
the corpus; ordinary delta work follows changed evidence and its dependencies.

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
