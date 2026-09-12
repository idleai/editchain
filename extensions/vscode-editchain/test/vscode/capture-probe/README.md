# Editor capture feasibility tests

This is a **test-only extension**, loaded by `wdio.capture.conf.ts` in a disposable
desktop VS Code process, profile, workspace, and X display. The shipped EditChain
extension does not load this observer. No Rust sidecar or provider credentials are
needed. The existing WebdriverIO dependencies download VS Code and Chromedriver.

From `extensions/vscode-editchain` on Linux with Xvfb installed:

```sh
npm ci
npm run test:capture:types
npm run ui:vscode:capture
npm run ui:vscode:capture:baseline
EDITCHAIN_CAPTURE_PROPOSED=1 npm run ui:vscode:capture
```

The default is pinned to **1.137.0**, the stable release examined in the research
plan. `EDITCHAIN_CAPTURE_VSCODE` selects another exact version. Each execution
uses fresh synthetic files. The Linux x64 baseline script fetches ChromeDriver
114 from Google's legacy archive because the current WDIO downloader uses the
newer archive, which returns 404 for that version. This follows Google's
[pre-115 driver guidance](https://developer.chrome.com/docs/chromedriver/downloads/version-selection).
The optional proposed run adds
`textDocumentChangeReason` to a temporary copy of the fixture manifest and passes
`--enable-proposed-api=ambientlight.editchain-capture-probe` to the development
host. It does not enable proposals in the production extension.

Each run writes `trace/capture-<version>[-proposed]/`:

- `results.json`: actual VS Code/Electron versions, available capabilities, and
  individual test outcomes. These describe the running executable, not merely
  the requested download.
- `events.json`: immutable snapshots, raw replacement batches, API object IDs,
  event keys, viewport/selection/tab events, timestamps, and scenario labels.
- `run.json`: requested configuration, completion time, and process exit code,
  including runs that fail before an individual test finishes.
- `wheel-input.jsonl`: independent DOM evidence that trusted, modifier-free
  wheel input reached the workbench.
- `folded-code.png`, failure screenshots, and driver logs for diagnosis.

The fixture is removed on completion; traces remain. Re-running a configuration
replaces its trace directory. The driver log may contain the synthetic source text.
Never point this fixture at a real workspace; the config creates and modifies
test files, uses the test display's clipboard, and exercises save/rename commands.

The suite sends WebDriver keyboard input and Chromium mouse-scroll gestures through the VS Code UI for
typing, deletion, paste, undo/redo, completion acceptance, and scrolling. Other
scenarios deliberately invoke extension APIs or workbench commands. These are
automated experiments, not a human reading study. The completion provider is
deterministic and does not call an AI model. Scenario labels supply test ground
truth; a production observer does not receive them. Chromium 114's direct wheel
dispatch supplies zero legacy `wheelDeltaY`, which VS Code 1.85 interprets as no
movement. The [Chromium gesture API](https://chromedevtools.github.io/devtools-protocol/tot/Input/#method-synthesizeScrollGesture)
supplies usable wheel ticks without replacing any VS Code handlers. The suite
asserts both actual input delivery and a resulting public viewport event.

The independent replay oracle applies each emitted replacement array in its
original order with JavaScript UTF-16 offsets and compares every resulting buffer
with the synchronously copied event after-state. It checks state-only events too.
The probe's WeakMap IDs describe observed API objects; production recording needs
separate lifecycle/incarnation semantics, particularly for language changes.

On versions with `WindowState.active`, the activity test deliberately waits for
inactivity (normally up to about a minute) before sending wheel-only input and
then a keyboard event. On 1.85 it asserts that this capability is unavailable.
The full suite normally takes longer than this wait plus host startup/downloads.

This harness does not implement or validate durable ingestion, crash recovery,
remote or detached windows, IME composition, actual AI/chat integration, or a
reading/skimming classifier. See the [research plan](../../../../../docs/vscode-editor-capture-plan.md)
and [runtime findings](../../../../../docs/vscode-editor-capture-results.md).
