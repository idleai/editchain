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
of loading a document. Since 0.1.7, split tabs share one file lifecycle identity.

| Stored event | Meaning |
| --- | --- |
| `tracking_started` | Recorder incarnation, VS Code version, loaded `extension_version` (since 0.1.6), dwell policy, activity derivation contract. |
| `workspace_context` | Observed workspace location and worktree-qualified Git HEADs; this is not a working-tree snapshot. |
| `tracking_stopped` | Orderly shutdown. |
| `tracking_gap` | Skipped buffer, missing baseline, or capacity pause. |
| `document_snapshot` | Exact initial/recovered text, including unsaved text. |
| `document_changed` | Before/after revisions and raw replacements in emitted order. |
| `human_edit` | Reference to a change with explicit editor-input, keyboard-selection, undo, or redo evidence. |
| `human_edit_batch` | Ordered change references and input signals; optional `group` identifies the first change of a live editing episode. |
| `document_saved` | Saved buffer revision. |
| `document_renamed` | Explicit file/directory rename within the workspace. |
| `editor_opened` / `editor_closed` | First-tab / last-tab file lifecycle, identity, URI, and relative path. Startup opens carry `restored: true`. |
| `editor_activated` | Active tracked document, or no tracked active editor. |
| `code_read` | One qualified interval: exact revision, disjoint visible ranges, start time, and monotonic duration at qualification. Optional `group` attaches it to an ongoing edit. |

Window focus is **not** an event or a stored field. An in-memory focus guard
pauses exposure timing. Cursor/selection and viewport observations stay local;
`selection_changed`, `visible_ranges_changed`, and `code_exposure` are no longer
posted. The service still accepts and replays these legacy events unchanged.

A one-shot timer emits `code_read` when the active visible editor reaches the
recorded dwell threshold (2 seconds by default). An unchanged view emits only
one read, even over ten minutes. The last view of each loaded document retains
its read receipt across focus loss, tab reactivation (including a replacement
`TextEditor` object), and Git context changes. A viewport or buffer revision
change rearms reading. There is no read heartbeat. Checkpoints may
flush a qualified interval but never split it or duplicate its read. Recorded
duration is evidence at qualification, not total reading time; delayed callbacks
are capped at 60 seconds. Short intervals are discarded rather than accumulated.

Unqualified intervals end on changes to the viewed buffer, viewport, active
editor, focus, captured Git context, or shutdown; separate short visits are never
added together. Ending an interval does not clear an already-qualified receipt.
Scroll, reveal, folding, and layout changes all use local viewport observations.
Duplicate viewport/activation notifications and edits to background files do not
reset the timer. Hidden tabs and background windows
earn no reads. Horizontal clipping, terminal or sidebar keyboard focus, gaze,
and comprehension are not observable guarantees.

Opening a file's first text tab stores `editor_opened`; loading its document establishes an
exact baseline if needed. Opening a background tab alone does not start reading.
Activation of a visible editor starts its local timer. Closing a tab stores
`editor_closed` with the same lifecycle identity only when the final tab closes;
removing the viewed editor ends its
interval and flushes a qualified read if its timer was delayed. A close before
the threshold produces no read. Closing one split does not close the shared
document or end another split's active interval. Startup records existing tabs
with `restored: true` as raw inventory, without new open activities. Genuine
opens and final closes produce graph rows in the same connected human series
as reads and edits. These lifecycle rows never contribute read or
edit coverage and do not claim a buffer modification.
Orderly recorder shutdown flushes a pending qualified read and records
`tracking_stopped`; it does not manufacture tab closes. Abrupt process exits can
leave lifecycle intervals incomplete.

Version 0.1.5 records the observable basis for attribution, under the cooperative
assumption of intentional human work. Both paths require the focused active
document and retain exact changes before saving:

- With the optional `textDocumentChangeReason` API enabled, `cursor` origins
  with typing, paste, cut, composition, or editor-command kinds emit
  an input receipt with signal `editor_input`. Backspace, Delete, Tab,
  and selected-text deletion qualify even without a keyboard selection event.
  `document_changed.origin` retains bounded source, kind, detailed source,
  mechanism name, and provider extension fields when reported. Known non-input
  origins, missing reasons, and unfamiliar kinds stay unattributed regardless
  of nearby keyboard activity. Ordinary undo/redo has `applyEdits` origin and
  retains its separate signal.
- On stable APIs, the first selection update must be keyboard-caused, belong
  to the same editor and exact buffer revision, arrive within 250 ms, and end
  at the replacement's resulting UTF-16 caret positions. It consumes only that
  candidate, including when rejected. A newer mutation replaces the candidate;
  save, activation, focus, and document-close boundaries clear it. This path
  leaves ambiguous deletions of pre-existing text and other unsupported actions
  unattributed. A deletion wholly inside text already introduced by confirmed
  input in the active edit can instead emit `typing_correction`; known non-input
  origins never qualify. This cooperative inference cannot distinguish an unknown
  automatic retraction of that same newly typed text. Focused active-editor
  undo/redo also qualifies.

Saving records `document_saved` without inventing or duplicating an edit.
Formatting on save, `WorkspaceEdit`, `TextEditor.edit`, disk reloads, and provider
completion acceptance are observations with no automatic human attribution.
The coverage report includes `unattributed_changes`, the count of observed
mutations without a human indicator. This includes both automated changes and
uncertain changes; it is not an additional AI-origin count.

Since 0.1.7, the first confirmed input publishes immediately. Further receipts
publish every **100 ms** or **128 changes**, updating one stable live row. Save,
active-editor/focus/lifecycle/context boundaries, a new input after **30000 ms
idle**, or an automatic/unconfirmed mutation of the edited file finishes its
group. Undo and redo are separate immediate edits. Reads during an edit retain
their own coverage evidence under that row; neither reads nor mutations/saves of
background files end the group. Disposing a background or internal VS Code
document does not end it either. A coverage query and orderly shutdown flush it.
Raw `document_changed` evidence is queued immediately; each grouped receipt has
an exact cumulative diff from the first before-buffer to its latest after-buffer.
This is independent of transport batch sizes. The 0.1.6 idle/continuous publication
delays no longer apply.

The native derivation requires every intervening change to that document, exact document and
revision continuity, and unchanged recorded context/boundaries. Discontinuous
or missing burst references produce a visible gap, never a composite human
diff. Coverage accepts receipts only from successfully derived edits and still
replays each constituent change, so grouping cannot swallow an agent revision
or erase work that was edited and then reverted within a burst. Existing raw
and derived records retain their original bytes and per-keystroke rows.

An abrupt process exit can lose the pending burst's attribution receipts (up to
the bounds above); any already persisted raw changes remain unattributed.
Ordinary reload/shutdown flushes both the burst and the durable outbox.

Stable VS Code reports the same unspecified selection kind for Backspace and
some programmatic edits; widening that filter alone would misattribute work.
The proposed API provides mechanism evidence, not verified authorship: an
extension invoking the built-in `type` command can still produce the same
origin as physical input. See the [VS Code source mapping](https://github.com/microsoft/vscode/blob/1.136.2/src/vs/workbench/api/common/extHostTypes.ts#L690),
[edit-origin definitions](https://github.com/microsoft/vscode/blob/1.136.2/src/vs/editor/common/textModelEditSource.ts),
and [prior runtime counterexample](vscode-editor-capture-results.md#attribution).

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
back off to 30 seconds; a busy writer retries after 50 ms. New records keep their
original identities on replay. A durable incremental `editor-v1` checkpoint
retains admission and normalization state across recorder processes. Its first
full scan runs before acquiring the chain append lock; contention cannot discard
that completed scan. Each request releases checkpoint ownership for other windows.
The service acquires the existing chain writer lock, refreshes its indexed
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
outbox publication is scheduled 25 ms after enqueue, with a one-second recovery
poll and an orderly-shutdown flush. Acknowledgement wakes live History before
provider backlog processing. An abrupt failure can
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

Tracking is enabled by default. Version 0.1.2 creates an unsigned local identity
GUID once in the extension's profile storage (`globalStorageUri`, file
`unsigned-human-identity.json`). Concurrent first activations publish one complete
identity file atomically; an invalid file is reported rather than silently
rotating the identity. The GUID identifies local attribution, without a login,
signature, or claim that a real-world identity has been verified.

Every activation/reload still creates a fresh recorder session UUID and resets
its sequence to one. Source observations and semantic work retain both that
incarnation and `{kind: "unsigned", guid, stream}`. The opaque stream binds the
workspace URI and resolved chain location; the same person in another worktree
has a separate path. The GUID supplies the stable actor, while GUID plus stream
supplies the graph group. No Git name, email, or machine identifier is used.

Within a stream, the service records each new source's predecessor under the
chain writer lock, across reloads, pauses, and interleaved recorder windows.
This is durable admission order, not a claim that buffer changes happened in
that order or used isolated workspace snapshots. Recorder-local sequence checks
remain independent. Normalization follows those captured parents, so retries,
backfill after a crash, and UUID/wall-clock ordering cannot rearrange existing
work. New semantic work continues the previous human fragment across sessions;
different unsigned identities and workspace bindings remain separate.

Work episodes start after more than 30 seconds
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
without removing the predecessor. Identity-bearing sessions avoid adding the
same Git link again on reload; a temporarily unknown context remains unknown on
the record without discarding the stream's last established Git link. Edits retain the context observed when their
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

New capture contributes graph activity for editor opens/closes, qualified
reads, confirmed human edits, and gaps. Legacy brief-exposure work is retained
as Trace evidence and omitted from the Activity graph. Legacy exposure that
qualified as a read remains visible as a reading indicator.

Version 0.1.1 recorders declare `activity_schema: 2` at session start. Version
0.1.2 declares `activity_schema: 3` and requires the persistent unsigned identity
on every event. Both contracts include tab lifecycle in the work series and episode boundaries.
Older sessions keep their original normalization, including their immutable
parents and episode IDs; their raw open/close records remain evidence rather
than being inserted retroactively into existing chains. Existing anonymous
sessions are not reassigned to a newly created GUID. Restarting capture creates
a new session and observes currently open tabs under the new contract, while
identity-bearing sessions continue the same human branch.
Episode folding does not assert that a person completed a task or understood a
file.

Static snapshots use projection revision 58. Retained live checkpoint version 5
upgrades version-4 caches when opened: cached brief-exposure rows are removed,
ancestry and task disclosure are reconnected, and other rows are retained.
This does not rewrite canonical evidence or replay the complete source history.
Earlier graph/disclosure checkpoint migrations still use explicit prepare-view.

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
5. Join reads to their document incarnation and exact revision. Count distinct
   AI origins at the recorded dwell threshold as having a reading indicator.
   The service checks the recorded policy rather than trusting the event name.
   Repeated reads do not inflate line coverage.
   AI source time must precede the observation; importing that evidence later
   still allows retrospective measurement.
6. Report current **saved** file line counts, read, edited, and overlap.
   Historical totals separately retain unsaved work and deleted lines. Current
   counts use matched AI-origin lines as their denominator; they are not a claim
   that all repository code has known provenance. Zero known AI lines means
   “not yet measurable.”

The report API retains `exposed_lines` and `exposure_ms` for older consumers.
They include qualified reads and legacy exposure records; with current capture
alone, exposed-line coverage equals read-line coverage. They no longer measure
skimming or total viewing duration and are not shown as such in the report.

Each source event creates a raw import plus an observation annotation. A read
also creates one semantic work record (three chain records total), with a Git
link when its context first needs anchoring. A typical keyboard change creates
two source events plus a work record, FileOp, and path annotation (seven chain
records). Each new editor open/close also creates three records: a source,
annotation, and lifecycle activity, plus a Git link when needed. Startup, saves,
and context changes add their own records. Ten minutes on one unchanged view produce one read, plus lifecycle
overhead; typing still produces exact versioned change evidence.

The metric is line based. It neither proves comprehension nor measures the
fraction of characters read within a line. Explicit renames maintain path
continuity. Untitled Save As, external moves, detached windows, notebook/diff
editors, and remote/web extension hosts do not yet have complete provenance
continuity. Full snapshots preserve code for later re-analysis as those cases
gain support.

## Verification

The 0.1.7 regression suite exercises real keyboard input in VS Code 1.136.2
with code and History simultaneously visible. It observes stored events and
rendered rows without calling coverage or forcing capture to flush. The measured
run showed the first unsaved edit in 354 ms and saved-event persistence in 21 ms
(47 ms including the test's keyboard dispatch and polling). Seven changes,
including a Backspace correction, retained seven receipts and one live edit row.
Split tabs produced one open/final close, and restarting the recorder with split
tabs present retained one inventory entry without another open activity. The
existing host-restart test separately checks persistent identity and ancestry.

A release-service replay on the existing 1.7 GB source history (2,438,873 records)
took 43.6 seconds to create the first capture checkpoint. Two fresh service
processes then acknowledged the same already-recorded event in 64.4 and 60.2 ms,
each decoding zero source records. All requests accepted zero new events and
replayed one; the check did not inject artificial human work. These are local
measurements, not a latency guarantee during arbitrary external writer activity.

Build the native service, synthetic fixture generator, and extension:

```sh
cargo build -p editchain-node --bins --example editor_work_fixture --locked
cd extensions/vscode-editchain
npm run compile
npm run test:capture:types
npm run build:renderer
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
complete seven-scenario suite passed on 1.137.0 with the reduced event stream and
visible tab lifecycle. The sixth scenario opens and closes a real text tab,
checks unchanged coverage and connected graph rows, and waits for layout to
settle before saving its screenshot. The seventh restarts the complete VS Code
application with the same profile and chain, verifies a fresh recorder and the
same GUID/group, walks the graph path back to the prior recording, and waits for
visible graph dots and completed connection animations before its screenshot.
The test uses WebDriver's application restart because direct window reload
terminates its extension-test proxy. Three capture sessions retain one unsigned
identity; the retained source stream contains no exposure events. The
sample measured 39 reading-indicated lines and one edited line; the brief
distant jump added no reading or exposure coverage, with zero capture gaps. Exact viewport
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
disposal, overlapping requests, reading boundaries, and serialized diff retries
that cannot cross live epochs or replaced panels.

Read-capture regressions cover ten minutes without duplicate reads, local-only
selection and viewport observations, brief visits that cannot accumulate,
configured dwell thresholds, early/delayed timer callbacks, active/background
tab closure, split identities, background agent edits, orderly shutdown, and
byte-identical legacy replay. Native service tests check exact read revisions,
recorded dwell policies, lifecycle records without inferred work, and the number
of canonical records created by a read.

Validation on 2026-09-12: all 21 editor service tests and the version-4 cache
upgrade regression passed; all 83 extension
harness tests passed; extension compilation and capture-test type checks passed;
the production WASM renderer built; all seven real VS Code scenarios passed;
`./scripts/lint.sh` exited 0 with **`RESULT: PASS`**. The lint command completed
format, check, clippy, workspace tests, doc tests, and dependency checks without
policy changes or new suppressions.

Identity regression tests cover concurrent GUID creation, restart coalescing,
interleaved recorder processes, disagreeing wall clocks, independent people and
workspace streams, mid-session identity rejection, repeated batches, and exact
source-only recovery after missing-payload repair.

Validation logs and actual VS Code screenshots for persistent human identity are
retained under `outputs/vscode-capture/human-identity/`. Build and test the current checkout
with the commands above; the native service and extension renderer must come
from the same build.

After updating this branch, rebuild the native service as well as the extension:
the new `code_read` payload and activity contract require a service that recognizes them. A process
already running an older binary must be restarted before the new extension
sends reads. Set `editchain-history.servicePath` to the matching build when the
open workspace is a different checkout.

The installed extension must be version 0.1.2 or later for persistent human
identity across recorder sessions (0.1.1 introduced lifecycle graph rows).
Installing a VSIX updates disk files; reload the VS Code window to replace an
already-running older recorder and its service clients. Historical brief
exposure records remain retained even after new capture stops producing them.

### Optional local build with editor origins

`npm run package` keeps the extension on stable APIs. After compiling, run
`npm run package:editor-origins` to produce
`outputs/editchain-history-0.1.6-editor-origins.vsix` from an isolated staging
directory. Its manifest declares only `textDocumentChangeReason`; packaging
does not change the ordinary manifest or enable APIs in an existing VS Code.
The native service must also be rebuilt to accept retained origin metadata.

Install that VSIX and launch VS Code with
`--enable-proposed-api ambientlight.editchain-history`. For persistent local
opt-in, merge `"enable-proposed-api": ["ambientlight.editchain-history"]` into
the runtime arguments opened by **Preferences: Configure Runtime Arguments**,
preserving other entries, then quit and relaunch VS Code. These are local
development builds; Microsoft's [proposed-API guidance](https://code.visualstudio.com/api/advanced-topics/using-proposed-api)
does not permit publishing them to the Marketplace.

The EditChain output channel logs the loaded version/path on activation and,
after the first mutation, either `detailed editor reasons` or
`stable selection hints (partial coverage)`. Without runtime opt-in the local
build falls back to stable observations. Old recorded changes are not
retroactively relabeled from timing guesses.
