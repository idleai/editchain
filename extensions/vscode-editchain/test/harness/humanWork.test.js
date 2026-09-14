'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');

async function harness() {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-human-host-'));
  const captures = [], clients = [], contexts = [], outboxes = [], statuses = [], logs = [];
  const commands = new Map(), settings = new Map();
  const report = { calls: 0, release: undefined, responses: [] };
  report.promise = new Promise(resolve => { report.release = resolve; });
  let listener;
  const disposable = () => ({ dispose() {} });
  const uri = { scheme: 'file', fsPath: '/workspace', toString: () => 'file:///workspace' };
  const configuration = { get: (key, fallback) => settings.has(key) ? settings.get(key) : fallback,
    async update(key, value) { settings.set(key, value); listener({ affectsConfiguration: name => name === 'editchain-history.tracking' }); } };
  const vscode = {
    ConfigurationTarget: { Workspace: 1 }, StatusBarAlignment: { Left: 1 },
    workspace: { isTrusted: true, workspaceFolders: [{ uri, index: 0 }],
      getConfiguration: () => configuration, registerTextDocumentContentProvider: disposable,
      onDidChangeWorkspaceFolders: disposable, onDidChangeConfiguration: callback => { listener = callback; return disposable(); },
      openTextDocument: async uri => uri },
    Uri: { parse: value => value },
    window: { createStatusBarItem: () => { const status = { show() {}, dispose() {} }; statuses.push(status); return status; },
      showTextDocument: async () => {}, showErrorMessage: async message => logs.push(message) },
    commands: { registerCommand: (name, callback) => { commands.set(name, callback); return disposable(); } },
  };
  class Capture {
    constructor(_folder, _dwell, _bytes, emit, identity) { this.identity = identity; this.emit = emit; captures.push(this); }
    checkpoint() {} dispose() { this.stopped = true; }
  }
  class Outbox {
    constructor(_directory, workspace, chain, send) { this.send = send; this.workspace = workspace; this.chain = chain; outboxes.push(this); }
    push(event) { this.delivery = this.send([Buffer.from(JSON.stringify({ RecordEditorEvents: { events: [event] } }))]); return true; }
    async flush() { await this.delivery; return true; } async stop() {}
  }
  class Client {
    constructor() { this.tail = Promise.resolve(); this.requests = []; clients.push(this); }
    setLog() {} ensureStarted() {} stop() { this.stopped = true; }
    requestJson(body) { return this.request(JSON.parse(Buffer.concat(body))); }
    request(body) {
      this.requests.push(body);
      this.tail = this.tail.then(async () => {
        if (body.GetHumanWork) { report.calls++; await report.promise; return report.responses.shift() ?? { Ok: { schema: 1, files: [], limitations: [] } }; }
        return { Ok: { observed_ms: 1, repositories: [], ack: body.RecordEditorEvents?.events.map(event => [event.session, event.sequence]) } };
      });
      return this.tail;
    }
  }
  const filename = require.resolve('../../out/humanWork');
  delete require.cache[filename];
  const original = Module._load;
  Module._load = function(name, ...args) {
    if (name === 'vscode') return vscode;
    if (name === './editorCapture') return { EditorCapture: Capture };
    if (name === './editorOutbox') return { EditorOutbox: Outbox };
    if (name === './stdioClient') return { StdioClient: Client, resolveServicePath: () => '/service' };
    if (name === './editorContext') return { observeEditorContext: (_capture, request) => { contexts.push(request); return disposable(); } };
    return original.call(this, name, ...args);
  };
  let HumanWorkHost;
  try { ({ HumanWorkHost } = require(filename)); } finally { Module._load = original; }
  const context = () => ({ subscriptions: [], storageUri: { fsPath: directory }, globalStorageUri: { fsPath: directory } });
  return { captures, clients, contexts, outboxes, statuses, logs, commands, report, vscode,
    create: () => new HumanWorkHost(context(), { appendLine: line => logs.push(line), show() { logs.push('output shown'); } }),
    cleanup: () => fs.rm(directory, { recursive: true, force: true }) };
}

test('tracking starts by default, resumes once, and keeps its identity through host reloads', async () => {
  const env = await harness();
  const { captures, commands } = env;
  const host = env.create();
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
    reloaded = env.create();
    await reloaded.lifecycle;
    assert.equal(captures.length, 3);
    assert.deepEqual(captures[2].identity, captures[0].identity);
  } finally {
    await host.stop();
    if (reloaded) await reloaded.stop();
    await env.cleanup();
  }
});

test('slow coverage cannot block recording or Git context; repeated report clicks share one worker', async () => {
  const env = await harness();
  const host = env.create();
  let pending;
  try {
    await host.lifecycle;
    pending = env.commands.get('editchain-history.humanWork')();
    assert.equal(env.commands.get('editchain-history.humanWork')(), pending);
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.report.calls, 1);
    env.captures[0].emit({ session: 'test', sequence: 1, event: { type: 'human_edit', change: 1 } });
    let completed = false;
    void Promise.all([env.outboxes[0].delivery, env.contexts[0]()]).then(() => { completed = true; });
    await new Promise(resolve => setTimeout(resolve, 50));
    assert.ok(completed, 'live capture and context complete while coverage is still blocked');
    const reportClient = env.clients.find(client => client.requests.some(request => request.GetHumanWork));
    assert.ok(!reportClient.requests.some(request => request.RecordEditorEvents || request.GetEditorContext));
    env.report.release();
    await pending;
    assert.ok(reportClient.stopped, 'one-shot report worker releases its resources');
    assert.ok(env.clients.filter(client => client !== reportClient).every(client => !client.stopped));
  } finally {
    env.report.release(); await pending; await host.stop(); await env.cleanup();
  }
});

test('tracking status shows runtime diagnostics without replaying history or flushing capture', async () => {
  const env = await harness();
  const host = env.create();
  try {
    await host.lifecycle;
    await env.commands.get(env.statuses[0].command)();
    assert.equal(env.report.calls, 0);
    assert.ok(env.clients.every(client => client.requests.length === 0));
    assert.ok(env.logs.includes('output shown'));
    assert.ok(env.logs.some(line => line.includes('[capture] Runtime')));
  } finally { await host.stop(); await env.cleanup(); }
});

test('coverage retries a changing source snapshot on its independent worker', async () => {
  const env = await harness(), host = env.create();
  try {
    await host.lifecycle;
    env.report.responses.push({ Error: { code: 'stale_snapshot', message: 'new capture arrived' } });
    env.report.release();
    const result = await env.commands.get('editchain-history.humanWork')();
    assert.equal(result.schema, 1);
    assert.equal(env.report.calls, 2);
    assert.equal(env.clients.filter(client => client.requests.some(request => request.GetHumanWork)).length, 1);
    assert.ok(env.clients.filter(client => !client.requests.some(request => request.GetHumanWork)).every(client => !client.stopped));
  } finally { env.report.release(); await host.stop(); await env.cleanup(); }
});

test('persistent coverage errors settle after bounded retries without waiting for notification dismissal', async () => {
  const env = await harness(), host = env.create();
  let notified, dismiss, pending;
  const shown = new Promise(resolve => { notified = resolve; });
  const notification = new Promise(resolve => { dismiss = resolve; });
  env.vscode.window.showErrorMessage = message => { env.logs.push(message); notified(); return notification; };
  try {
    await host.lifecycle;
    env.report.responses.push(...Array.from({ length: 3 }, () => ({ Error: { code: 'stale_snapshot' } })));
    env.report.release();
    let settled = false;
    pending = env.commands.get('editchain-history.humanWork')().then(result => { settled = true; return result; });
    await shown;
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.report.calls, 3);
    assert.ok(settled, 'an undismissed notification cannot keep the command pending');
    assert.equal(await pending, undefined);
    const next = await env.commands.get('editchain-history.humanWork')();
    assert.equal(next.schema, 1, 'another report can start while the notification is still visible');
  } finally { dismiss(); env.report.release(); await pending; await host.stop(); await env.cleanup(); }
});

test('stopping the extension terminates the coverage worker without disturbing report completion', async () => {
  const env = await harness();
  const host = env.create();
  let pending;
  try {
    await host.lifecycle;
    pending = env.commands.get('editchain-history.humanWork')();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.report.calls, 1);
    await host.stop();
    assert.ok(env.clients.every(client => client.stopped));
    env.report.release();
    assert.equal(await pending, undefined, 'a disposed extension does not publish the completed report');
  } finally { env.report.release(); await pending; await host.stop(); await env.cleanup(); }
});
