import * as vscode from 'vscode';
import { resolveServicePath, StdioClient } from './stdioClient';

let clientStarted = false;
// The single history panel. Reused across `open` invocations so we never create
// two webviews of the same type (which races VS Code's service-worker
// registration and can throw "Could not register service worker").
let historyPanel: vscode.WebviewPanel | undefined = undefined;
// Output channel for debugging the service bridge and panel lifecycle.
let output: vscode.OutputChannel | undefined = undefined;

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
  // be edited), which is exactly what we want for node detail views.
  const jsonProvider = new JsonContentProvider();
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider('editchain-json', jsonProvider)
  );

  const openCommand = vscode.commands.registerCommand('editchain-history.open', () => {
    openHistoryView(context, client, jsonProvider);
  });
  context.subscriptions.push(openCommand);
}

/**
 * Open (or reveal) the history explorer webview panel.
 */
function openHistoryView(
  context: vscode.ExtensionContext,
  client: StdioClient,
  jsonProvider: JsonContentProvider
): void {
  // Reuse an existing panel if one is still open, so we never create two
  // webviews of the same type (which races service-worker registration).
  if (historyPanel) {
    output?.appendLine('[openHistoryView] reusing existing panel');
    historyPanel.reveal(vscode.ViewColumn.One);
    return;
  }
  output?.appendLine('[openHistoryView] creating new panel');

  const panel = vscode.window.createWebviewPanel(
    'editchainHistory',
    'EditChain History',
    vscode.ViewColumn.One,
    {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(context.extensionUri, 'media')],
    }
  );
  historyPanel = panel;
  // Clear the reference when the panel is closed so a later `open` creates a
  // fresh one instead of reusing a disposed webview.
  panel.onDidDispose(() => {
    output?.appendLine('[panel] disposed');
    if (historyPanel === panel) {
      historyPanel = undefined;
    }
  });

  // When the panel becomes visible again (e.g. after navigating to a JSON
  // editor and back), ask the webview to re-render. The webview's JS state is
  // preserved across backgrounding, but its DOM can be stale/cleared while
  // hidden, so it needs a fresh render pass on reveal.
  panel.onDidChangeViewState((e) => {
    output?.appendLine('[panel] view state changed, active=' + e.webviewPanel.active);
    if (e.webviewPanel.active) {
      panel.webview.postMessage({ id: 'reveal' });
    }
  });

  // Start the service if not already running.
  if (!clientStarted) {
    client.start(resolveServicePath());
    clientStarted = true;
  }

  // Forward webview -> service.
  panel.webview.onDidReceiveMessage(async (msg) => {
    // Intercept the "open JSON editor" request from the webview: fetch the
    // node's details from the service and open a read-only JSON editor instead
    // of forwarding to the service and rendering in the webview.
    if (msg.type === 'openJson') {
      output?.appendLine('[webview] openJson request');
      await openJsonEditor(client, jsonProvider, msg);
      return;
    }
    if (msg.type === 'log') {
      output?.appendLine('[webview] ' + msg.text);
      return;
    }
    try {
      const resp = await client.request(msg.body);
      panel.webview.postMessage({ id: msg.id, body: resp });
    } catch (e) {
      panel.webview.postMessage({ id: msg.id, body: { error: String(e) } });
    }
  });

  // Forward service -> webview (unsolicited updates).
  client.setMessageHandler((msg) => {
    panel.webview.postMessage(msg);
  });

  panel.webview.html = getHtml(context, panel.webview);

  // Open the workspace on load, THEN tell the webview to load its first window.
  const workspacePath =
    vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '';
  const chainDir = vscode.workspace
    .getConfiguration('editchain-history')
    .get<string>('chainDir', '.editchain');
  client.request({
    Open: { workspace_path: workspacePath, chain_dir: chainDir },
  }).then((resp) => {
    output?.appendLine('[openHistoryView] sending open message');
    panel.webview.postMessage({ id: 'open', body: resp });
    // After open succeeds, ask the webview to fetch the first window.
    panel.webview.postMessage({ id: 'ready' });
  }).catch((e) => {
    output?.appendLine('[openHistoryView] open failed: ' + String(e));
    panel.webview.postMessage({ id: 'open', body: { error: String(e) } });
  });
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
  msg: { op_id?: string; git_oid?: string; repository?: number }
): Promise<void> {
  try {
    // Fetch the node details from the service.
    let details: any;
    if (msg.git_oid) {
      const resp = await client.request({
        ResolveObject: { repository: msg.repository, oid: msg.git_oid },
      });
      details = resp?.Ok ?? resp;
    } else if (msg.op_id) {
      const resp = await client.request({ GetNodeDetails: { op_id: msg.op_id } });
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

/** Build the webview HTML. */
function getHtml(context: vscode.ExtensionContext, webview: vscode.Webview): string {
  const scriptUri = webview.asWebviewUri(
    vscode.Uri.joinPath(context.extensionUri, 'media', 'main.js')
  );
  const styleUri = webview.asWebviewUri(
    vscode.Uri.joinPath(context.extensionUri, 'media', 'main.css')
  );
  const cspSource = webview.cspSource;
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${cspSource} 'unsafe-inline'; script-src ${cspSource};">
<title>EditChain History</title>
<link rel="stylesheet" href="${styleUri}">
</head>
<body>
<div id="controls">
<input id="search" type="text" placeholder="Search history… (Enter to search)">
<label class="toggle"><input type="checkbox" id="hideSubmodules"> Show git submodules</label>
<label class="toggle"><input type="checkbox" id="hideSystem"> Show messages only</label>
</div>
<div id="layout">
<div id="rows"></div>
<div id="detail"></div>
</div>
<script src="${scriptUri}"></script>
</body>
</html>`;
}

export function deactivate(): void {}