# VS Code human-work capture

The production extension records human work in the existing EditChain store and
provides **EditChain: Show Human Work Coverage**. The
[research](vscode-editor-capture-plan.md) and
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
| `observed_edit_batch` | Ordered change references without human attribution; one cumulative file row marked unattributed, excluded from human and AI-origin coverage. |
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

### Larger full snapshots (0.1.10)

The default and supported maximum buffer size are 8 MiB of UTF-8 text, including
unsaved content. Every edit still retains complete before/after text and the
original UTF-16 changes. Historical diffs resolve those retained revisions even
after another editor or agent changes the working file. Explicit smaller user
limits remain respected. Skip diagnostics identify either the measured byte
size and configured limit or the NUL-character binary heuristic. Current saved
files up to the same 8 MiB limit participate in human-work coverage reports.

Large snapshot events travel alone above the normal 4 MiB batch target. The
145 MiB event bound and 160 MiB native frame bound accommodate before, after,
and a full replacement for an 8 MiB buffer, including worst-case sixfold JSON
escaping and metadata. Native validation checks both full snapshots, initial
snapshots, UTF-16 replay, and large viewport coordinates. Intermediate replay
is bounded to twice the maximum buffer's UTF-16 length.

The outbox freezes each event into UTF-8 JSON once and sends those persisted
bytes directly, with acknowledgement metadata cached separately. A single
previous snapshot encoding is reused for unchanged text in a validated single
replacement. Each event still contains both complete independent snapshots;
multi-range edits, mismatches, and surrogate-interior offsets use full encoding.
Recovered journals are decoded once to restore acknowledgement metadata.
Delivery starts after each durable batch, while later batches are written. At
most one durable batch stays in memory ahead of the active request, avoiding
an immediate disk reread. When older batches are queued on disk, reading the
next journal overlaps the current request; failed reads leave evidence pending
for retry. Recovery still reads and verifies pending journals.
Journal writes submit the full buffer and retry short writes, avoiding repeated
512 KiB write continuations on a busy editor event loop. Transport copies the
saved bytes into the final frame without a UTF-8 decode/encode round trip.

The native connection also keeps one previous snapshot encoding and reuses its
unchanged JSON regions. Its source encoder produces bytes identical to the
established sorted JSON format, preserving source hashes and replay identities.
Native retry repair checks the recorder ID before encoding payloads; new input
needs no repair. After durable admission, projection reuses the validated request
event instead of reading and decoding its just-written blob. Ordinary single
replacements compare the full prefix, inserted text, and suffix directly;
multi-range changes and offsets inside surrogate pairs retain UTF-16 replay.
Retained encodings, identities, full snapshots, and exact acknowledgements are
unchanged; restart/recovery still reads and verifies canonical evidence. Native
acknowledgements include preparation, storage, and projection timings for slow
delivery diagnostics; the outbox also reports journal and read times. Rebuild
the native service as well as the extension when
upgrading from the earlier frame and replay limits.

The large-file UI test types 14 real keys into 2 MiB and 8 MiB files, checks
every retained revision, saves without forcing capture, and opens a native
VS Code diff with both complete expected texts. It also waits for the actual
edit to paint. A release-build reference run on VS Code 1.137.0 without the
proposed API measured:

| Buffer | First edit row | Save observed in durable history | Intermediate revisions |
| --- | ---: | ---: | ---: |
| 2 MiB | 219 ms | 86 ms | 14/14 |
| 8 MiB | 376 ms | 774 ms | 14/14 |

These timings describe this test host and typing workload. Coverage and exact
diff tests separately cover maximum-size files, restart/retry, external edits,
and later working-file drift. Unicode tests compare optimized source bytes
against the established canonical serialization and verify UTF-16 boundaries.

If capture changes the history snapshot during a coverage query, the independent
report worker makes at most three attempts. Persistent errors return promptly
without waiting for their notification to be dismissed, allowing another report
to start while that notification remains visible.

### Independent capture and coverage (0.1.9)

Coverage reports replay the entire retained history. Previously the status-bar
click ran that query on the recorder's serial native connection, blocking both
`RecordEditorEvents` and `GetEditorContext`. In a reported 1.7 GB history, retained
edit timestamps showed a 92-second delay before blob persistence. A diagnostic
coverage request blocked a queued context observation for over 35 seconds; the
same context request alone completed in 12 ms.

Coverage now owns a separate short-lived client. Repeated requests coalesce,
and disposal stops the report worker. Git-context observation also has its own
client. The status bar opens **EditChain: Show Tracking Status**, which reads
runtime diagnostics without a coverage scan or capture flush. Explicit coverage
remains available in the Command Palette. Slow delivery logs separate outbox
queue time, native request duration, and total event age from renderer latency.
This does not make full-history coverage scans or cold index builds inexpensive.

The input path remains event-driven: VS Code document events enter a 25 ms
outbox frame, durable acknowledgements wake live history, and subsequent edit
receipts publish within 100 ms. The live collector's 250 ms fallback poll and
the 15-second Git-context poll do not gate acknowledged human work. Runtime
API enablement is independent of this transport fix; retained unattributed
observations keep their original evidence.

### Direct attribution and visible uncertainty (0.1.8)

Capture and attribution now have separate owners. `EditorCapture` retains exact
document changes; `EditorAttribution` consumes direct reasons from that same
event and publishes human input immediately. Selection correlation is only the
limited stable fallback. A missing, rejected or late selection resolves to an
`observed_edit_batch` within 250 ms plus its bounded publication frame, instead
of dropping the change from Activity. Saves and shutdown resolve remaining
candidates before their lifecycle boundary.

Observed edits derive `vscode.work` records with kind `observed_edit`, exact
before/after `FileOp`s, and file source `editor`. Their semantic operations carry
neither HUMAN nor INFERRED tags. Activity displays a single cumulative file row
with an **unattributed** badge. Human and observed groups are separate, so an
automatic mutation cannot become part of a human diff; a background observed
edit cannot replace the active human group's normalization state. The coverage
query continues to count only valid human receipts and imported agent origins.
Older raw observations are retained as originally recorded; this change does
not retroactively assign them authorship or manufacture new receipts.

The appropriate passive API is `workspace.onDidChangeTextDocument`. Its stable
`reason` identifies undo/redo. The proposed `textDocumentChangeReason` capability
adds `detailedReason` to this event with a source and mechanism metadata. It
does not require keyboard interception, command replacement, an agent tool
hook, or waiting for save. There is no public stable general command-execution
observer in the checked VS Code 1.136.2 extension API. The installed source gate
selects a different event emitter for enabled extensions; a TypeScript cast
cannot unlock it. See [the proposal](https://github.com/microsoft/vscode/blob/1.136.2/src/vscode-dts/vscode.proposed.textDocumentChangeReason.d.ts)
and [the runtime gate](https://github.com/microsoft/vscode/blob/1.136.2/src/vs/workbench/api/common/extHost.api.impl.ts#L1326).

Use `npm run install:local:editor-origins -- --runtime-args PATH` from the
extension directory for local installations. PATH is the JSONC file opened by
Preferences: Configure Runtime Arguments. The installer stages the proposal in
the VSIX manifest and preserves existing runtime preferences while adding this
extension's opt-in. A full VS Code restart is required. A read-only
`editchain-history.trackingStatus` query verifies the mode actually observed on
events (`direct`, `limited`, or `unverified`), with observed and attributed
change counts. Packaging a proposal alone does not prove runtime enablement.

The live log now records renderer acknowledgement duration separately from
native capture/projection time. Source-blob timestamps alone are not an
end-to-end UI latency measurement.

The delivery path is direct document event → local outbox (25 ms frame) →
durable native append → live projection → replacement viewport. The recorder
has its own native client; its acknowledgement wakes History. On initial
attachment, History consumes the durable tail before scanning the provider
archive, including editor events acknowledged before the panel was ready.
Changing recorder settings leaves the History service running.

Native live handoffs fetch the actual visible rows before acknowledging the
revision. Previously they loaded the 400-row off-screen margin first, adding
hundreds of milliseconds to every revision in a large history. Normal prefetch
still fills that margin after the viewport publishes. Snapshot validation,
selection and scroll-anchor restoration remain part of the handoff.

The VS Code 1.136.2 regression run used an owned copy of a 1.7 GB canonical
chain (2.44 million records), its source blobs, and 481 provider headers. With
derived indexes prepared, History became ready in 3.58 seconds; the first typed
edit appeared in 312 ms and deletion of existing code in 450 ms. All 16 input
changes were attributed and grouped correctly. The save payload's write timestamp
was 70 ms after the event; the filesystem observer found it within 470 ms of
Save (neither timing is the native acknowledgement timestamp). No coverage query
or forced flush was used. The initial rebuild of the
copied indexes took 44.5 seconds for capture and 332.4 seconds for the graph;
these cold rebuild costs remain. Provider headers exercise discovery and native
startup, not full concurrent replay of every historical rollout. These are
local measurements, not a latency guarantee under arbitrary provider or disk load.

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

Limits: 128 events and approximately 4 MiB per normal outbound batch; a larger
event travels alone, up to 145 MiB encoded. The outbox allows 256 MiB of queued
serialized data and 512 MiB pending disk data per recorder; the default and
maximum buffer limit are 8 MiB. These are payload bounds, not process RSS limits.
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

## Portable human-history archive

Human work is normally recorded only in the chain. An optional archive writes the
same recorder events to local JSONL, so a rebuilt chain can be re-imported
without the original chain. Archiving is off by default.

- `editchain-history.tracking.jsonl.enabled` (default `false`) turns the archive on.
- `editchain-history.tracking.jsonl.directory` (default `""`) selects where the
  archive files are written. An empty value uses `human-history` under the
  extension's global storage directory. Absolute paths and `~` / `~/` paths are
  supported. A relative path resolves against the single workspace folder and is
  rejected for a multi-root or folderless window. Choose a location outside the
  `.editchain` chain directory so the archive survives a chain rebuild.

For example:

```json
{
  "editchain-history.tracking.enabled": true,
  "editchain-history.tracking.jsonl.enabled": true,
  "editchain-history.tracking.jsonl.directory": "~/editchain-human-history"
}
```

Archiving requires a trusted workspace and `editchain-history.tracking.enabled`
(default `true`); disabling tracking disables the archive too. Each line is one
JSON record with these keys:

- `format`: the constant `editchain-human-history`.
- `schema`: the archive schema version, currently `1`.
- `workspace_path`: the absolute workspace path recorded for the event.
- `event`: the full existing editor event, carrying its source snapshots,
  before/after text, attribution, sampled Git context, and lifecycle evidence.

The archive retains all source events and embeds text instead of chain blob
references. A single line can still reference an earlier change or revision in
the same archive, so it is not on its own a complete reconstruction of the work.
Like the chain recording, the archive retains code contents and unsaved buffers
locally, and the existing capture limits apply: the whole unsaved buffer is
capped at the configured `tracking.maxFileBytes` (8 MiB default and maximum),
NUL-containing or oversized buffers are skipped with an explicit coverage gap.

One file is written per continuous VS Code activation and archive destination,
named `YYYY-MM-DD-session-0001.jsonl`. The date is the local calendar day when the
file is allocated and is kept for that file's life even if it crosses midnight.
The counter is a per-day, zero-padded number of at least four digits: each new
file takes the largest existing counter for that day plus one and is created
exclusively, so two activations cannot collide. Reloading the window ends the
current file and starts the next one. Within one activation the destination
keeps its file: restarting tracking (pause and resume), disabling and
re-enabling the archive, or switching to another destination and back all reuse
the file already allocated for that destination instead of rotating.

If the archive directory is unwritable or the disk fills, the archiver reports the
error in the log and an error notification and stops writing archive files; normal
chain tracking continues. A destination that failed is retained for the rest of
the activation, so re-enabling it or switching back to it does not restart
writes; fix the underlying problem and reload the VS Code window to resume, which
allocates a new file. Separately, exhausting the chain outbox can pause capture
until it is resumed.

Re-import an archive with the `human` provider, or point the CLI at a directory
holding many JSONL files:

```sh
cargo run --release -p editchain-node -- import \
  --provider human \
  --sessions-dir /path/to/human-history \
  --workspace /path/to/project \
  --chain /path/to/project/.editchain
```

`--sessions-dir` accepts either one JSONL file or a directory of them. Admission
is idempotent and uses the normal import path, so re-running over an
already-imported archive does not duplicate work. Add `--dry-run` to preview
changes without writing.

The importer captures each file's byte length when it discovers it and copies that
prefix to private temporary storage. Both validation and replay read those copies,
so rewriting, replacing, or deleting an original file cannot change the bytes
admitted after validation. Imports, including dry runs, need temporary disk space
for the captured source bytes; the copies are removed when the import finishes.
Preflight also preserves its workspace selection for replay, including when a
workspace symlink changes between the two passes.

All archives are validated before the chain is modified. Preflight checks
envelope format and schema, per-event validity, per-session sequence continuity,
and a stable recorder identity within a session. Malformed
JSON, an unsupported schema version, an out-of-order sequence, or a truncated
archive therefore fails the import without writing a partial chain. Because both
passes stop at the captured length, an archive still being appended to cannot
inject unvalidated tail records, and records appended past the captured prefix
wait for the next import. A prefix that ends mid-record fails with an explicit
error; a complete final JSON line is accepted without a trailing newline.

Cross-record checks that need the chain itself run during replay, so a failure
there is reported and leaves earlier admitted records durable rather than rolling
the whole import back; re-running resumes from what was accepted.

Records carry the workspace they were captured in. The importer matches the
archive's recorded workspace against `--workspace`, preferring a canonical match
when both paths exist; records for other workspaces are counted and skipped, and
an archive with no matching records fails. History is not relocated to a
different workspace.

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
`outputs/editchain-history-0.1.10-editor-origins.vsix` from an isolated staging
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

An empty `enable-proposed-api` array in `argv.json` enables no extensions; the
helper adds EditChain's ID even when that array already exists. VS Code expands
each array entry into a startup flag, so this differs from passing the CLI flag
without an ID. See [VS Code's runtime-argument expansion](https://github.com/microsoft/vscode/blob/645f29cc3176500b4b5762ba887cf2a7f0ffdf2c/src/main.ts).

The EditChain output channel logs the loaded version/path on activation and,
after the first mutation, either `direct document change reasons` or
`limited; unconfirmed edits remain visible as unattributed`. Without runtime opt-in the local
build falls back to stable observations. Old recorded changes are not
retroactively relabeled from timing guesses.
