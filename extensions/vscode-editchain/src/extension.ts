import * as vscode from 'vscode';
import { resolveServicePath, StdioClient } from './stdioClient';

// The single history panel. Reused across `open` invocations so we never create
// two webviews of the same type (which races VS Code's service-worker
// registration and can throw "Could not register service worker"). The panel
// always renders the Rust/WASM history view: media/rust-history/loader.js is
// the ONLY script the webview loads. It initializes the wasm-bindgen module
// and calls the Rust shell's startHistoryView(), which owns the whole runtime
// (window/frame/lane presentation on a wgpu canvas, virtual paging, search,
// profile switching, selection and raw-JSON routing). Production media/main.js
// and media/gpu-preview/bootstrap.js are NOT loaded here; they remain only as
// the offscreen CPU/SVG parity-reference side of the harness oracle.
let historyPanel: vscode.WebviewPanel | undefined = undefined;
// Output channel for debugging the service bridge and panel lifecycle.
let output: vscode.OutputChannel | undefined = undefined;
// Status bar item showing how many history nodes are loaded vs total.
let statusItem: vscode.StatusBarItem | undefined = undefined;
// The last successful Open response body. Held so command reuse or a genuinely
// recreated Rust renderer instance can receive the authoritative `open` + `ready`
// handshake without rebuilding the workspace.
let lastOpenBody: any = null;
// The most recent terminal Open error. Successful bodies and errors are kept
// separately because only a success permits command reuse without another
// Open, while a recreated renderer still needs the current error replayed.
let lastOpenError: string | null = null;
// True while an Open request is in flight. While set, a reveal must NOT replay
// the previous workspace's open body: a new (authoritative) Open is pending and
// its response will deliver the real state. The stale body is cleared before
// the pending Open is issued, so the replay path is doubly safe.
let openPending = false;
// Monotonic ownership token for Open requests and the CURRENT panel. Bumped on
// every startOpen and on every disposal of the current panel. An async Open
// response captures the epoch it was issued under and only mutates the shared
// cache/pending state or posts to the webview while that epoch is still
// current: a late response from a superseded Open (its panel was disposed or a
// newer Open was issued) is dropped entirely, so it can never clear a newer
// Open's pending state or install a stale workspace body for replay.
let openEpoch = 0;
// Identity of the currently loaded Rust renderer context and the context that
// has already received the latest Open result. `retainContextWhenHidden` keeps
// the normal raw-JSON -> Back path alive; this handshake is the fallback for a
// real context recreation (window reload, renderer recovery, or memory
// pressure).
let rendererInstanceId: string | null = null;
let openDeliveredToRenderer: string | null = null;

// Generous finite deadline for NON-Open service requests (window fetches,
// search and object resolution). The measured first-window time on a large
// chain is close to
// a minute, so 120s is a generous bound; Open itself stays UNBOUNDED (it can
// build the chain + git graph for minutes). A timed-out window/search surfaces
// a visible error in the webview and suspends the progressive loader until the
// user explicitly retries or re-opens.
const NON_OPEN_TIMEOUT_MS = 120_000;

/**
 * Activate the EditChain History extension.
 *
 * The extension is a thin shell: it spawns the native Rust service, forwards
 * messages between the webview and the service, and owns the webview lifecycle.
 */
export function activate(context: vscode.ExtensionContext): void {
  const client = new StdioClient();
  context.subscriptions.push({ dispose: () => client.stop() });

  // Output channel for debugging the service bridge.
  const out = vscode.window.createOutputChannel('EditChain History');
  output = out;
  context.subscriptions.push(out);
  client.setLog((line) => out.appendLine(line));

  // Read-only JSON content provider: documents opened under the
  // `editchain-json:` scheme are read-only by default (content providers cannot
  // be edited), which is exactly what we want for raw node views.
  const jsonProvider = new JsonContentProvider();
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider('editchain-json', jsonProvider)
  );

  const openCommand = vscode.commands.registerCommand('editchain-history.open', () => {
    openHistoryView(context, client, jsonProvider);
  });
  context.subscriptions.push(openCommand);

  // Status bar item for the loaded/total node count. Created once and shown only
  // while the history viewer is open; hidden when the panel closes.
  statusItem = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    100
  );
  statusItem.command = 'editchain-history.open';
  statusItem.tooltip = 'EditChain History — loaded / total nodes';
  context.subscriptions.push(statusItem);
}

/**
 * Update the status bar with the current loaded/total node counts.
 *
 * Called from the webview whenever rows load or the total changes. The item is
 * shown only while the history viewer is open; it is hidden when the panel
 * closes so it doesn't linger after the viewer is gone.
 */
function updateStatusBar(loaded: number, total: number): void {
  if (!statusItem) return;
  statusItem.text = `$(list-ordered) ${loaded} / ${total} nodes`;
  statusItem.show();
}

/**
 * Open (or reveal) the history explorer webview panel.
 *
 * The single panel always renders the Rust/WASM history view (the
 * media/rust-history/loader.js bootstrap) in the active/default column. The
 * Rust shell owns the full runtime — no media/main.js controller or
 * media/gpu-preview/bootstrap.js overlay is loaded in production.
 */
function openHistoryView(
  context: vscode.ExtensionContext,
  client: StdioClient,
  jsonProvider: JsonContentProvider
): void {
  // Reuse an existing panel if one is still open, so we never create two
  // webviews of the same type (which races service-worker registration).
  // Reuse must also RECOVER: the service process may have crashed or been
  // killed since the last open, leaving the panel holding a dead bridge or a
  // visible error. ensureStarted restarts it and the full open handshake
  // re-runs against the fresh process — never just reveal a stale view.
  if (historyPanel) {
    output?.appendLine('[openHistoryView] reusing existing panel');
    const wasRunning = client.isRunning();
    client.ensureStarted(resolveServicePath());
    historyPanel.reveal(vscode.ViewColumn.Active);
    // Re-run the Open handshake when the service process was dead OR when the
    // last Open never produced a successful body (e.g. it returned an Error),
    // so command reuse always ends up with an authoritative view. Skipped while
    // an Open is already pending to avoid issuing duplicate Opens.
    if ((!wasRunning || lastOpenBody === null) && !openPending) {
      output?.appendLine('[openHistoryView] service was not running or no successful open — re-opening chain');
      // A restart Open supersedes any previously cached body: drop it BEFORE
      // the pending Open is issued so the view-state handler cannot replay
      // stale data while the fresh Open is in flight (startOpen clears
      // defensively too).
      lastOpenBody = null;
      startOpen(client, historyPanel);
    }
    return;
  }
  output?.appendLine('[openHistoryView] creating new panel');

  const panel = vscode.window.createWebviewPanel(
    'editchainHistory',
    'EditChain History',
    vscode.ViewColumn.Active,
    {
      enableScripts: true,
      // The renderer retains only a bounded viewport cache, so preserving its
      // context while a read-only raw JSON editor covers the panel is cheap and
      // makes Back instantaneous: the existing DOM, scroll position, and rows
      // are shown instead of booting into "Loading history…" again.
      retainContextWhenHidden: true,
      localResourceRoots: [vscode.Uri.joinPath(context.extensionUri, 'media')],
    }
  );
  historyPanel = panel;
  // A fresh panel is a fresh JS context: never let a body cached from a
  // previous panel/workspace leak into it (e.g. via a view-state event fired
  // while the first Open is still pending).
  lastOpenBody = null;
  lastOpenError = null;
  openPending = false;
  rendererInstanceId = null;
  openDeliveredToRenderer = null;
  // Clear the reference when the panel is closed so a later `open` creates a
  // fresh one instead of reusing a disposed webview.
  panel.onDidDispose(() => {
    output?.appendLine('[panel] disposed');
    if (historyPanel === panel) {
      historyPanel = undefined;
      // Disposing the CURRENT panel invalidates any outstanding Open for it:
      // its response must not be delivered to a dead webview or mutate state
      // for a panel that no longer exists. A stale dispose of a panel that is
      // no longer current must not clear a newer panel's state, so the bump
      // and clears happen only while this panel still owns the globals.
      openEpoch++;
      lastOpenBody = null;
      lastOpenError = null;
      openPending = false;
      rendererInstanceId = null;
      openDeliveredToRenderer = null;
      // Hide the status bar item once the viewer is gone.
      statusItem?.hide();
    }
  });

  // A normal raw-JSON -> Back navigation retains the renderer context, including
  // its bounded row cache and DOM. Do not replay Open on reveal: Open is an
  // authoritative reset and would throw that cache away. If VS Code genuinely
  // recreates the Rust renderer, its `webviewReady` message below carries a new instance
  // id and receives the last Open state only after its listener is installed.
  panel.onDidChangeViewState((e) => {
    output?.appendLine('[panel] view state changed, active=' + e.webviewPanel.active);
    if (e.webviewPanel.active && openPending) {
      output?.appendLine('[panel] retained view active while Open is pending');
    }
  });

  // Start (or restart) the service. The process may have exited since the last
  // open (e.g. it crashed or the user killed it), so re-check liveness instead
  // of trusting a one-time "already started" flag. In-flight requests from a
  // dead process are rejected by the client, so a restart is always safe.
  client.ensureStarted(resolveServicePath());

  // Forward webview -> service.
  panel.webview.onDidReceiveMessage(async (msg) => {
    if (msg === null || typeof msg !== 'object' || Array.isArray(msg)) {
      output?.appendLine('[webview] rejected malformed message');
      panel.webview.postMessage({
        id: -1,
        body: { Error: 'EditChain History: malformed webview message' },
      });
      return;
    }
    // The Rust/WASM shell announces a fresh context via webviewReady after
    // installing its host-message listener; deliver the current Open result
    // to THAT instance exactly once. A new id means VS Code recreated the JS
    // context; the retained raw-JSON -> Back path sends no new handshake and
    // therefore performs no reset or network request. While an Open is
    // pending, delivery waits for its authoritative settle (startOpen also
    // routes the settle to this panel).
    if (msg.type === 'webviewReady') {
      const instanceId = typeof msg.instanceId === 'string' ? msg.instanceId : '';
      if (!instanceId) return;
      if (rendererInstanceId !== instanceId) {
        rendererInstanceId = instanceId;
        openDeliveredToRenderer = null;
        output?.appendLine('[webview] renderer ready: ' + instanceId);
      }
      deliverOpenState(panel);
      return;
    }
    // Intercept the "open JSON editor" request from the webview: fetch the
    // node's details from the service and open a read-only JSON editor instead
    // of forwarding to the service and rendering in the webview.
    if (msg.type === 'openJson') {
      output?.appendLine('[webview] openJson request');
      await openJsonEditor(client, jsonProvider, msg);
      return;
    }
    // Renderer diagnostics and live-region announcements are host-side only.
    if (msg.type === 'log') {
      output?.appendLine('[webview] ' + msg.text);
      return;
    }
    if (msg.type === 'statusText') {
      // Assistive-tech/live-region announcements (profile switches, load
      // progress). Mirrored to the output channel for diagnostics; the status
      // bar count remains driven by the `status` message below.
      output?.appendLine('[webview] status: ' + msg.text);
      return;
    }
    // The webview reports its loaded/total node counts; surface them in the
    // status bar.
    if (msg.type === 'status') {
      updateStatusBar(msg.loaded, msg.total);
      return;
    }
    // The Rust/WASM renderer speaks the production generic bridge: numeric-id
    // { body: <one-key envelope> } frames. ONLY the read-only envelopes the
    // renderer issues are forwarded (GetWindow, FindInHistory, legacy Search);
    // anything else (Open, ResolveObject, GetNodeDetails, ...) is rejected
    // visibly instead of reaching a non-read-only service call. The explicitly
    // handled openJson UI action above remains outside this bridge.
    const id = typeof msg.id === 'number' && Number.isFinite(msg.id) ? msg.id : null;
    const body = msg.body;
    // Single-key envelope guard for the generic bridge: exactly one top-level
    // key, and it must be on the read-only allowlist below.
    const hasOwnProperty = (target: object, key: string): boolean =>
      Object.prototype.hasOwnProperty.call(target, key);
    const isForwardable =
      body !== null &&
      typeof body === 'object' &&
      !Array.isArray(body) &&
      Object.keys(body).length === 1 &&
      (hasOwnProperty(body, 'GetWindow') ||
        hasOwnProperty(body, 'FindInHistory') ||
        hasOwnProperty(body, 'Search'));
    if (id === null || !isForwardable) {
      output?.appendLine(
        '[webview] rejected request (only GetWindow/FindInHistory/Search are forwarded): ' +
          JSON.stringify(msg)
      );
      panel.webview.postMessage({
        id: id === null ? -1 : id,
        body: {
          Error: 'EditChain History: only GetWindow, FindInHistory and Search requests are forwarded by the host',
        },
      });
      return;
    }
    try {
      // Non-Open calls get a generous finite deadline (see NON_OPEN_TIMEOUT_MS):
      // a hung window/search surfaces visibly in the webview (which suspends
      // retries until explicit recovery) instead of spinning forever. Open
      // itself stays unbounded — it can legitimately take minutes.
      const resp = await client.request(body, { timeoutMs: NON_OPEN_TIMEOUT_MS });
      panel.webview.postMessage({ id, body: resp });
    } catch (e) {
      panel.webview.postMessage({ id, body: { Error: String(e) } });
    }
  });

  // Forward service -> webview (unsolicited updates).
  client.setMessageHandler((msg) => {
    panel.webview.postMessage(msg);
  });

  panel.webview.html = getHtml(context, panel.webview);

  // Open the workspace on load, THEN tell the webview to load its first window.
  startOpen(client, panel);
}

/** The workspace root the extension opens (first workspace folder, if any). */
function workspacePath(): string {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '';
}

/** The configured chain directory, relative to the workspace root. */
function chainDir(): string {
  return vscode.workspace
    .getConfiguration('editchain-history')
    .get<string>('chainDir', '.editchain');
}

/**
 * Run the Open request against the service and push the handshake to the
 * webview (open body, then `ready` to fetch the first window).
 *
 * Open is intentionally UNBOUNDED (timeoutMs: 0): building the chain + git
 * graph can take minutes on a large workspace, and the request still settles
 * when the service exits or is stopped, so it can never hang forever. Used on
 * first load AND on command reuse after a service crash (recovery).
 */
function startOpen(client: StdioClient, panel: vscode.WebviewPanel): void {
  // Claim ownership of the Open lifecycle: this Open (and this panel) is now
  // authoritative, and any older in-flight Open becomes a no-op. A response is
  // honored only while this epoch is still current — a newer startOpen or a
  // disposal of the current panel bumps the epoch and invalidates it.
  const epoch = ++openEpoch;
  // Never replay a stale open body while this Open is pending, and never let a
  // previous workspace's body survive a restart that may fail.
  openPending = true;
  lastOpenBody = null;
  lastOpenError = null;
  openDeliveredToRenderer = null;
  // Opening a workspace builds the chain + git graph and can take minutes on a
  // large repo — never apply the request timeout to it. The request is rejected
  // if the service exits or is stopped, so it cannot hang indefinitely.
  client.request(
    { Open: { workspace_path: workspacePath(), chain_dir: chainDir() } },
    { timeoutMs: 0 }
  ).then((resp) => {
    // Late response from a superseded Open: drop it entirely. It must neither
    // mutate the shared cache/pending state (a newer Open may still be in
    // flight, or the panel may be gone) nor post into a dead or stale webview.
    if (epoch !== openEpoch || panel !== historyPanel) {
      output?.appendLine(
        `[startOpen] dropping stale open response (epoch ${epoch}, current ${openEpoch})`
      );
      return;
    }
    // Only a successful Open { Ok } is authoritative: it is the ONLY body ever
    // cached/replayed, and it is the only path that sends `ready` (which makes
    // the webview fetch its first window). An Open Error surfaces visibly and
    // leaves lastOpenBody null so command reuse retries.
    if (!resp || resp.Ok === undefined || resp.Ok === null) {
      const errText = resp && resp.Error !== undefined
        ? String(resp.Error)
        : String(resp);
      output?.appendLine('[startOpen] open returned an error: ' + errText);
      lastOpenBody = null;
      lastOpenError = errText;
      openPending = false;
      deliverOpenState(panel);
      return;
    }
    output?.appendLine('[startOpen] sending open message');
    // Hold the last open body so a genuinely recreated renderer can replay it
    // after its readiness handshake. Ordinary raw-JSON navigation retains the
    // original context and does not enter this path.
    lastOpenBody = resp;
    lastOpenError = null;
    openPending = false;
    deliverOpenState(panel);
  }).catch((e) => {
    if (epoch !== openEpoch || panel !== historyPanel) {
      output?.appendLine(
        `[startOpen] dropping stale open failure (epoch ${epoch}, current ${openEpoch})`
      );
      return;
    }
    output?.appendLine('[startOpen] open failed: ' + String(e));
    // A failed open must not be replayed as an authoritative body later.
    lastOpenBody = null;
    lastOpenError = String(e);
    openPending = false;
    deliverOpenState(panel);
  });
}

/** Deliver the latest terminal Open state to the current Rust renderer instance. */
function deliverOpenState(panel: vscode.WebviewPanel): void {
  if (
    panel !== historyPanel ||
    openPending ||
    rendererInstanceId === null ||
    openDeliveredToRenderer === rendererInstanceId
  ) {
    return;
  }

  if (lastOpenBody !== null) {
    openDeliveredToRenderer = rendererInstanceId;
    panel.webview.postMessage({ id: 'open', body: lastOpenBody });
    // Kept for protocol compatibility with older renderers. The current
    // renderer begins its first window from `open` itself.
    panel.webview.postMessage({ id: 'ready' });
    return;
  }

  if (lastOpenError !== null) {
    openDeliveredToRenderer = rendererInstanceId;
    panel.webview.postMessage({ id: 'open', body: { Error: lastOpenError } });
  }
}

/**
 * Open a read-only JSON editor for a history node.
 *
 * Fetches the node's details from the service (by op id or git oid), then opens
 * an untitled, read-only JSON document in VS Code so the user sees the full
 * pretty-formatted record in a dedicated editor.
 */
async function openJsonEditor(
  client: StdioClient,
  jsonProvider: JsonContentProvider,
  msg: { op_id?: string; git_oid?: string; repository?: string }
): Promise<void> {
  try {
    // Fetch the node details from the service.
    let details: any;
    if (msg.git_oid) {
      const resp = await client.request(
        { ResolveObject: { repository: msg.repository, oid: msg.git_oid } },
        { timeoutMs: NON_OPEN_TIMEOUT_MS }
      );
      // A service Error envelope must SURFACE as an error, never be opened as
      // a JSON document of the error object.
      if (resp && resp.Error !== undefined) throw new Error(String(resp.Error));
      details = resp?.Ok ?? resp;
    } else if (msg.op_id) {
      const resp = await client.request(
        { GetNodeDetails: { op_id: msg.op_id } },
        { timeoutMs: NON_OPEN_TIMEOUT_MS }
      );
      if (resp && resp.Error !== undefined) throw new Error(String(resp.Error));
      details = resp?.Ok ?? resp;
    } else {
      return;
    }

    // Pretty-format the record as JSON (4-space indent). For whitelisted keys
    // whose value is itself a JSON-serialized string (e.g. tool input/output),
    // parse it so it renders as nested JSON rather than an escaped string.
    const json = JSON.stringify(parseNestedJson(details), null, 4);

    // Open a read-only JSON document via the virtual `editchain-json:` scheme.
    // Content-provider documents are read-only by default. The `.json`
    // extension makes VS Code apply the JSON language mode (syntax highlighting
    // + formatting) instead of plain text.
    const uri = vscode.Uri.parse(
      `editchain-json:${msg.op_id ?? msg.git_oid ?? 'node'}.json`
    );
    jsonProvider.setContent(uri.toString(), json);
    const doc = await vscode.workspace.openTextDocument(uri);
    await vscode.window.showTextDocument(doc, { preview: true });
  } catch (e) {
    vscode.window.showErrorMessage(`EditChain: failed to open JSON editor: ${String(e)}`);
  }
}

/**
 * Recursively parse string values that are themselves JSON-serialized, so they
 * render as nested JSON rather than escaped strings.
 *
 * For whitelisted keys (e.g. `summary`, `body` — which can hold tool input /
 * output) and any other string that parses as JSON, the string is replaced with
 * its parsed value. Non-JSON strings and all other values pass through unchanged.
 */
function parseNestedJson(value: any): any {
  // Keys whose string values are commonly JSON-serialized payloads.
  const jsonKeys = new Set(['summary', 'body', 'content', 'input', 'output', 'result']);
  if (Array.isArray(value)) {
    return value.map(parseNestedJson);
  }
  if (value && typeof value === 'object') {
    const out: any = {};
    for (const [k, v] of Object.entries(value)) {
      out[k] = jsonKeys.has(k) ? tryParseJsonString(v) : parseNestedJson(v);
    }
    return out;
  }
  return value;
}

/** If `v` is a string that parses as JSON, return the parsed value; else `v`. */
function tryParseJsonString(v: any): any {
  if (typeof v !== 'string') return v;
  const trimmed = v.trim();
  if (!trimmed) return v;
  // Only attempt to parse if it looks like a JSON value (object, array, or
  // scalar literal), to avoid mangling ordinary prose.
  if (!/^[{[\-0-9tfn"]/.test(trimmed)) return v;
  try {
    return JSON.parse(trimmed);
  } catch {
    return v;
  }
}

/**
 * A read-only text document content provider for node detail JSON.
 *
 * Documents served under the `editchain-json:` scheme are read-only (content
 * providers cannot be edited), so the user sees a full pretty-formatted JSON
 * record without being able to modify it.
 */
class JsonContentProvider implements vscode.TextDocumentContentProvider {
  private contents = new Map<string, string>();

  /** Set (or update) the content for a document URI. */
  setContent(uri: string, content: string): void {
    this.contents.set(uri, content);
  }

  provideTextDocumentContent(uri: vscode.Uri): string {
    return this.contents.get(uri.toString()) ?? '';
  }
}

/** Build the single history panel's webview HTML (the Rust/WASM history view).
 *
 * This is the ONLY panel the extension opens. The page is the EXACT production
 * scaffold (media/main.css + media/gpu-preview/gpu-preview.css and the
 * controls/rows/gpu chrome markup) with media/rust-history/loader.js as its
 * ONLY script: the loader initializes the wasm-bindgen module and calls the
 * Rust shell's startHistoryView(), which owns the full runtime — Activity/Raw
 * profile switching, virtual paging (PAGE=500), FindInHistory search/nav,
 * loading/error, work-unit/bundle/promotion rows, row selection/keyboard/
 * disclosure, raw JSON routing, five responsive columns, accessibility,
 * resize, and the wgpu canvas under #gpu-canvas-host. Production media/main.js
 * and media/gpu-preview/bootstrap.js are deliberately NOT loaded here; they
 * remain only as the offscreen CPU/SVG parity-reference side of the harness
 * oracle (test/harness/index.html + gpu.html).
 *
 * The CSP keeps the production policy's shape and permits no network or worker
 * access. Its script-src additionally allows 'wasm-unsafe-eval' for
 * WebAssembly instantiation, while connect-src is restricted to the
 * extension's own webview resource origin so the loader can fetch the local
 * .wasm bytes next to media/rust-history/loader.js.
 */
function getHtml(context: vscode.ExtensionContext, webview: vscode.Webview): string {
  const rustLoaderUri = webview.asWebviewUri(
    vscode.Uri.joinPath(context.extensionUri, 'media', 'rust-history', 'loader.js')
  );
  const mainStyleUri = webview.asWebviewUri(
    vscode.Uri.joinPath(context.extensionUri, 'media', 'main.css')
  );
  const styleUri = webview.asWebviewUri(
    vscode.Uri.joinPath(context.extensionUri, 'media', 'gpu-preview', 'gpu-preview.css')
  );
  const cspSource = webview.cspSource;
  // The scaffold mirrors test/harness/rust.html exactly: the SAME production
  // rust-history loader initializes the wasm module and starts the Rust shell
  // inside VS Code and in the harness, against the same controls/rows/gpu
  // chrome. The body carries only the treatment and requested backend; the
  // loader resolves the wasm URL relative to its own module location, so no
  // glue/wasm URI data attributes are needed.
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${cspSource} 'unsafe-inline'; script-src ${cspSource} 'wasm-unsafe-eval'; connect-src ${cspSource};">
<title>EditChain History</title>
<link rel="stylesheet" href="${mainStyleUri}">
<link rel="stylesheet" href="${styleUri}">
</head>
<body data-treatment="pulse" data-gpu-backend="auto">
<div id="gpu-toolbar" role="toolbar" aria-label="Renderer status">
<span id="gpu-backend" data-backend="auto">backend: detecting</span>
<span id="gpu-status">idle</span>
</div>
<div id="controls" role="group" aria-label="History controls">
<div id="profile-control" class="segmented" role="group" aria-label="History profile">
<button type="button" id="profile-activity" class="segmented-btn active" aria-pressed="true">Activity</button>
<button type="button" id="profile-raw" class="segmented-btn" aria-pressed="false">Raw</button>
</div>
<label class="visually-hidden" for="search">Search history</label>
<div id="search-control" class="search-control" role="group" aria-label="Find in chain">
<input id="search" type="text" placeholder="Search history… (Enter to search)">
<span id="search-counter" class="search-counter" role="status" aria-live="polite"></span>
<button type="button" id="search-prev" class="search-nav" title="Previous match (Shift+Enter)" aria-label="Previous match" hidden disabled><svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path fill="currentColor" d="M13.5 10.5 8 5.06 2.5 10.5l-1.06-1.06L8 2.94l6.56 6.5z"/></svg></button>
<button type="button" id="search-next" class="search-nav" title="Next match (Enter)" aria-label="Next match" hidden disabled><svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path fill="currentColor" d="M8 13.06 1.44 6.56 2.5 5.5 8 10.94l5.5-5.44 1.06 1.06z"/></svg></button>
</div>
</div>
<div id="layout">
<div id="rows"></div>
<div id="gpu-canvas-host" aria-hidden="true"></div>
</div>
<div id="status-live" class="visually-hidden" role="status" aria-live="polite"></div>
<div id="gpu-scroll">
<div id="gpu-rows" role="table" aria-label="Rust/WASM history renderer" aria-hidden="true"></div>
</div>
<div id="gpu-live" class="visually-hidden" role="status" aria-live="polite"></div>
<script type="module" src="${rustLoaderUri}"></script>
</body>
</html>`;
}

export function deactivate(): void {}
