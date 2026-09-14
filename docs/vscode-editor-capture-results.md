# VS Code editor capture: runtime findings

Test date: **September 11, 2026**. Follow-up to the [research and implementation plan](vscode-editor-capture-plan.md). No subagents were used.

The experiments demonstrate capture of buffer edits, document/tab/editor state, and vertical viewport changes through public extension events. **These events do not establish human authorship or that someone read a file.** The added code is a disposable test extension and a reproducible runtime suite; production recording and durable ingestion remain to be implemented.

## Executed configurations

The [23-scenario suite](../extensions/vscode-editchain/test/vscode/editor-capture.e2e.ts) runs in fresh Linux x64 desktop Extension Development Hosts under Xvfb, with synthetic workspaces, isolated profiles, and no external AI providers. The probe records the actual executable version and capability state.

| Configuration | Final result | Capability path |
| --- | --- | --- |
| VS Code 1.85.0 | **23/23 passed; exit 0** | Stable APIs; unavailable window-activity capability handled explicitly. |
| VS Code 1.137.0 | **23/23 passed; exit 0** | Stable APIs, including window activity. |
| VS Code 1.137.0 with `textDocumentChangeReason` enabled | **23/23 passed; exit 0** | Development-only proposed reason path. |

The 1.85 activity scenario verifies the unsupported-capability behavior; its timed inactivity experiment runs only where that capability is available. The proposed configuration uses the same stable executable with the explicit development enablement flag. It is not an Insiders or Marketplace-distribution test.

The final runs completed at 19:19–19:20 UTC. Each captured and replayed 52 document-change notifications: 31 with text replacements and 21 state-only events. The baseline used Electron 25.9.7 / Chromium 114.0.5735.289; current stable used Electron 42.10.0 / Chromium 148.0.7778.280. Total: **69 successful scenario executions**, with zero skipped tests. Both `npm run test:capture:types` and `npm run compile` also passed.

## Buffer replay

The independent oracle applies each raw replacement array in its emitted order, using JavaScript UTF-16 offsets, and compares the result with the text copied synchronously from the event's document. It validates **every observed change**, including state-only notifications, rather than checking just the final saved file. [Probe](../extensions/vscode-editchain/test/vscode/capture-probe/extension.js); [assertions](../extensions/vscode-editchain/test/vscode/editor-capture.e2e.ts).

Verified cases include UI typing, backspace, clipboard paste, programmatic edits, two cursors, multiple replacements, same-offset insertions, snippets, undo/redo, formatting, inline completion acceptance, CRLF-to-LF conversion, autosave, save-as, and disk reload.

- In `A😀B`, the position after the emoji is offset **3 in UTF-16** and **5 in UTF-8 bytes**. The two-cursor transaction emitted offsets `[7, 3]` against a CRLF buffer.
- Two same-offset insertions requested as `FIRST`, then `SECOND`, arrived as `SECOND`, then `FIRST`, both at offset zero. Replaying the emitted order produced `FIRSTSECOND`. Sorting or assuming request order would lose this distinction.
- The tested EOL conversion emitted a full-buffer replacement. Undo and redo preserved their respective public reasons.
- Some change events contained no replacements and only updated dirty state. The first text edit could arrive before the separate notification that set `isDirty`; filtering out changes solely because `isDirty` is false would lose evidence.
- Explicit API save reported the `Manual` save reason; delayed autosave reported `AfterDelay`. Neither save reason establishes who initiated the edit.

These are decoded editor-buffer checks. They do not establish byte-identical replay of arbitrary original file encodings or BOMs.

## Attribution

In the stable runs, UI input, `WorkspaceEdit`, and `TextEditor.edit` had the same change-event keys: `contentChanges`, `document`, and `reason`. Ordinary changes had no undo/redo reason and no `detailedReason` property. Formatter edits and accepted inline completions also lacked stable authorship information. The inline completion was supplied by a deterministic fixture provider; no real AI model was called.

The proposed path exposed useful but incomplete mechanism evidence:

| Trigger | Observed `detailedReason` in 1.137.0 |
| --- | --- |
| UI typing | `source: cursor`, metadata `kind: type`, `detailedSource: keyboard` |
| Programmatic `executeCommand('type', …)` | **The same detailed reason as UI typing.** One command produced multiple character events. |
| Clipboard paste | `source: cursor`, metadata `kind: paste` |
| `WorkspaceEdit` | `source: unknown` |
| `TextEditor.edit` | `source: unknown`, metadata `name: MainThreadTextEditor` |
| Formatting | `source: unknown`, metadata `name: formatEditsCommand` |
| Snippet insertion | `source: snippet` |
| Completion acceptance | `source: inlineCompletionAccept`, with `$extensionId` identifying the fixture provider |
| EOL conversion / disk reload | `eolChange` / `reloadFromDisk` |

The runtime strings differ from the examples in the [proposal declaration](https://raw.githubusercontent.com/microsoft/vscode/645f29cc3176500b4b5762ba887cf2a7f0ffdf2c/src/vscode-dts/vscode.proposed.textDocumentChangeReason.d.ts). Preserve unknown values and metadata. Even a `keyboard` detailed source cannot certify physical human input, as the programmatic counterexample demonstrates.

## Compatibility and lifetimes

On **1.85**, accessing `window.state.active` throws an error requiring the `windowActivity` proposal. A plain `typeof` or optional-chain access is therefore unsafe. The fixture probes once, catches only that specific unavailable-proposal error, and then records an explicit unavailable capability. This corrects the original plan's overly simple feature-detection wording.

The lifecycle experiments verified an already-open dirty buffer at capture startup, a loaded document with no tab/editor, preview replacement, two views of one document, tab close/reopen/movement, workspace rename, external reload, and untitled save-as.

Split views have separate editor/tab identities while sharing the document object. Closing a tab does not require a simultaneous document-close event. Changing language emits document close/open events **with the same API object and `isClosed === false`**. Production code therefore needs explicit lifecycle/incarnation rules; a WeakMap document ID alone is insufficient.

## Viewports and attention

Wheel gestures, PageDown, `revealRange`, workbench layout changes, wrapping, and folding all produced public viewport updates. Wheel-driven and programmatic updates carried the same keys, `textEditor` and `visibleRanges`, with no cause field. Folded code returned disjoint ranges; filling the gap would invent exposure. A jump to line 800 did not expose the intervening lines, and the other split retained its own viewport.

The terminal-focus test checked actual DOM keyboard focus. `activeTextEditor` still referred to the code pane and the workbench remained focused. These fields cannot by themselves establish attention to code.

On current stable, the timed test let `WindowState.active` become false, sent a mouse-wheel gesture, and observed a viewport change while the flag remained false. A subsequent keyboard event restored activity. This supports retaining quiet visible intervals separately from interaction evidence; an inactivity flag must not turn possible reading into a definitive absence of reading.

## Harness corrections and limits

The test harness waits for editor/selection transitions before sending dependent input. Native window resizing through ChromeDriver was unavailable in Electron, so the layout test changes the editor viewport through the workbench panel and wrapping.

Chromium 114's direct synthetic wheel input delivered `deltaY: 600` with `wheelDeltaY: 0`. VS Code 1.85 prioritizes that legacy field, so it interpreted the input as zero movement. The suite uses a [Chromium mouse-scroll gesture](https://chromedevtools.github.io/devtools-protocol/tot/Input/#method-synthesizeScrollGesture), which supplies nonzero wheel ticks, and checks both trusted DOM input delivery and the resulting extension event. This avoids misreporting a test-input problem as an absent VS Code hook. [1.85 wheel normalization](https://raw.githubusercontent.com/microsoft/vscode/1.85.0/src/vs/base/browser/mouseEvent.ts).

The old baseline also needs ChromeDriver 114 from Google's legacy archive; the current WDIO automatic downloader targets the newer archive and receives 404. The baseline script handles this using the [documented pre-115 distribution path](https://developer.chrome.com/docs/chromedriver/downloads/version-selection).

Still untested: real human reading/skimming, physical IME composition, real AI/chat integrations, refactoring providers, remote/detached/native multiple-window behavior, multi-root ownership, diff/notebook/custom editors, encoding changes, large-file load, and production ingestion/recovery. The probe is in-memory and fixture-scoped. Its callback timings exclude durable transport and are not a production performance benchmark. This completes the core desktop feasibility experiments, not the research plan's entire future acceptance matrix.

## Reproduction and evidence

From `extensions/vscode-editchain`:

```sh
npm ci
npm run test:capture:types
npm run ui:vscode:capture
npm run ui:vscode:capture:baseline
EDITCHAIN_CAPTURE_PROPOSED=1 npm run ui:vscode:capture
npm run compile
```

Generated evidence is retained under `extensions/vscode-editchain/trace/` in `capture-1.85.0`, `capture-1.137.0`, and `capture-1.137.0-proposed`. Each contains `run.json`, `results.json`, complete `events.json`, wheel input observations, and screenshots/logs. Re-running a configuration replaces its trace directory. [Harness instructions](../extensions/vscode-editchain/test/vscode/capture-probe/README.md).

Repository validation: `./scripts/lint.sh` returned **`RESULT: PASS`**, exit code **0**. All six checks passed: cargo fmt, cargo check, cargo clippy, cargo test, cargo doc tests, and cargo deny. Full output is retained at `outputs/vscode-capture/lint.log`. No quality thresholds, exclusions, or production extension behavior were changed.
