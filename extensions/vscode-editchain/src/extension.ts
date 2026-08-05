import * as vscode from 'vscode';
import { resolveServicePath, StdioClient } from './stdioClient';

let clientStarted = false;

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
  const output = vscode.window.createOutputChannel('EditChain History');
  context.subscriptions.push(output);
  client.setLog((line) => output.appendLine(line));

  const openCommand = vscode.commands.registerCommand('editchain-history.open', () => {
    openHistoryView(context, client);
  });
  context.subscriptions.push(openCommand);
}

/**
 * Open (or reveal) the history explorer webview panel.
 */
function openHistoryView(context: vscode.ExtensionContext, client: StdioClient): void {
  const panel = vscode.window.createWebviewPanel(
    'editchainHistory',
    'EditChain History',
    vscode.ViewColumn.One,
    {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(context.extensionUri, 'media')],
    }
  );

  // Start the service if not already running.
  if (!clientStarted) {
    client.start(resolveServicePath());
    clientStarted = true;
  }

  // Forward webview -> service.
  panel.webview.onDidReceiveMessage(async (msg) => {
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
    panel.webview.postMessage({ id: 'open', body: resp });
    // After open succeeds, ask the webview to fetch the first window.
    panel.webview.postMessage({ id: 'ready' });
  }).catch((e) => {
    panel.webview.postMessage({ id: 'open', body: { error: String(e) } });
  });
}

/** Build the webview HTML. */
function getHtml(context: vscode.ExtensionContext, webview: vscode.Webview): string {
  const scriptUri = webview.asWebviewUri(
    vscode.Uri.joinPath(context.extensionUri, 'media', 'main.js')
  );
  const cspSource = webview.cspSource;
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${cspSource}; script-src ${cspSource};">
<title>EditChain History</title>
<style>
body { font-family: var(--vscode-font-family); color: var(--vscode-foreground); padding: 12px; }
#status { color: var(--vscode-descriptionForeground); margin-bottom: 8px; }
#controls { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
#search { flex: 1; padding: 6px; box-sizing: border-box; }
.toggle { display: flex; align-items: center; gap: 4px; color: var(--vscode-descriptionForeground); font-size: 0.9em; white-space: nowrap; }
#layout { display: flex; gap: 12px; height: calc(100vh - 120px); }
#rows { flex: 1.6; overflow-y: auto; display: flex; flex-direction: column; }
#detail { flex: 1; overflow-y: auto; border-left: 1px solid var(--vscode-panel-border); padding-left: 12px; }
.block-sep { font-weight: 700; color: var(--vscode-descriptionForeground); padding: 8px 4px 4px; border-bottom: 1px solid var(--vscode-panel-border); position: sticky; top: 0; background: var(--vscode-editor-background); z-index: 1; }
.row { display: grid; align-items: center; cursor: pointer; min-height: 34px; }
.row:hover { background: var(--vscode-list-hoverBackground); }
.graph-cell { line-height: 0; }
.text-cell { padding-left: 8px; overflow: hidden; }
.row .summary { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.row .meta { color: var(--vscode-descriptionForeground); font-size: 0.85em; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.detail-title { font-weight: 700; margin-bottom: 8px; }
.detail-body { white-space: pre-wrap; font-family: var(--vscode-editor-font-family); font-size: 0.9em; }
.detail-meta { color: var(--vscode-descriptionForeground); margin-top: 4px; font-size: 0.9em; word-break: break-all; }
</style>
</head>
<body>
<div id="status">Loading…</div>
<div id="controls">
<input id="search" type="text" placeholder="Search history… (Enter to search)">
<label class="toggle"><input type="checkbox" id="hideSubmodules"> Hide git submodules</label>
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