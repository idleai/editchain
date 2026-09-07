// Node unit tests for the extension host Open/panel lifecycle race.
//
// Loads the COMPILED extension host entry (out/extension.js, built by `npm run
// compile`) against a stubbed `vscode` and a controllable fake `./stdioClient`
// (same approach as stdioClient.lifecycle.test.js), then drives the open
// command through fake panels to assert the stale-Open ownership invariant:
//   - a late Open response from a disposed/superseded panel is dropped: it
//     must not cache its body, clear a newer Open's pending state, or post
//     into a newer panel's webview;
//   - disposing the current panel invalidates its in-flight Open callbacks;
//   - a stale dispose of a panel that is no longer current must not clear a
//     newer panel's state (so command reuse never issues a duplicate Open);
//   - only the current Open's Ok response is cached/replayed; an Open Error
//     surfaces without `ready` and command reuse retries.
//
// Run: node --test test/harness/extension.lifecycle.test.js
'use strict';

const { after, test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');

const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'editchain-extension-test-'));
after(() => fs.rmSync(tmpDir, { recursive: true, force: true }));

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
// Let any queued promise microtasks (and one macrotask) run.
const flush = () => sleep(0);

const uri = (s) => ({ toString: () => s, fsPath: s.replace(/^file:\/\//, '') });

// Stub modules are written to disk so the compiled extension.js can `require`
// them through the normal resolution hook.
const fakeVscodePath = path.join(tmpDir, 'fake-vscode.js');
const fakeStdioPath = path.join(tmpDir, 'fake-stdio-client.js');

function writeFakeVscode() {
  fs.writeFileSync(
    fakeVscodePath,
    `'use strict';
const providers = [];
const executedCommands = [];
const warnings = [];
module.exports = {
  __esModule: true,
  workspace: {
    workspaceFolders: [{ uri: ${uri.toString()}('/ws') }],
    getConfiguration: () => ({ get: (_key, def) => def }),
    registerTextDocumentContentProvider: (scheme, provider) => {
      providers.push({ scheme, provider });
      return { dispose() {} };
    },
    openTextDocument: async () => ({}),
  },
  window: {
    createOutputChannel: () => ({ appendLine() {} }),
    createStatusBarItem: () => ({ text: '', command: null, tooltip: null, show() {}, hide() {}, dispose() {} }),
    createWebviewPanel: () => { throw new Error('createWebviewPanel must be intercepted by the test harness'); },
    showTextDocument: async () => ({}),
    showErrorMessage: () => undefined,
    showWarningMessage: async (message) => { warnings.push(message); },
  },
  commands: {
    registerCommand: () => { throw new Error('registerCommand must be intercepted by the test harness'); },
    executeCommand: async (...args) => { executedCommands.push(args); },
  },
  Uri: {
    parse: (s) => ${uri.toString()}(s),
    joinPath: (...p) => ${uri.toString()}(p.map(String).join('/')),
    from: (parts) => (${uri.toString()})(parts.scheme + '://' + (parts.authority || '') + (parts.path || '')),
  },
  ViewColumn: { One: 1 },
  StatusBarAlignment: { Left: 1 },
  __providers: providers,
  __executedCommands: executedCommands,
  __warnings: warnings,
};
`
  );
}

// The fake StdioClient module records every instance and exposes a controllable
// `request`: Open calls return promises the test resolves by hand, everything
// else resolves immediately.
function writeFakeStdioClient() {
  fs.writeFileSync(
    fakeStdioPath,
    `'use strict';
const instances = [];
class FakeStdioClient {
  constructor() {
    this.running = false;
    this.openRequests = [];
    this.requests = [];
    instances.push(this);
  }
  stop() {}
  setLog() {}
  isRunning() { return this.running; }
  ensureStarted() { this.running = true; }
  request(body, opts) {
    const rec = { body, opts, resolve: null, promise: null };
    rec.promise = new Promise((resolve) => { rec.resolve = resolve; });
    if (body && body.Open) {
      this.openRequests.push(rec);
    } else {
      this.requests.push(rec);
      rec.resolve(this.nextResponse || { Ok: {} });
      this.nextResponse = null;
    }
    return rec.promise;
  }
  setMessageHandler() {}
}
module.exports = {
  StdioClient: FakeStdioClient,
  resolveServicePath: () => '/fake/service',
  __instances: instances,
};
`
  );
}

function fakePanel(index, registeredCommands) {
  const messages = [];
  const handlers = {};
  const panel = {
    index,
    webview: {
      messages,
      postMessage(msg) { messages.push(msg); return Promise.resolve(true); },
      html: '',
      cspSource: 'vscode-webview://csp',
      asWebviewUri: (u) => uri('vscode-resource://' + u.toString()),
      onDidReceiveMessage: (cb) => { handlers.message = cb; return { dispose() {} }; },
    },
    reveal() {},
    onDidDispose: (cb) => { handlers.dispose = cb; return { dispose() {} }; },
    onDidChangeViewState: (cb) => { handlers.viewState = cb; return { dispose() {} }; },
    handlers,
  };
  return panel;
}

/** Announce that one concrete webview renderer context has installed its listener. */
async function rendererReady(panel, instanceId = 'renderer-' + panel.index) {
  await panel.handlers.message({ type: 'webviewReady', instanceId });
  await flush();
}

// Install stubs, load the compiled extension fresh (module globals reset per
// test), and activate it against a fake context. Returns the pieces the tests
// drive: the open command, the fake panels, the fake client, and status item.
function loadExtension() {
  writeFakeVscode();
  writeFakeStdioClient();
  const origResolveFilename = Module._resolveFilename;
  Module._resolveFilename = function (request, ...rest) {
    if (request === 'vscode') return fakeVscodePath;
    if (request === './stdioClient') return fakeStdioPath;
    return origResolveFilename.call(this, request, ...rest);
  };

  const panels = [];
  const registeredCommands = {};
  const statusItem = { text: '', command: null, tooltip: null, hideCalls: 0, showCalls: 0, show() { this.showCalls++; }, hide() { this.hideCalls++; } };
  const extPath = path.join(__dirname, '..', '..', 'out', 'extension.js');
  // Clear module caches BEFORE re-requiring so each test gets fresh module
  // globals AND the mutated stub instance below is what the extension sees.
  delete require.cache[extPath];
  delete require.cache[fakeVscodePath];
  delete require.cache[fakeStdioPath];
  const fakeVscode = require(fakeVscodePath);
  fakeVscode.window.createWebviewPanel = (type, title, column, options) => {
    const panel = fakePanel(panels.length, registeredCommands);
    panel.options = options;
    panels.push(panel);
    return panel;
  };
  fakeVscode.commands.registerCommand = (name, handler) => {
    registeredCommands[name] = handler;
    return { dispose() {} };
  };
  fakeVscode.window.createStatusBarItem = () => statusItem;

  const ext = require(extPath);
  const context = { subscriptions: [], extensionUri: uri('file:///ext') };
  ext.activate(context);
  const client = require(fakeStdioPath).__instances[0];

  return {
    open: registeredCommands['editchain-history.open'],
    panels,
    client,
    statusItem,
    vscode: fakeVscode,
  };
}

test('late Open response from a superseded panel is dropped', async () => {
  const env = loadExtension();

  env.open(); // panel A starts its Open
  assert.equal(env.panels.length, 1);
  const panelA = env.panels[0];
  const openA = env.client.openRequests[0];
  await rendererReady(panelA);

  // Panel A is disposed, then panel B is created and starts its own Open.
  panelA.handlers.dispose();
  env.open();
  assert.equal(env.panels.length, 2);
  const panelB = env.panels[1];
  const openB = env.client.openRequests[1];
  await rendererReady(panelB);

  // A's response lands LATE, after B's Open is already pending. It must be
  // dropped: it may not post into A (dead) or B, and must not cache A's body.
  openA.resolve({ Ok: { workspace: 'A' } });
  await flush();
  assert.equal(panelA.webview.messages.length, 0, 'A is disposed: no delivery to A');
  assert.equal(panelB.webview.messages.length, 0, 'A stale body must not reach B');

  // B becoming active while its own Open is pending must wait for B's
  // authoritative body — never replay A's (the pre-fix bug).
  panelB.handlers.viewState({ webviewPanel: { active: true } });
  assert.equal(panelB.webview.messages.length, 0, 'reveal must wait for B own Open');

  // B's authoritative response lands and is the ONLY body cached/replayed.
  openB.resolve({ Ok: { workspace: 'B' } });
  await flush();
  assert.deepEqual(panelB.webview.messages[0], { id: 'open', body: { Ok: { workspace: 'B' } } });
  assert.deepEqual(panelB.webview.messages[1], { id: 'ready' });

  panelB.handlers.viewState({ webviewPanel: { active: true } });
  assert.equal(panelB.webview.messages.length, 2, 'retained reveal must not reset cached rows');

  // A genuinely recreated webview renderer context announces a NEW identity
  // after its listener exists and gets the authoritative state exactly once.
  await rendererReady(panelB, 'renderer-B-recreated');
  assert.deepEqual(panelB.webview.messages[2], { id: 'open', body: { Ok: { workspace: 'B' } } });
  assert.deepEqual(panelB.webview.messages[3], { id: 'ready' });
  await rendererReady(panelB, 'renderer-B-recreated');
  assert.equal(panelB.webview.messages.length, 4, 'same renderer identity is not replayed twice');
});

test('stale dispose and stale error must not clear a newer panel state', async () => {
  const env = loadExtension();

  env.open(); // panel A, Open A pending
  const panelA = env.panels[0];
  const openA = env.client.openRequests[0];
  await rendererReady(panelA);

  // A is disposed while current, then B is created and starts its Open.
  panelA.handlers.dispose();
  assert.equal(env.statusItem.hideCalls, 1, 'current-panel dispose hides the status item');
  env.open();
  const panelB = env.panels[1];
  const openB = env.client.openRequests[1];
  await rendererReady(panelB);

  // A duplicate/late dispose delivery for the STALE panel A must be a no-op:
  // it must not clear B's in-flight state nor hide B's status item.
  panelA.handlers.dispose();
  assert.equal(env.statusItem.hideCalls, 1, 'stale dispose must not hide the status item');

  // A stale ERROR response must also be dropped: it must not clear B's pending
  // flag or post an error anywhere.
  openA.resolve({ Error: 'boom' });
  await flush();
  assert.equal(panelA.webview.messages.length, 0, 'stale error must not post to A');
  assert.equal(panelB.webview.messages.length, 0, 'stale error must not reach B');

  // Command reuse while B's Open is pending must NOT issue a duplicate Open —
  // this only holds if neither the stale dispose nor the stale error cleared
  // openPending.
  env.open();
  assert.equal(env.client.openRequests.length, 2, 'no duplicate Open while one is pending');

  // B's view-state change while its Open is pending waits (no reveal fallback,
  // which a cleared openPending would have triggered).
  panelB.handlers.viewState({ webviewPanel: { active: true } });
  assert.equal(panelB.webview.messages.length, 0, 'reveal must wait for B own Open');

  // B's authoritative response still lands normally.
  openB.resolve({ Ok: { workspace: 'B' } });
  await flush();
  assert.deepEqual(panelB.webview.messages[0], { id: 'open', body: { Ok: { workspace: 'B' } } });
  assert.deepEqual(panelB.webview.messages[1], { id: 'ready' });
});

test('disposing the current panel invalidates its in-flight Open', async () => {
  const env = loadExtension();

  env.open(); // panel A, Open A pending
  const panelA = env.panels[0];
  const openA = env.client.openRequests[0];
  await rendererReady(panelA);

  // Disposing the CURRENT panel invalidates its outstanding Open: a late Ok
  // response must not be cached for replay by a later panel.
  panelA.handlers.dispose();
  openA.resolve({ Ok: { workspace: 'A' } });
  await flush();
  assert.equal(panelA.webview.messages.length, 0, 'disposed panel must not receive its late Open');

  // A later panel starts from a clean slate: its reveal must not replay A.
  env.open();
  const panelB = env.panels[1];
  const openB = env.client.openRequests[1];
  await rendererReady(panelB);
  panelB.handlers.viewState({ webviewPanel: { active: true } });
  assert.equal(panelB.webview.messages.length, 0, 'new panel must not replay the disposed panel body');

  openB.resolve({ Ok: { workspace: 'B' } });
  await flush();
  assert.deepEqual(panelB.webview.messages[0], { id: 'open', body: { Ok: { workspace: 'B' } } });
  assert.deepEqual(panelB.webview.messages[1], { id: 'ready' });
});

test('Open Error surfaces without ready and command reuse retries', async () => {
  const env = loadExtension();

  env.open(); // panel A, Open A pending
  const panelA = env.panels[0];
  const openA = env.client.openRequests[0];
  await rendererReady(panelA);

  // Error on the CURRENT Open: surfaced to the webview, no `ready`, no body
  // cached (so command reuse retries instead of replaying).
  openA.resolve({ Error: 'boom' });
  await flush();
  assert.deepEqual(panelA.webview.messages, [{ id: 'open', body: { Error: 'boom' } }]);

  env.open(); // reuse: no successful body -> retry with a fresh Open
  assert.equal(env.client.openRequests.length, 2, 'command reuse retries after Error');
  const openA2 = env.client.openRequests[1];
  openA2.resolve({ Ok: { workspace: 'A' } });
  await flush();
  assert.deepEqual(panelA.webview.messages[1], { id: 'open', body: { Ok: { workspace: 'A' } } });
  assert.deepEqual(panelA.webview.messages[2], { id: 'ready' });
});

test('history panel retains its bounded renderer context across raw JSON navigation', async () => {
  const env = loadExtension();

  env.open();
  const panel = env.panels[0];
  assert.equal(
    panel.options.retainContextWhenHidden,
    true,
    'VS Code must preserve the cached history DOM while a raw JSON editor covers it'
  );

  await rendererReady(panel);
  env.client.openRequests[0].resolve({ Ok: { workspace: 'A' } });
  await flush();
  assert.equal(panel.webview.messages.length, 2);

  panel.handlers.viewState({ webviewPanel: { active: false } });
  panel.handlers.viewState({ webviewPanel: { active: true } });
  assert.equal(panel.webview.messages.length, 2, 'Back must display retained rows without replaying Open');
  assert.equal(env.client.openRequests.length, 1, 'Back must not rebuild the workspace');
});

test('openDiff resolves service content into VS Code native virtual documents', async () => {
  const env = loadExtension();
  env.open();
  const panel = env.panels[0];
  const change = {
    source: 'git',
    path: 'src/lib.rs',
    status: 'modified',
    repository: '42',
    repository_path: 'src/lib.rs',
    commit_oid: '0123456789012345678901234567890123456789',
    old_oid: '1111111111111111111111111111111111111111',
    new_oid: '2222222222222222222222222222222222222222',
    old_mode: 'blob',
    new_mode: 'blob',
    binary: false,
    partial: false,
  };
  env.client.nextResponse = {
    Ok: {
      path: 'src/lib.rs',
      status: 'modified',
      binary: false,
      partial: false,
      before: 'fn old() {}\n',
      after: 'fn new() {}\n',
    },
  };

  await panel.handlers.message({ type: 'openDiff', change });
  await flush();
  assert.deepEqual(env.client.requests[0].body, { GetFileDiff: { change } });
  assert.equal(env.client.requests[0].opts.timeoutMs, 120_000);

  const command = env.vscode.__executedCommands[0];
  assert.equal(command[0], 'vscode.diff');
  assert.match(command[1].toString(), /^editchain-diff:\/\/1-before\/src\/lib\.rs$/);
  assert.match(command[2].toString(), /^editchain-diff:\/\/1-after\/src\/lib\.rs$/);
  assert.equal(command[3], 'src/lib.rs (Git)');
  assert.deepEqual(command[4], { preview: true });

  const registration = env.vscode.__providers.find((entry) => entry.scheme === 'editchain-diff');
  assert.ok(registration, 'the read-only diff content provider is registered');
  assert.equal(registration.provider.provideTextDocumentContent(command[1]), 'fn old() {}\n');
  assert.equal(registration.provider.provideTextDocumentContent(command[2]), 'fn new() {}\n');
});
