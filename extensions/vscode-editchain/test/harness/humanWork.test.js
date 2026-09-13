'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');

test('tracking starts by default, resumes once, and keeps its identity through host reloads', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-human-host-'));
  const captures = [], commands = new Map(), settings = new Map();
  let listener;
  const disposable = () => ({ dispose() {} });
  const uri = { scheme: 'file', fsPath: '/workspace', toString: () => 'file:///workspace' };
  const configuration = { get: (key, fallback) => settings.has(key) ? settings.get(key) : fallback,
    async update(key, value) { settings.set(key, value); listener({ affectsConfiguration: name => name === 'editchain-history.tracking' }); } };
  const vscode = {
    ConfigurationTarget: { Workspace: 1 }, StatusBarAlignment: { Left: 1 },
    workspace: { isTrusted: true, workspaceFolders: [{ uri, index: 0 }],
      getConfiguration: () => configuration, registerTextDocumentContentProvider: disposable,
      onDidChangeWorkspaceFolders: disposable, onDidChangeConfiguration: callback => { listener = callback; return disposable(); } },
    window: { createStatusBarItem: () => ({ show() {}, dispose() {} }) },
    commands: { registerCommand: (name, callback) => { commands.set(name, callback); return disposable(); } },
  };
  class Capture { constructor(_folder, _dwell, _bytes, _emit, identity) { this.identity = identity; captures.push(this); } dispose() { this.stopped = true; } }
  class Outbox { push() { return true; } async stop() {} }
  class Client { setLog() {} stop() {} }
  const filename = require.resolve('../../out/humanWork');
  delete require.cache[filename];
  const original = Module._load;
  Module._load = function(name, ...args) {
    if (name === 'vscode') return vscode;
    if (name === './editorCapture') return { EditorCapture: Capture };
    if (name === './editorOutbox') return { EditorOutbox: Outbox };
    if (name === './stdioClient') return { StdioClient: Client };
    if (name === './editorContext') return { observeEditorContext: disposable };
    return original.call(this, name, ...args);
  };
  let HumanWorkHost;
  try { ({ HumanWorkHost } = require(filename)); } finally { Module._load = original; }
  const context = () => ({ subscriptions: [], storageUri: { fsPath: directory }, globalStorageUri: { fsPath: directory } });
  const host = new HumanWorkHost(context(), { appendLine() {} });
  let reloaded;
  try {
    await host.lifecycle;
    assert.equal(captures.length, 1, 'no tracking setting is required');
    await commands.get('editchain-history.stopTracking')();
    assert.equal(captures.length, 1);
    assert.equal(captures[0].stopped, true);
    await commands.get('editchain-history.startTracking')();
    await host.lifecycle;
    assert.equal(captures.length, 2, 'command and configuration callback share one restart');
    assert.deepEqual(captures[1].identity, captures[0].identity);
    await host.stop();
    reloaded = new HumanWorkHost(context(), { appendLine() {} });
    await reloaded.lifecycle;
    assert.equal(captures.length, 3);
    assert.deepEqual(captures[2].identity, captures[0].identity);
  } finally {
    await host.stop();
    if (reloaded) await reloaded.stop();
    await fs.rm(directory, { recursive: true, force: true });
  }
});
