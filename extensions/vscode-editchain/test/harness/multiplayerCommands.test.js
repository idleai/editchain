'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const { until } = require('./multiplayerFixture');

async function environment(options, body) {
  const registered = new Map(), stored = new Map(), workspace = new Map(), secrets = new Map(), logs = [], calls = [];
  if (options.saved) {
    workspace.set('editchain.multiplayer.enabled.file:///fixture/workspace', true);
    secrets.set('editchain.multiplayer.session.file:///fixture/workspace', JSON.stringify(options.saved));
  }
  const uri = { fsPath: '/fixture/workspace', scheme: 'file', toString: () => 'file:///fixture/workspace' };
  const folder = { name: 'workspace', uri };
  const session = { account: { id: 'account-id' }, accessToken: 'secret-user-token' };
  let managerLoads = 0, managerOptions, clipboard, resolveAuth;
  const state = values => ({ keys: () => [...values.keys()], get: key => values.get(key),
    async update(key, value) { if (value === undefined) values.delete(key); else values.set(key, value); } });
  const fake = {
    StatusBarAlignment: { Left: 1 },
    workspace: { isTrusted: options.trusted !== false, workspaceFolders: [folder],
      onDidChangeWorkspaceFolders: () => ({ dispose() {} }), onDidChangeConfiguration: () => ({ dispose() {} }),
      getConfiguration: () => ({ get: (_key, fallback) => fallback }) },
    env: { clipboard: { async writeText(text) { clipboard = text; } } },
    commands: { registerCommand(name, callback) { registered.set(name, callback); return { dispose() {} }; } },
    authentication: { async getSession(provider, scopes, request) {
      calls.push({ provider, scopes, request });
      if (options.authError) throw new Error('secret-user-token in provider failure');
      if (options.delayAuth && request.createIfNone) return await new Promise(resolve => { resolveAuth = resolve; });
      return session;
    } },
    window: {
      showInputBox: async () => 'join-request', showQuickPick: async items => items[0],
      showWarningMessage: async (_message, _options, choice) => choice,
      createOutputChannel: () => ({ appendLine: value => logs.push(value), show() {}, dispose() {} }),
      showInformationMessage: async value => { logs.push(value); },
      showErrorMessage: async value => { logs.push(value); },
      createStatusBarItem: () => ({ show() {}, dispose() {} }),
    },
  };
  class FakeManager {
    constructor(value) { managerOptions = value; }
    joinRequest() { return Promise.resolve('public-request'); }
    inspectRequest() { return Promise.resolve({ device: { fingerprint: 'a'.repeat(64) } }); }
    async hostHistory(_request, backfill) {
      calls.push({ host: true, backfill });
      assert.equal(await managerOptions.githubToken(), session.accessToken);
      await managerOptions.journal.remember('editchain-multiplayer-' + '1'.repeat(24));
      await managerOptions.saveSession({ version: 1, space: 'space', peers: ['private-invite-secret'] });
      return 'private-invite-secret';
    }
    status() { return { hosting: false, peers: [] }; }
    async stop() { calls.push({ stop: true }); await managerOptions.saveSession(undefined); await managerOptions.journal.forget('editchain-multiplayer-' + '1'.repeat(24)); }
    async suspend() { calls.push({ suspend: true }); }
    async resume() { calls.push({ resume: true }); }
    async reconnect() { calls.push({ reconnect: true }); }
  }
  const original = Module._load;
  Module._load = function (name, parent, ...rest) {
    if (name === 'vscode') return fake;
    if (name === './manager' && parent.filename.endsWith('/multiplayer/commands.js')) { managerLoads++; return { MultiplayerManager: FakeManager }; }
    if (name === './relay' && parent.filename.endsWith('/multiplayer/commands.js')) return {
      managementClient: () => ({ async dispose() {} }), cleanupRelay: async (_management, marker, journal) => { calls.push({ cleanup: marker }); await journal.forget(marker); },
    };
    return original.call(this, name, parent, ...rest);
  };
  const file = require.resolve('../../out/multiplayer/commands');
  delete require.cache[file];
  const context = { subscriptions: [], globalState: state(stored), workspaceState: state(workspace),
    secrets: { get: async key => secrets.get(key), store: async (key, value) => { secrets.set(key, value); }, delete: async key => { secrets.delete(key); } },
    asAbsolutePath: path => '/fixture/extension/' + path, globalStorageUri: { fsPath: '/fixture/private-storage' } };
  try {
    const commands = require(file).registerMultiplayerCommands(context, () => {});
    await body({ invoke: name => registered.get('editchain-history.' + name)(), stored, logs, calls,
      loads: () => managerLoads, clipboard: () => clipboard, resolveAuth: () => resolveAuth?.(session), authPending: () => !!resolveAuth, secrets, workspace, commands });
  } finally {
    for (const subscription of context.subscriptions) subscription.dispose();
    Module._load = original;
    delete require.cache[file];
  }
}

test('multiplayer activation is offline and explicit hosting defaults to new records', async () => {
  await environment({}, async env => {
    assert.equal(env.loads(), 0);
    assert.deepEqual(env.calls, []);
    assert.equal((await env.invoke('multiplayerHost')).ok, true);
    assert.ok(env.calls.some(call => call.host && call.backfill === false));
    assert.equal(env.clipboard(), 'private-invite-secret');
    assert.deepEqual(env.calls.filter(call => call.provider).map(call => call.scopes), [['read:user', 'read:org'], ['read:user', 'read:org']]);
    assert.equal(env.stored.size, 1);
    assert.ok(!JSON.stringify(env.logs).includes('secret-user-token'));
    assert.ok(!JSON.stringify(env.logs).includes('private-invite-secret'));
    assert.ok(!JSON.stringify([...env.workspace.values(), ...env.stored.values()]).includes('private-invite-secret'));
    assert.ok([...env.secrets.values()].some(value => value.includes('private-invite-secret')));
    await env.invoke('multiplayerStop');
    assert.equal(env.stored.size, 0);
    assert.equal(env.secrets.size, 0);
  });
});

test('enabled workspace resumes with silent account lookup; deactivation keeps private state', async () => {
  await environment({ saved: { account: 'account-id', session: { host: {}, peers: [] } } }, async env => {
    await until(() => env.calls.some(call => call.reconnect), 'saved sharing did not resume');
    assert.deepEqual(env.calls.filter(call => call.provider).map(call => call.request), [{ silent: true }]);
    await env.commands.suspend();
    assert.equal(env.secrets.size, 1);
    assert.ok(!env.calls.some(call => call.stop));
  });
});

test('a changed host account cannot resume a saved tunnel', async () => {
  await environment({ saved: { account: 'other-account', session: { host: {}, peers: [] } } }, async env => {
    await until(() => env.logs.some(line => line.includes('original host')), 'missing account mismatch notice');
    assert.ok(!env.calls.some(call => call.resume));
  });
});

test('refused authentication never logs provider credentials or starts hosting', async () => {
  await environment({ authError: true }, async env => {
    assert.equal((await env.invoke('multiplayerHost')).ok, false);
    assert.ok(!env.calls.some(call => call.host));
    assert.ok(!JSON.stringify(env.logs).includes('secret-user-token'));
  });
});

test('untrusted workspaces cannot start native or account activity', async () => {
  await environment({ trusted: false }, async env => {
    assert.equal((await env.invoke('multiplayerRequest')).ok, false);
    assert.equal(env.loads(), 0);
    assert.deepEqual(env.calls, []);
  });
});

test('Stop during sign-in prevents a late host start', async () => {
  await environment({ delayAuth: true }, async env => {
    const host = env.invoke('multiplayerHost');
    await until(env.authPending, 'sign-in was not requested');
    await env.invoke('multiplayerStop');
    env.resolveAuth();
    await host;
    assert.ok(!env.calls.some(call => call.host));
  });
});

test('cleanup skips other active windows and other workspaces', async () => {
  await environment({}, async env => {
    const prefix = 'editchain.multiplayer.pending.';
    const current = { account: 'account-id', workspace: 'file:///fixture/workspace', owner: 'different-window', leaseUntil: Date.now() + 60_000 };
    env.stored.set(prefix + 'active', current);
    env.stored.set(prefix + 'other-workspace', { ...current, workspace: 'file:///other', leaseUntil: 0 });
    env.stored.set(prefix + 'expired', { ...current, leaseUntil: 0 });
    await env.invoke('multiplayerCleanup');
    assert.deepEqual(env.calls.filter(call => call.cleanup), [{ cleanup: 'expired' }]);
    assert.ok(env.stored.has(prefix + 'active'));
    assert.ok(env.stored.has(prefix + 'other-workspace'));
  });
});
