'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const { CancellationTokenSource } = require('vscode-jsonrpc');
const actualSdk = require('../../out/devTunnels/spike');

async function withCommand(options, testBody) {
  const registered = new Map();
  const stored = new Map();
  const logs = [];
  const authCalls = [];
  let sdkLoads = 0;
  let created = 0;
  let getToken;
  const session = { account: { id: 'test-account', label: 'test-user' }, accessToken: 'secret-user-token' };
  const progressCancellation = new CancellationTokenSource();
  const fakeVscode = {
    CancellationTokenSource,
    ProgressLocation: { Notification: 15 },
    authentication: {
      async getSession(provider, scopes, request) {
        authCalls.push({ provider, scopes, request });
        if (options.authError) throw new Error('credential-bearing error: secret-user-token');
        if (request.silent && options.changedAccount) return { ...session, account: { id: 'other-account' } };
        return session;
      },
    },
    commands: { registerCommand(name, callback) { registered.set(name, callback); return { dispose() {} }; } },
    window: {
      createOutputChannel: () => ({ show() {}, dispose() {}, appendLine: line => logs.push(line) }),
      withProgress: (_options, task) => task({ report() {} }, progressCancellation.token),
      showInformationMessage: async () => {},
      showErrorMessage: async message => { logs.push(message); },
    },
  };
  const fakeSdk = {
    ...actualSdk,
    createSpikeServices(callback) {
      created++;
      getToken = callback;
      return { management: { async dispose() {} } };
    },
    async runSpike(_services, journal) {
      assert.equal(await getToken(), session.accessToken);
      await journal.remember('test-resource');
      assert.deepEqual([...stored.values()], [session.account.id]);
      await journal.forget('test-resource');
      return { tunnelDeleted: true, roundTrips: 20 };
    },
  };
  const originalLoad = Module._load;
  Module._load = function (request, parent, ...rest) {
    if (request === 'vscode') return fakeVscode;
    if (request === './spike' && parent.filename.endsWith('/devTunnels/commands.js')) {
      sdkLoads++;
      return fakeSdk;
    }
    return originalLoad.call(this, request, parent, ...rest);
  };
  const path = require.resolve('../../out/devTunnels/commands');
  delete require.cache[path];
  const context = {
    subscriptions: [],
    globalState: {
      keys: () => [...stored.keys()],
      get: key => stored.get(key),
      async update(key, value) { if (value === undefined) stored.delete(key); else stored.set(key, value); },
    },
  };
  try {
    require(path).registerDevTunnelsCommands(context);
    await testBody({ registered, logs, authCalls, stored, sdkLoads: () => sdkLoads, created: () => created });
  } finally {
    Module._load = originalLoad;
    for (const subscription of context.subscriptions) subscription.dispose();
    progressCancellation.dispose();
    delete require.cache[path];
  }
}

test('activation is offline; the command uses VS Code GitHub auth and refreshes silently', async () => {
  await withCommand({}, async env => {
    assert.equal(env.authCalls.length, 0);
    assert.equal(env.sdkLoads(), 0);
    assert.equal(env.created(), 0);
    const result = await env.registered.get('editchain-history.devTunnelsSpike')();
    assert.equal(result.ok, true);
    assert.deepEqual(env.authCalls, [
      { provider: 'github', scopes: ['read:user', 'read:org'], request: { createIfNone: true } },
      { provider: 'github', scopes: ['read:user', 'read:org'], request: { silent: true } },
    ]);
    assert.equal(env.stored.size, 0);
    assert.ok(!env.logs.join('\n').includes('secret-user-token'));
  });
});

test('refused authentication creates no SDK services and never logs the provider error', async () => {
  await withCommand({ authError: true }, async env => {
    const result = await env.registered.get('editchain-history.devTunnelsSpike')();
    assert.equal(result.ok, false);
    assert.equal(env.created(), 0);
    assert.ok(!env.logs.join('\n').includes('secret-user-token'));
    assert.equal(env.stored.size, 0);
  });
});

test('an account switch during token refresh aborts instead of mixing identities', async () => {
  await withCommand({ changedAccount: true }, async env => {
    const result = await env.registered.get('editchain-history.devTunnelsSpike')();
    assert.equal(result.ok, false);
    assert.match(result.message, /session changed/);
    assert.ok(!env.logs.join('\n').includes('secret-user-token'));
  });
});
