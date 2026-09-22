'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { archiveDay, archiveFileName } = require('../../out/historyArchive');
// A junction needs no symlink privilege on Windows; 'dir' is the POSIX default.
const linkDirectory = (target, link) => fs.symlink(target, link, process.platform === 'win32' ? 'junction' : 'dir');

async function harness(initial = {}) {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-human-host-'));
  const captures = [], clients = [], contexts = [], outboxes = [], statuses = [], logs = [];
  const commands = new Map(), settings = new Map(Object.entries(initial));
  const report = { calls: 0, release: undefined, responses: [] };
  report.promise = new Promise(resolve => { report.release = resolve; });
  let listener;
  const disposable = () => ({ dispose() {} });
  const uri = { scheme: 'file', fsPath: '/workspace', toString: () => 'file:///workspace' };
  const configuration = { get: (key, fallback) => settings.has(key) ? settings.get(key) : fallback,
    async update(key, value) {
      settings.set(key, value);
      const changed = `editchain-history.${key}`;
      listener({ affectsConfiguration: name => changed === name || changed.startsWith(`${name}.`) });
    } };
  const vscode = {
    authentication: { getSession: async () => undefined, onDidChangeSessions: disposable },
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
    constructor(_folder, _dwell, _bytes, emit, identity, userName, excluded) {
      this.identity = identity; this.userName = userName; this.emit = emit; this.excluded = excluded; captures.push(this);
    }
    setUserName(name) { this.userName = name; }
    checkpoint() {} dispose() { this.stopped = true; }
  }
  class Outbox {
    constructor(_directory, workspace, chain, send, _status, _delivered, _slow, observed) {
      this.send = send; this.workspace = workspace; this.chain = chain; this.observed = observed; outboxes.push(this);
    }
    push(event) {
      this.observed?.(this.workspace, event);
      this.delivery = this.send([Buffer.from(JSON.stringify({ RecordEditorEvents: { events: [event] } }))]);
      return true;
    }
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
  delete require.cache[require.resolve('../../out/humanAccount')];
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
  return { captures, clients, contexts, outboxes, statuses, logs, commands, report, vscode, directory, settings, configuration,
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

test('sign-in updates active and restarted recorders without changing the local identity', async () => {
  const env = await harness();
  const host = env.create();
  try {
    await host.lifecycle;
    const identity = env.captures[0].identity;
    host.useAccount({ id: 'local-account', label: 'ambientlight' });
    assert.equal(env.captures[0].userName, 'ambientlight');
    await env.commands.get('editchain-history.stopTracking')();
    await env.commands.get('editchain-history.startTracking')();
    assert.equal(env.captures.at(-1).userName, 'ambientlight');
    assert.deepEqual(env.captures.at(-1).identity, identity);
  } finally { await host.stop(); await env.cleanup(); }
});

const archiveEvent = (session, sequence, text) => ({ schema: 1, session, sequence, time_ms: 1700000000000 + sequence,
  identity: { kind: 'unsigned', guid: '22222222-2222-4222-8222-222222222222', stream: 'a'.repeat(24) },
  event: { type: 'document_snapshot', document: { id: '1', uri: 'file:///workspace/a.ts', path: 'a.ts', version: 1 }, text } });

async function archiveRecords(file) {
  return (await fs.readFile(file, 'utf8')).split('\n').filter(Boolean).map(line => JSON.parse(line));
}

test('the human history archive is off by default', async () => {
  const env = await harness();
  const host = env.create();
  try {
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('default', 1, 'off'));
    await host.stop();
    await assert.rejects(fs.readdir(path.join(env.directory, 'human-history')), { code: 'ENOENT' });
  } finally { await host.stop(); await env.cleanup(); }
});

test('an enabled archive keeps one file per activation and destination', async () => {
  const env = await harness();
  const host = env.create();
  const first = path.join(env.directory, 'archives-one');
  const second = path.join(env.directory, 'archives-two');
  try {
    await host.lifecycle;
    const started = env.captures.length;
    await env.configuration.update('tracking.jsonl.enabled', true);
    await host.lifecycle;
    assert.equal(env.captures.length, started + 1, 'enabling restarts capture so new sequences are complete');
    await env.configuration.update('tracking.jsonl.directory', first);
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('session-a1', 1, 'a1'));
    await env.configuration.update('tracking.jsonl.enabled', false);
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('session-off', 1, 'off'));
    await env.configuration.update('tracking.jsonl.enabled', true);
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('session-a2', 2, 'a2'));
    await env.configuration.update('tracking.jsonl.directory', second);
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('session-b1', 1, 'b1'));
    await env.configuration.update('tracking.jsonl.directory', first);
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('session-a3', 3, 'a3'));
    await host.stop();
    const one = (await fs.readdir(first)).sort();
    const two = (await fs.readdir(second)).sort();
    assert.deepEqual(one, [archiveFileName(archiveDay(new Date()), 1)], 're-enabling and returning reuse the activation file');
    assert.deepEqual(two, [archiveFileName(archiveDay(new Date()), 1)]);
    const records = await archiveRecords(path.join(first, one[0]));
    assert.deepEqual(records.map(record => record.event.session), ['session-a1', 'session-a2', 'session-a3']);
    assert.deepEqual(records.map(record => record.workspace_path), ['/workspace', '/workspace', '/workspace']);
    assert.deepEqual((await archiveRecords(path.join(second, two[0]))).map(record => record.event.session), ['session-b1']);
  } finally { await host.stop(); await env.cleanup(); }
});

test('a reload starts the next archive file and keeps earlier files intact', async () => {
  const env = await harness({ 'tracking.jsonl.enabled': true });
  let host = env.create();
  try {
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('first', 1, 'first'));
    await host.stop();
    host = env.create();
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('second', 1, 'second'));
    await host.stop();
    const directory = path.join(env.directory, 'human-history');
    const names = (await fs.readdir(directory)).sort();
    assert.deepEqual(names, [archiveFileName(archiveDay(new Date()), 1), archiveFileName(archiveDay(new Date()), 2)]);
    assert.deepEqual((await archiveRecords(path.join(directory, names[0]))).map(record => record.event.session), ['first']);
    assert.deepEqual((await archiveRecords(path.join(directory, names[1]))).map(record => record.event.session), ['second']);
  } finally { await host.stop(); await env.cleanup(); }
});

test('a failed archive destination is retained instead of rotated', async () => {
  const env = await harness();
  const host = env.create();
  const directory = path.join(env.directory, 'archives');
  try {
    await host.lifecycle;
    await fs.mkdir(directory);
    await env.configuration.update('tracking.jsonl.enabled', true);
    await env.configuration.update('tracking.jsonl.directory', directory);
    await host.lifecycle;
    // Break the destination after its writer exists, then let one write fail.
    await fs.rm(directory, { recursive: true, force: true });
    await fs.writeFile(directory, 'not a directory');
    env.captures.at(-1).emit(archiveEvent('failed', 1, 'x'));
    const stopped = () => env.logs.filter(line => line.includes('archive archiving stopped')).length;
    for (let attempt = 0; attempt < 200 && !stopped(); attempt++) {
      await new Promise(resolve => setTimeout(resolve, 5));
    }
    assert.equal(stopped(), 1);
    assert.equal(env.captures.at(-1).excluded(path.join(directory, archiveFileName(archiveDay(new Date()), 1))), true,
      'a failed destination stays excluded from capture');
    await env.configuration.update('tracking.jsonl.enabled', false);
    await host.lifecycle;
    await env.configuration.update('tracking.jsonl.enabled', true);
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('later', 1, 'y'));
    await host.stop();
    assert.equal(stopped(), 1, 'a failed destination is not replaced inside the same activation');
    assert.equal((await fs.stat(directory)).isFile(), true, 'no second same-activation file was allocated');
  } finally { await host.stop(); await env.cleanup(); }
});

test('every archive destination used by the activation stays excluded from capture', async () => {
  const env = await harness();
  const host = env.create();
  const first = path.join(env.directory, 'one');
  const second = path.join(env.directory, 'two');
  try {
    await host.lifecycle;
    await env.configuration.update('tracking.jsonl.enabled', true);
    await env.configuration.update('tracking.jsonl.directory', first);
    await host.lifecycle;
    await env.configuration.update('tracking.jsonl.directory', second);
    await host.lifecycle;
    const excluded = env.captures.at(-1).excluded;
    const name = archiveFileName(archiveDay(new Date()), 1);
    assert.equal(excluded(path.join(first, name)), true, 'returning to an earlier destination cannot re-record its file');
    assert.equal(excluded(path.join(second, name)), true);
    assert.equal(excluded(path.join(first, 'notes.txt')), false);
  } finally { await host.stop(); await env.cleanup(); }
});

test('a symlinked archive destination is excluded at its physical path', async () => {
  const env = await harness();
  const host = env.create();
  const workspace = path.join(env.directory, 'workspace');
  const physical = path.join(workspace, 'archives');
  const alias = path.join(env.directory, 'alias');
  try {
    await host.lifecycle;
    await fs.mkdir(physical, { recursive: true });
    await linkDirectory(physical, alias);
    await env.configuration.update('tracking.jsonl.enabled', true);
    await env.configuration.update('tracking.jsonl.directory', alias);
    await host.lifecycle;
    const excluded = env.captures.at(-1).excluded;
    const name = archiveFileName(archiveDay(new Date()), 1);
    assert.equal(excluded(path.join(alias, name)), true, 'the configured alias');
    assert.equal(excluded(path.join(physical, name)), true, 'the real path VS Code observes');
    assert.equal(excluded(path.join(workspace, 'notes.txt')), false, 'ordinary files still capture');
  } finally { await host.stop(); await env.cleanup(); }
});

test('concurrent stop callers share one shutdown that drains the archive', async () => {
  const env = await harness({ 'tracking.jsonl.enabled': true });
  const host = env.create();
  try {
    await host.lifecycle;
    env.captures.at(-1).emit(archiveEvent('drain', 1, 'x'.repeat(1024 * 1024)));
    const first = host.stop();
    assert.equal(host.stop(), first, 'subscription disposal and deactivate share one completion');
    await first;
    const directory = path.join(env.directory, 'human-history');
    const names = await fs.readdir(directory);
    assert.deepEqual(names, [archiveFileName(archiveDay(new Date()), 1)]);
    assert.deepEqual((await archiveRecords(path.join(directory, names[0]))).map(record => record.event.session), ['drain'],
      'the shared shutdown waited for the pending write');
  } finally { await host.stop(); await env.cleanup(); }
});
