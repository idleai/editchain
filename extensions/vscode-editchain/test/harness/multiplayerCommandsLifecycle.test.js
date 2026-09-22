'use strict';
// Commands-layer companion to multiplayerLifecycle.test.js: an explicit Stop must
// win over an in-flight Remove, and a retired manager must never rewrite the saved
// session, the auto-resume flag, or the status tooltip.
//
// The manager stand-in mirrors the lifecycle contract of manager.ts: stop()/
// suspend() bump a generation and clear state, resume() re-checks that generation
// after its awaits, and native-touching methods persist() at the end. It keeps no
// generation guard of its own, so retirement must be enforced by the commands.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const { until } = require('./multiplayerFixture');

async function commands(options, body) {
  const registered = new Map(), stored = new Map(), workspace = new Map(), secrets = new Map(), logs = [], calls = [];
  const instances = [];
  if (options.saved) {
    workspace.set('editchain.multiplayer.enabled.file:///fixture/workspace', true);
    secrets.set('editchain.multiplayer.session.file:///fixture/workspace', JSON.stringify(options.saved));
  }
  if (options.directory) workspace.set('editchain.multiplayer.directory.file:///fixture/workspace', options.directory);
  const uri = { fsPath: '/fixture/workspace', scheme: 'file', toString: () => 'file:///fixture/workspace' };
  const folder = { name: 'workspace', uri };
  const session = { account: { id: 'account-id' }, accessToken: 'secret-user-token' };
  let managerLoads = 0, clipboard, resolveAuth, configurationChanged, foldersChanged, resolveRevoke, resolveResumeAfter, resolveStop, statusItem;
  const configuration = new Map();
  const state = values => ({ keys: () => [...values.keys()], get: key => values.get(key),
    async update(key, value) { if (value === undefined) values.delete(key); else values.set(key, value); } });
  const fake = {
    StatusBarAlignment: { Left: 1 },
    workspace: { isTrusted: options.trusted !== false, workspaceFolders: [folder],
      onDidChangeWorkspaceFolders: callback => { foldersChanged = callback; return { dispose() {} }; },
      onDidChangeConfiguration: callback => { configurationChanged = callback; return { dispose() {} }; },
      getConfiguration: () => ({ get: (key, fallback) => configuration.get(key) ?? fallback }) },
    env: { clipboard: { async writeText(text) { clipboard = text; } } },
    commands: { registerCommand(name, callback) { registered.set(name, callback); return { dispose() {} }; } },
    authentication: { async getSession(provider, scopes, request) {
      calls.push({ provider, scopes, request });
      if (options.authError) throw new Error('secret-user-token in provider failure');
      if (options.delayAuth && request.createIfNone) return await new Promise(resolve => { resolveAuth = resolve; });
      return session;
    } },
    window: {
      showInputBox: async () => Object.hasOwn(options, 'input') ? options.input : 'join-request',
      showQuickPick: async items => options.disableDiscovery ? items.find(item => item.label === 'Disable repository discovery') : items[0],
      showWarningMessage: async (_message, _options, choice) => choice,
      createOutputChannel: () => ({ appendLine: value => logs.push(value), show() {}, dispose() {} }),
      showInformationMessage: async value => { logs.push(value); },
      showErrorMessage: async value => { logs.push(value); },
      createStatusBarItem: () => { statusItem ??= { show() {}, dispose() {} }; return statusItem; },
    },
  };
  class FakeManager {
    constructor(value) {
      this.options = value; this.enabled = !!options.sharing; this.generation = 0;
      this.space = options.sharing ? 'space' : undefined; this.peers = []; this.lease = undefined;
      this.statusText = undefined;
      instances.push(this);
    }
    async sharingScope() { return options.scope; }
    async changeScope(backfill) { calls.push({ scopeChange: true, backfill }); }
    joinRequest() { return Promise.resolve('public-request'); }
    inspectRequest() { return Promise.resolve({ device: { fingerprint: 'a'.repeat(64) } }); }
    async hostHistory(_request, backfill) {
      const generation = this.generation;
      calls.push({ host: true, backfill });
      assert.equal(await this.options.githubToken(), session.accessToken);
      await this.options.journal.remember('editchain-multiplayer-' + '1'.repeat(24));
      if (generation !== this.generation) throw new Error('Sharing was stopped.');
      this.space = this.space ?? 'space';
      this.enabled = true; this.lease = { marker: 'editchain-multiplayer-' + '1'.repeat(24), tunnelId: 'tunnel', clusterId: 'use' };
      this.statusText = 'Hosting. Give the invitation to the approved device.';
      this.options.changed(this.status(), true);
      await this.persist();
      return 'private-invite-secret';
    }
    status() { return { hosting: !!this.lease, peers: this.peers, enabled: this.enabled, space: this.space, message: this.statusText }; }
    async devices() { return [{ fingerprint: 'f'.repeat(64), certificate: 'certificate' }]; }
    async revoke() {
      calls.push({ revoke: true });
      if (options.delayRevoke) await new Promise(resolve => { resolveRevoke = resolve; });
      this.peers = this.peers.filter(peer => peer !== 'f'.repeat(64));
      await this.persist();
    }
    async stop() {
      this.generation++; this.enabled = false; this.peers = []; this.lease = undefined;
      this.statusText = 'Sharing stopped.'; calls.push({ stop: true }); this.options.changed(this.status(), false);
      await this.options.saveSession(undefined);
      await this.options.journal.forget('editchain-multiplayer-' + '1'.repeat(24));
      if (options.delayStop) await new Promise(resolve => { resolveStop = resolve; });
    }
    async suspend() {
      this.generation++; this.enabled = false; this.statusText = 'Sharing paused until this workspace reopens.';
      calls.push({ suspend: true }); this.options.changed(this.status(), false);
    }
    async resume() {
      const generation = this.generation;
      calls.push({ resume: true });
      if (generation !== this.generation) throw new Error('Sharing was stopped.');
      this.enabled = true;
      if (options.delayResumeAfterEnable) await new Promise(resolve => { resolveResumeAfter = resolve; });
      if (generation !== this.generation) throw new Error('Sharing was stopped.');
    }
    async reconnect() { calls.push({ reconnect: true }); }
    async persist() { await this.options.saveSession({ version: 1, space: this.space, host: this.lease, peers: this.peers }); }
  }
  const original = Module._load;
  Module._load = function (name, parent, ...rest) {
    if (name === 'vscode') return fake;
    if (name === './manager' && parent.filename.endsWith('/multiplayer/commands.js')) { managerLoads++; return { MultiplayerManager: FakeManager }; }
    if (name === './relay' && parent.filename.endsWith('/multiplayer/commands.js')) return {
      managementClient: () => ({ async dispose() {} }), cleanupRelay: async (_management, marker, journal) => { calls.push({ cleanup: marker }); await journal.forget(marker); },
    };
    if (name === './discovery' && parent.filename.endsWith('/multiplayer/commands.js')) return {
      repositoryName: value => require('../../out/multiplayer/discovery').repositoryName(value),
      GitHubDirectory: class { constructor(repository, token) { this.repository = repository; this.token = token; } },
      DirectorySync: class {
        constructor(directory) { this.directory = directory; }
        async start() { await this.directory.token(); calls.push({ discovery: this.directory.repository }); }
        async stop() { calls.push({ discoveryStop: true }); }
        async refresh() {}
      },
    };
    return original.call(this, name, parent, ...rest);
  };
  const file = require.resolve('../../out/multiplayer/commands');
  delete require.cache[file];
  const context = { subscriptions: [], globalState: state(stored), workspaceState: state(workspace),
    secrets: { get: async key => secrets.get(key), store: async (key, value) => { secrets.set(key, value); }, delete: async key => { secrets.delete(key); } },
    asAbsolutePath: value => '/fixture/extension/' + value, globalStorageUri: { fsPath: '/fixture/private-storage' } };
  try {
    const commands = require(file).registerMultiplayerCommands(context, () => {});
    await body({ invoke: name => registered.get('editchain-history.' + name)(), stored, logs, calls, instances,
      loads: () => managerLoads, clipboard: () => clipboard, resolveAuth: () => resolveAuth?.(session), authPending: () => !!resolveAuth, secrets, workspace, commands,
      configure: (values, affected = uri) => {
        for (const [key, value] of Object.entries(values)) configuration.set(key, value);
        const keys = new Set(Object.keys(values).map(key => 'editchain-history.' + key));
        configurationChanged({ affectsConfiguration: (key, scope) => keys.has(key) && (!scope || scope === affected) });
      },
      folders: (removed = [], added = []) => { fake.workspace.workspaceFolders = [folder, ...added].filter(value => !removed.includes(value)); foldersChanged({ added, removed }); },
      folder, resolveRevoke: () => { options.delayRevoke = false; resolveRevoke?.(); }, revokePending: () => !!resolveRevoke,
      resolveStop: () => { options.delayStop = false; resolveStop?.(); }, stopPending: () => !!resolveStop,
      statusItem: () => statusItem,
    });
  } finally {
    for (const subscription of context.subscriptions) subscription.dispose();
    Module._load = original;
    delete require.cache[file];
  }
}

const ENABLED = 'editchain.multiplayer.enabled.file:///fixture/workspace';
const SESSION = 'editchain.multiplayer.session.file:///fixture/workspace';

test('status command follows peer changes automatically and stops reporting when sharing stops', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setInterval'], now: Date.now() });
  await commands({}, async env => {
    const initial = await env.invoke('multiplayerStatus');
    assert.equal(initial.ok, true);
    assert.deepEqual(initial.value.peers, []);
    assert.equal(env.loads(), 0, 'status alone must not load the manager or start native/account activity');
    assert.deepEqual(env.calls, []);
    await env.invoke('multiplayerHost');
    const current = env.instances[0];
    current.peers = [{ fingerprint: 'd'.repeat(64), state: 'Catching up', progress: {
      accepted: true, synchronizing: true, rounds: 0, records: 128, blobs: 5, unavailable: 0,
    } }];
    current.options.changed(current.status(), true);
    assert.equal(env.statusItem().text, '$(broadcast) Sharing · 1/1 connected · syncing');
    assert.match(env.statusItem().tooltip, /Connected; syncing shared history \(first pass\)/);
    t.mock.timers.tick(1000);
    assert.ok(env.logs.some(line => /Peer dddddddddddd.*Received here: 128 records, 5 content objects/.test(line)));
    const snapshot = await env.invoke('multiplayerStatus');
    assert.equal(snapshot.value.peers[0].progress.records, 128, 'retain the structured command result');
    env.logs.length = 0;
    t.mock.timers.tick(15_000);
    assert.equal(env.logs.length, 1, 'reopening status must not register duplicate watchers');
    assert.match(env.logs[0], /No new saved-data update observed in 16s/);
    assert.ok(!JSON.stringify(env.logs).includes('secret-user-token'));
    assert.ok(!JSON.stringify(env.logs).includes('private-invite-secret'));
    current.peers[0].progress.synchronizing = false;
    current.peers[0].progress.rounds = 1;
    current.peers[0].state = 'Live';
    current.options.changed(current.status(), false);
    assert.equal(env.statusItem().text, '$(broadcast) Sharing · 1/1 connected');
    current.peers[0] = { fingerprint: 'd'.repeat(64), state: 'Waiting to reconnect' };
    current.options.changed(current.status(), false);
    assert.equal(env.statusItem().text, '$(broadcast) Sharing · reconnecting');
    await env.invoke('multiplayerStop');
    assert.equal(env.statusItem().text, '$(broadcast) Sharing stopped');
    assert.match(env.logs.at(-1), /Sharing stopped/);
    env.logs.length = 0;
    t.mock.timers.tick(60_000);
    assert.equal(env.logs.length, 0, 'Stop must cancel the progress timer');
  });
});

test('C1: Stop during an in-flight Remove must not resurrect the saved session', async () => {
  await commands({ delayRevoke: true }, async env => {
    await env.invoke('multiplayerHost');
    assert.equal(env.secrets.size, 1, 'host should save a session');
    const remove = env.invoke('multiplayerRemove');
    await until(env.revokePending, 'revoke did not reach its native call');
    await env.invoke('multiplayerStop');
    assert.equal(env.secrets.size, 0, 'Stop clears the saved session');
    assert.ok(!env.workspace.get(ENABLED), 'Stop clears the auto-resume flag');
    env.resolveRevoke();
    await remove;
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.secrets.size, 0, 'saved session resurrected after explicit Stop');
    assert.ok(!env.workspace.get(ENABLED), 'auto-resume flag resurrected after explicit Stop');
  });
});

test('C2: a late Remove must not re-arm sharing after the shared chain changed', async () => {
  await commands({ delayRevoke: true }, async env => {
    await env.invoke('multiplayerHost');
    const remove = env.invoke('multiplayerRemove');
    await until(env.revokePending, 'revoke did not reach its native call');
    env.configure({ chainDir: '.another-history' });
    await until(() => env.calls.some(call => call.stop), 'chain change did not close the session');
    env.resolveRevoke();
    await remove;
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.secrets.size, 0, 'saved session re-armed after the shared chain changed');
    assert.ok(!env.workspace.get(ENABLED), 'auto-resume flag re-armed after the shared chain changed');
  });
});

test('C3: suspend during an in-flight Remove keeps the private session', async () => {
  await commands({ delayRevoke: true }, async env => {
    await env.invoke('multiplayerHost');
    const remove = env.invoke('multiplayerRemove');
    await until(env.revokePending, 'revoke did not reach its native call');
    await env.commands.suspend();
    env.resolveRevoke();
    await remove;
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.secrets.size, 1, 'suspend must preserve the private session');
    assert.ok(env.workspace.get(ENABLED), 'suspend must keep the auto-resume flag for the next window');
  });
});

test('C4: a settings change after an explicit Stop must not promise a resume', async () => {
  await commands({}, async env => {
    await env.invoke('multiplayerHost');
    await env.invoke('multiplayerStop');
    assert.match(String(env.statusItem().tooltip), /Sharing stopped/, 'precondition: Stop reports stopped');
    const before = env.calls.filter(call => call.suspend).length;
    env.configure({ servicePath: '/new/service' });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.calls.filter(call => call.suspend).length, before,
      'a retired manager must not be suspended again after Stop');
    assert.doesNotMatch(String(env.statusItem().tooltip), /paused until this workspace reopens/,
      'status after Stop must not promise a resume');
  });
});

test('C5: a window stopped by an in-flight Remove does not auto-resume', async () => {
  let leaked;
  await commands({ delayRevoke: true }, async env => {
    await env.invoke('multiplayerHost');
    const remove = env.invoke('multiplayerRemove');
    await until(env.revokePending, 'revoke did not reach its native call');
    await env.invoke('multiplayerStop');
    env.resolveRevoke();
    await remove;
    await new Promise(resolve => setImmediate(resolve));
    leaked = { saved: env.secrets.get(SESSION), enabled: !!env.workspace.get(ENABLED) };
  });
  assert.equal(leaked.saved, undefined, 'Stop left a saved session behind');
  assert.equal(leaked.enabled, false, 'Stop left the auto-resume flag behind');
  await commands(leaked.saved ? { saved: JSON.parse(leaked.saved) } : {}, async env => {
    await new Promise(resolve => setImmediate(resolve));
    assert.ok(!env.calls.some(call => call.resume), 'a stopped window silently resumed sharing');
  });
});

test('C6: a Host started during Stop cleanup must not grab the retired manager', async () => {
  await commands({ delayStop: true }, async env => {
    await env.invoke('multiplayerHost');
    assert.equal(env.instances.length, 1, 'precondition: one manager is hosting');
    const stopping = env.invoke('multiplayerStop');
    await until(env.stopPending, 'Stop cleanup did not reach its await');
    const hosting = env.invoke('multiplayerHost');
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.instances.length, 1, 'a Host grabbed the retired manager while Stop was cleaning up');
    assert.equal(env.calls.filter(call => call.host).length, 1, 'a Host ran before Stop cleanup finished');
    env.resolveStop();
    await stopping;
    await hosting;
    assert.equal(env.instances.length, 2, 'the Host after Stop cleanup must use a fresh manager');
    assert.equal(env.instances[0].enabled, false, 'the retired manager must stay stopped');
    assert.equal(env.instances[1].enabled, true, 'the fresh manager must be hosting');
    assert.equal(env.secrets.size, 1, 'the fresh sharing session must be saved');
    assert.ok(env.workspace.get(ENABLED), 'the fresh auto-resume flag must be set');
    assert.equal(env.calls.filter(call => call.stop).length, 1, 'the fresh manager must not be discarded by the earlier Stop');
  });
});
