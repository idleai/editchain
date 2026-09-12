# VS Code human-work capture

The production extension records human work in the existing EditChain store and
provides **EditChain: Show Human Work Coverage**. No subagents were used for this
implementation or its tests. The [research](vscode-editor-capture-plan.md) and
[API experiments](vscode-editor-capture-results.md) explain the stable API choices.

## Event contract

`editchain_protocol::editor` defines schema version 1. Every event has a random
recorder session, a strictly increasing sequence, observer wall time, and typed
payload. Each document carries URI, workspace-relative path when available,
incarnation identity, and exact buffer version. Opening an editor is independent
of loading a document. Split tabs have separate identities.

| Stored event | Meaning |
| --- | --- |
| `tracking_started` | Recorder incarnation, VS Code version, dwell policy. |
| `workspace_context` | Observed workspace location and worktree-qualified Git HEADs; this is not a working-tree snapshot. |
| `tracking_stopped` | Orderly shutdown. |
| `tracking_gap` | Skipped buffer, missing baseline, or capacity pause. |
| `document_snapshot` | Exact initial/recovered text, including unsaved text. |
| `document_changed` | Before/after revisions and raw replacements in emitted order. |
| `human_edit` | Reference to a change with keyboard-selection, undo, or redo evidence. |
| `document_saved` | Saved buffer revision. |
| `document_renamed` | Explicit file/directory rename within the workspace. |
| `editor_opened` / `editor_closed` | Text tab lifecycle, including preview tabs. |
| `editor_activated` | Active tracked document, or no tracked active editor. |
| `selection_changed` | Cursor/selection ranges and keyboard-kind indicator. |
| `visible_ranges_changed` | Disjoint viewport ranges; the API does not identify scroll cause. |
| `code_exposure` | Revision-bound ranges, start time, and monotonic duration. |

Window focus is **not** an event or a stored field. An in-memory focus guard
pauses exposure. Intervals also end on document changes, viewport changes, active
editor changes, captured Git-context changes, checkpoints, and shutdown. A
heartbeat bounds each interval.
Scroll, reveal, folding, and layout changes all use the viewport event. Hidden
tabs and background windows earn no exposure. Horizontal clipping, terminal or
sidebar keyboard focus, gaze, and comprehension are not observable guarantees.

The intentionally cooperative user assumption applies to edit attribution:
keyboard selection correlated with changes within 250 ms marks those changes
as human work; focused active-editor undo/redo also qualifies. Typing, deletion,
and paste work through stable API events, without overriding VS Code commands.
Programmatic changes remain observations unless they also produce the same
human indicators. This is a heuristic, not verified authorship.

## Durable integration

The recorder activates on startup in trusted local workspace folders, with its
own service client and lifetime. It does not require a History panel or its
renderer handshake. Multi-root folders have separate recorders and chain paths;
untitled documents are captured under the first root with no file provenance.

`RecordEditorEvents` is additive to protocol version 2 and independent of history
snapshot negotiation. The service validates UTF-16 replay against the exact
after-text before writing. JavaScript offsets never become Rust byte offsets.
Stable reason metadata is retained; dirty-only events do not manufacture edits.

The outbox writes atomic local batches, fsyncs them, then sends bounded requests.
Persistence continues while an earlier service request waits. Offline retries
back off to 30 seconds. New records keep their original identities on replay.
The service acquires the existing chain writer lock, refreshes its canonical
tail, rejects conflicting identities and sequence gaps, writes content-addressed
blobs, and acknowledges only after durable segment append. Retrying after a
lost acknowledgement is a duplicate, not another human action. Other windows
and AI importers use the same writer lock.

Events use ordinary `ImportOp` envelopes with a versioned `vscode.editor` source.
Human indicators additionally carry `HUMAN | INFERRED`; raw observations carry
the human-work source tag without claiming that each mutation was authored by
the human. No permanent core operation-code allocation was needed. The query
hydrates editor events sequentially and retains one revision per document
incarnation rather than every historical buffer in memory.

Limits: 128 events and approximately 4 MiB per outbound batch; 32 MiB queued
memory and 64 MiB pending disk data per recorder; 256 KiB default buffer limit.
Capacity failures record a final gap and pause recording until resumed. Code
snapshots are local retained evidence; chain disk usage grows with work. Atomic
outbox publication runs every second and during shutdown. An abrupt failure can
lose its unpersisted tail; absence of `tracking_stopped` does not distinguish a
live recorder from an interrupted one. Remnants before atomic publication are
not silently presented as complete events.

## Human work in History

The service derives versioned `vscode.work` Imports, human `FileOp`s, path notes,
and `BasedOn` Git links alongside unchanged raw observations. Derived identities
are deterministic per recorder session, source sequence, and operation role.
Raw observations have explicit supporting-evidence annotations, so Activity
shows human work instead of transport envelopes. For new captures, the annotation
is appended before the source record; even a live reader stopping at that record
boundary cannot display a temporary transport row. Trace retains the source facts.

Each VS Code recorder has its own connected work series. Edits and read indicators
continue the previous fragment. Work episodes start after more than 30 seconds
without a work fragment, at observed context changes, renames, stops, and capture
gaps. Live History uses native task disclosure to fold these episodes while
preserving their physical parent path. In static History, human fragments stay
as graph rows with their original ancestry and expandable file details; they do
not use the older nested-detail contraction that omits internal graph edges.

Every edit references its exact before/after buffer contents in the blob store,
including unsaved text and intermediate agent or external edits. Content hashes
and revision-occurrence IDs are separate: undo from X to Y to X does not turn the
last occurrence into the first one. The file-diff source is `human`; clicking a
file opens those retained sides in VS Code's native diff editor. Human file
operations are excluded from the AI-origin denominator. If a live update races
with a file click, the host waits for publication and retries once with the same
complete edit identity. The service revalidates that identity in the settled
revision; it never substitutes the file's current contents. This retry cannot
cross a live epoch or a replaced panel. Static snapshot checks remain strict.

A separate observer calls `GetEditorContext` at startup and every 15 seconds.
Successful context changes enter the recorder's ordered stream with the observed
workspace path, repository identity, worktree root, HEAD, and observation time.
The first work fragment under a changed context links to that recorded commit;
subsequent work continues its own series. A changed HEAD adds a new baseline
without removing the predecessor. Edits retain the context observed when their
text changed, even if keyboard confirmation arrives after a new context
observation. No current-HEAD or current-workspace-path substitution is made
during replay. An unborn, missing, or unavailable context stays unanchored.
Failed polls clear the old context until a successful poll.
Up to 64 worktrees are supported per observation; exceeding the limit reports an
observation failure rather than inventing a partial context.

The Git context is a sampled observation, not an atomic input snapshot. Changes
between polls can be missed, and capture may begin before the first poll returns.
The exact buffer before-state remains authoritative for the edit. Neither an
agent tool boundary nor a Git anchor proves that the whole working directory,
index, dependencies, processes, or external environment were isolated or observed.
This implementation does not build a global workspace-state DAG or guarantee
replay of every external mutation. Historical context can outlive the worktree
or commit objects needed to render its Git node.

Continuous exposure of at least 250 ms appears as a graph activity. Shorter
intervals, including those between keystrokes, stay in raw evidence and still
contribute to exposure coverage; they do not create a dot per interval. The
recorded reading threshold (2 seconds by default) separately determines reading
indicators. Episode folding does not assert that a person completed a task or
understood a file.

When a recorder next submits a batch, the service backfills older canonical raw
captures and appends only missing derivations. It repairs replayed source payloads
before deriving, and acknowledges only after both source and derived evidence
are durable. Older captures without a recorded Git/workspace context remain
unknown. Repeated batches, service restarts, and relocation of retained evidence
do not rewrite historical work. Git rendering at a relocated workspace still
needs the original repository identity to be available.

## Coverage rules

`GetHumanWork` reads the canonical AI and editor evidence and returns a report.
The editor command shows it as a native read-only Markdown document.

1. Extract dated, materializable AI changes from the existing file-change index.
   Full before/after snapshots and recorded unified diff hunks are supported.
   Partial tool reconstructions, binary evidence, missing payloads, and unknown
   timestamps are counted as unsupported AI changes.
2. Assign origin identities only to newly added/changed nonblank AI lines.
   Context lines in a diff do not become AI-generated lines.
3. Align unchanged lines from complete snapshots; match partial hunks only as
   unique contiguous exact blocks in the same path. A bounded alignment that
   cannot resolve a replacement contributes a gap. Unmatched code is unknown.
4. Apply editor changes in recorder order, requiring matching before text.
   Unchanged lines retain origins. Human replacement descendants retain the
   touched AI origins; deleted origins remain in historical edited counts.
   Unattributed replacement text does not inherit removed AI origins.
5. Join exposure to its document incarnation and exact revision. Count distinct
   AI origins at any duration as exposed, and at the recorded dwell threshold
   as having a reading indicator. Repeated reads do not inflate line coverage.
   AI source time must precede the observation; importing that evidence later
   still allows retrospective measurement.
6. Report current **saved** file line counts, read, edited, overlap, and exposure.
   Historical totals separately retain unsaved work and deleted lines. Current
   counts use matched AI-origin lines as their denominator; they are not a claim
   that all repository code has known provenance. Zero known AI lines means
   “not yet measurable.”

The metric is line based. It neither proves comprehension nor measures the
fraction of characters read within a line. Explicit renames maintain path
continuity. Untitled Save As, external moves, detached windows, notebook/diff
editors, and remote/web extension hosts do not yet have complete provenance
continuity. Full snapshots preserve code for later re-analysis as those cases
gain support.

## Verification

Build the native service, synthetic fixture generator, and extension:

```sh
cargo build -p editchain-node --bins --example editor_work_fixture --locked
cd extensions/vscode-editchain
npm run compile
npm run test:capture:types
npm run test:harness
npm run ui:vscode:work
```

The production host suite runs on 1.137.0 by default. To repeat on 1.85.0, reuse
the ChromeDriver installed by `scripts/test-capture-baseline.sh`:

```sh
EDITCHAIN_CAPTURE_VSCODE=1.85.0 \
CHROMEDRIVER_PATH="$PWD/.wdio-vscode-service/chromedriver-114.0.5735.90/chromedriver" \
npm run ui:vscode:work
```

The suite creates a real Git repository and a 200-line AI fixture, dwells, types
through WebDriver, checks unsaved and saved coverage, navigates briefly to a
distant viewport, and pauses/resumes recording. The original four capture
scenarios passed on both releases. A fifth scenario exercises the live graph,
episode folding, updates while the panel is open, and the native human diff. The
complete five-scenario suite passed on 1.137.0 after graph integration. The
1.137.0 sample measured 39 reading-indicated lines, one edited line, and 71
exposed lines after the distant jump, with zero capture gaps. Exact viewport
counts vary with layout. Synthetic chains and reports are in ignored
`extensions/vscode-editchain/trace/work-<version>/` directories.

Focused service tests cover canonical retries, restart recovery, conflict/gap
rejection, UTF-16 validation, disjoint exposures, unsaved work, deletion,
automatic changes, late AI imports, and partial-hunk provenance. Host tests
cover focus guards without focus events, document incarnations, attribution,
offline outbox replay, exact acknowledgements, independent disk persistence,
and capacity pauses. Run `./scripts/lint.sh` at the repository root for the
canonical full Rust verification.

The integration also has service regressions for interleaved agent/human
before-and-after state, two human windows and two agents sharing Git, external
HEAD movement, static and paged-live ancestry, byte-identical replay, legacy
backfill after payload repair, relocation, and stale-diff rejection followed by
exact revalidation. Browser-host tests cover context poll failure/recovery,
disposal, overlapping requests, exposure boundaries, and serialized diff retries
that cannot cross live epochs or replaced panels.

Validation on 2026-09-12: 13 focused editor service tests passed; all 73 extension
harness tests passed; extension compilation and capture-test type checks passed;
the production WASM renderer built; all five real VS Code scenarios passed;
`./scripts/lint.sh` exited 0 with **`RESULT: PASS`**. The lint command completed
format, check, clippy, workspace tests, doc tests, and dependency checks without
policy changes or new suppressions.

Integration validation logs and actual VS Code screenshots are retained under
`outputs/vscode-capture/human-integration/`. Build and test the current checkout
with the commands above; the native service and extension renderer must come
from the same build.
