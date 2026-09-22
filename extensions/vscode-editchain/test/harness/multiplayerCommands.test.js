'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const { spawnSync } = require('node:child_process');
const { until } = require('./multiplayerFixture');

async function environment(options, body) {
  const registered = new Map(), stored = new Map(), workspace = new Map(), secrets = new Map(), logs = [], calls = [];
  if (options.saved) {
    workspace.set('editchain.multiplayer.enabled.file:///fixture/workspace', true);
    secrets.set('editchain.multiplayer.session.file:///fixture/workspace', JSON.stringify(options.saved));
  }
  if (options.directory) workspace.set('editchain.multiplayer.directory.file:///fixture/workspace', options.directory);
  const uri = { fsPath: '/fixture/workspace', scheme: 'file', toString: () => 'file:///fixture/workspace' };
  const folder = { name: 'workspace', uri };
  const session = { account: { id: 'account-id', label: 'account-name' }, accessToken: 'secret-user-token' };
  let managerLoads = 0, clipboard, resolveAuth, configurationChanged, foldersChanged, resolveSuspend, resolveResume;
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
      showQuickPick: async items => {
        calls.push({ choices: items.map(item => ({ label: item.label, detail: item.detail })) });
        return options.cancelScope ? undefined : options.scopeChoice ? items.find(item => item.label === options.scopeChoice)
          : options.disableDiscovery ? items.find(item => item.label === 'Disable repository discovery') : items[0];
      },
      showWarningMessage: async (_message, _options, choice) => choice,
      createOutputChannel: () => ({ appendLine: value => logs.push(value), show() {}, dispose() {} }),
      showInformationMessage: async value => { logs.push(value); },
      showErrorMessage: async value => { logs.push(value); },
      createStatusBarItem: () => ({ show() {}, dispose() {} }),
    },
  };
  class FakeManager {
    constructor(value) { this.options = value; this.enabled = !!options.sharing; }
    async sharingScope() { return options.scope; }
    async changeScope(backfill) { calls.push({ scopeChange: true, backfill }); }
    joinRequest() { return Promise.resolve('public-request'); }
    inspectRequest() { return Promise.resolve({ device: { fingerprint: 'a'.repeat(64) } }); }
    inspectInvitation() { return Promise.resolve({ space: 'space', host: { fingerprint: 'b'.repeat(64) } }); }
    async joinHistory(_invitation, backfill) { this.enabled = true; calls.push({ join: true, backfill }); }
    async hostHistory(_request, backfill) {
      this.enabled = true;
      calls.push({ host: true, backfill });
      assert.equal(await this.options.githubToken(), session.accessToken);
      await this.options.journal.remember('editchain-multiplayer-' + '1'.repeat(24));
      await this.options.saveSession({ version: 1, space: 'space', host: {}, peers: ['private-invite-secret'] });
      return 'private-invite-secret';
    }
    status() { return { hosting: false, peers: [], enabled: this.enabled, space: this.enabled ? 'space' : undefined }; }
    async stop() { this.enabled = false; calls.push({ stop: true }); await this.options.saveSession(undefined); await this.options.journal.forget('editchain-multiplayer-' + '1'.repeat(24)); }
    async suspend() { this.enabled = false; calls.push({ suspend: true }); if (options.delaySuspend) await new Promise(resolve => { resolveSuspend = resolve; }); }
    async resume() {
      calls.push({ resume: true });
      if (options.delayResume) await new Promise(resolve => { resolveResume = resolve; });
      if (options.resumeStopped) throw new Error('Sharing was stopped.');
      this.enabled = true;
    }
    async reconnect() { calls.push({ reconnect: true }); if (options.reconnectThrows) throw new Error('host temporarily offline'); }
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
    asAbsolutePath: path => '/fixture/extension/' + path, globalStorageUri: { fsPath: '/fixture/private-storage' } };
  try {
    const commands = require(file).registerMultiplayerCommands(context, () => {}, account => calls.push({ identified: account }));
    await body({ invoke: name => registered.get('editchain-history.' + name)(), stored, logs, calls,
      loads: () => managerLoads, clipboard: () => clipboard, resolveAuth: () => resolveAuth?.(session), authPending: () => !!resolveAuth, secrets, workspace, commands,
      configure: (values, affected = uri) => {
        for (const [key, value] of Object.entries(values)) configuration.set(key, value);
        const keys = new Set(Object.keys(values).map(key => 'editchain-history.' + key));
        configurationChanged({ affectsConfiguration: (key, scope) => keys.has(key) && (!scope || scope === affected) });
      },
      folders: (removed = [], added = []) => { fake.workspace.workspaceFolders = [folder, ...added].filter(value => !removed.includes(value)); foldersChanged({ added, removed }); },
      folder, resolveSuspend: () => { options.delaySuspend = false; resolveSuspend?.(); },
      resolveResume: () => resolveResume?.(), resumePending: () => !!resolveResume,
    });
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
    assert.deepEqual(env.calls.find(call => call.identified), { identified: { id: 'account-id', label: 'account-name' } });
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
    await until(() => env.calls.some(call => call.resume), 'saved sharing did not resume');
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

test('repository discovery requests repo access only after explicit configuration', async () => {
  await environment({ sharing: true, input: 'owner/repository' }, async env => {
    assert.deepEqual(env.calls, []);
    assert.equal((await env.invoke('multiplayerDiscovery')).ok, true);
    assert.deepEqual(env.calls.filter(call => call.provider).map(call => call.scopes), [['repo'], ['repo']]);
    assert.ok(env.calls.some(call => call.discovery === 'owner/repository'));
    assert.ok(!JSON.stringify([...env.workspace.values()]).includes('secret-user-token'));
    await env.commands.stop();
    assert.ok(env.calls.some(call => call.discoveryStop));
  });
});

test('Stop during discovery sign-in prevents later publication', async () => {
  await environment({ sharing: true, input: 'owner/repository', delayAuth: true }, async env => {
    const enabling = env.invoke('multiplayerDiscovery');
    await until(env.authPending, 'discovery sign-in was not requested');
    await env.invoke('multiplayerStop'); env.resolveAuth(); await enabling;
    assert.ok(!env.calls.some(call => call.discovery));
    assert.ok(![...env.workspace.keys()].some(key => key.includes('.directory.')));
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

test('an immediate Stop cancels a command before it starts account activity', async () => {
  await environment({}, async env => {
    const host = env.invoke('multiplayerHost');
    await env.invoke('multiplayerStop');
    await host;
    assert.ok(!env.calls.some(call => call.host || call.provider));
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

test('an exited extension host releases its lease immediately while a live process stays protected', async () => {
  const exited = spawnSync(process.execPath, ['-e', '']);
  assert.equal(exited.status, 0);
  await environment({}, async env => {
    const prefix = 'editchain.multiplayer.pending.';
    const current = { account: 'account-id', workspace: 'file:///fixture/workspace', owner: 'different-window', leaseUntil: Date.now() + 90_000 };
    env.stored.set(prefix + 'live-process', { ...current, process: process.pid });
    env.stored.set(prefix + 'exited-process', { ...current, process: exited.pid });
    await env.invoke('multiplayerCleanup');
    assert.deepEqual(env.calls.filter(call => call.cleanup), [{ cleanup: 'exited-process' }]);
    assert.ok(env.stored.has(prefix + 'live-process'));
  });
});

test('cleanup cannot delete a current session while it is waiting to reconnect', async () => {
  await environment({ sharing: true }, async env => {
    await env.invoke('multiplayerRequest');
    const result = await env.invoke('multiplayerCleanup');
    assert.equal(result.ok, false);
    assert.match(result.message, /Stop sharing/);
    assert.ok(!env.calls.some(call => call.provider || call.cleanup));
  });
});

test('saved-resource deletion requires ownership before any account or service access', async () => {
  const { removeSavedRelay } = require('../../out/multiplayer/relay');
  let accessed = false;
  const lease = { marker: 'editchain-multiplayer-' + 'a'.repeat(24), tunnelId: 'saved-resource', clusterId: 'use' };
  await assert.rejects(removeSavedRelay(lease, {
    remember: async () => { throw new Error('owned by another active window'); }, forget: async () => { throw new Error('must remain recorded'); },
  }, async () => { accessed = true; return 'secret'; }), /another active window/);
  assert.equal(accessed, false);
});

test('binary setting changes resume the same session; unrelated settings and folders leave it running', async () => {
  await environment({}, async env => {
    assert.equal((await env.invoke('multiplayerHost')).ok, true);
    const saved = [...env.secrets.values()];
    env.folders([], [{ name: 'other', uri: { toString: () => 'file:///other' } }]);
    env.configure({ peerPath: '/other/peer' }, { fsPath: '/other' });
    await new Promise(resolve => setImmediate(resolve));
    assert.ok(!env.calls.some(call => call.stop || call.suspend));
    env.configure({ peerPath: '/new/peer' });
    await until(() => env.loads() === 2 && env.calls.some(call => call.resume), 'new worker did not resume sharing');
    assert.deepEqual([...env.secrets.values()], saved);
    assert.ok(env.workspace.get('editchain.multiplayer.enabled.file:///fixture/workspace'));
    assert.ok(!env.calls.some(call => call.stop));
  });
});

test('changing the shared chain or removing its folder closes only that sharing session', async () => {
  for (const change of [env => env.configure({ chainDir: '.another-history' }), env => env.folders([env.folder])]) {
    await environment({}, async env => {
      await env.invoke('multiplayerHost');
      change(env);
      await until(() => env.calls.some(call => call.stop) && env.secrets.size === 0, 'old chain did not stop');
      assert.ok(!env.calls.some(call => call.resume));
    });
  }
});

test('Stop during a binary restart removes the preserved session and prevents late resume', async () => {
  await environment({ delaySuspend: true }, async env => {
    await env.invoke('multiplayerHost');
    env.configure({ servicePath: '/new/service' });
    await until(() => env.calls.some(call => call.suspend), 'worker was not suspended');
    await env.invoke('multiplayerStop');
    env.resolveSuspend();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(env.secrets.size, 0);
    assert.ok(!env.calls.some(call => call.resume));
  });
});

test('consecutive binary changes coalesce into one resume with the saved session', async () => {
  await environment({ delaySuspend: true }, async env => {
    await env.invoke('multiplayerHost');
    env.configure({ peerPath: '/first/peer' });
    await until(() => env.calls.some(call => call.suspend), 'first restart did not begin');
    env.configure({ peerPath: '/second/peer' });
    env.resolveSuspend();
    await until(() => env.calls.some(call => call.resume), 'consecutive changes lost resume intent');
    assert.equal(env.calls.filter(call => call.resume).length, 1);
    assert.equal(env.secrets.size, 1);
    assert.ok(!env.calls.some(call => call.stop));
  });
});

test('saved discovery starts without a redundant reconnect after resume', async () => {
  await environment({ saved: { account: 'account-id', session: { host: {}, peers: [] } },
    directory: { repository: 'owner/repository', account: 'account-id' }, reconnectThrows: true }, async env => {
    await until(() => env.calls.some(call => call.discovery), 'saved discovery did not resume');
    assert.ok(!env.calls.some(call => call.reconnect));
  });
});

test('Stop during native resume is quiet and prevents discovery from restarting', async () => {
  const options = { saved: { account: 'account-id', session: { host: {}, peers: [] } },
    directory: { repository: 'owner/repository', account: 'account-id' }, delayResume: true };
  await environment(options, async env => {
    await until(env.resumePending, 'native resume was not reached');
    await env.invoke('multiplayerStop');
    options.resumeStopped = true;
    env.resolveResume();
    await new Promise(resolve => setImmediate(resolve));
    assert.ok(!env.calls.some(call => call.reconnect || call.discovery));
    assert.deepEqual(env.logs, []);
  });
});

test('cancelled, invalid and denied discovery changes preserve the working directory', async () => {
  for (const failure of ['cancel', 'invalid', 'denied']) {
    const options = { saved: { account: 'account-id', session: { host: {}, peers: [] } },
      directory: { repository: 'owner/repository', account: 'account-id' } };
    await environment(options, async env => {
      await until(() => env.calls.some(call => call.discovery), 'initial discovery did not start');
      options.input = failure === 'cancel' ? undefined : failure === 'invalid' ? 'not a repo name' : 'another/repository';
      options.authError = failure === 'denied';
      const result = await env.invoke('multiplayerDiscovery');
      assert.equal(result.ok, failure === 'cancel');
      assert.deepEqual(env.workspace.get('editchain.multiplayer.directory.file:///fixture/workspace'), options.directory);
      assert.ok(!env.calls.some(call => call.discoveryStop));
      assert.equal(env.calls.filter(call => call.discovery).length, 1);
    });
  }
});

test('confirmed discovery replacement switches directories and explicit disable withdraws it', async () => {
  const options = { saved: { account: 'account-id', session: { host: {}, peers: [] } },
    directory: { repository: 'owner/repository', account: 'account-id' }, input: 'another/repository' };
  await environment(options, async env => {
    await until(() => env.calls.some(call => call.discovery), 'initial discovery did not start');
    assert.equal((await env.invoke('multiplayerDiscovery')).ok, true);
    assert.equal(env.workspace.get('editchain.multiplayer.directory.file:///fixture/workspace').repository, 'another/repository');
    assert.deepEqual(env.calls.filter(call => call.discovery || call.discoveryStop), [
      { discovery: 'owner/repository' }, { discoveryStop: true }, { discovery: 'another/repository' },
    ]);
    options.disableDiscovery = true;
    assert.equal((await env.invoke('multiplayerDiscovery')).ok, true);
    assert.ok(!env.workspace.has('editchain.multiplayer.directory.file:///fixture/workspace'));
    assert.equal(env.calls.filter(call => call.discoveryStop).length, 2);
  });
});

test('joining labels local work with the joining account before connecting', async () => {
  await environment({}, async env => {
    assert.equal((await env.invoke('multiplayerJoin')).ok, true);
    const identified = env.calls.findIndex(call => call.identified);
    assert.ok(identified >= 0);
    assert.deepEqual(env.calls[identified].identified, { id: 'account-id', label: 'account-name' });
    assert.ok(identified < env.calls.findIndex(call => call.join));
    assert.deepEqual(env.calls.filter(call => call.provider).map(call => call.request), [{ createIfNone: true }]);
  });
});

test('Stop while a join sign-in is pending prevents late attribution and connection', async () => {
  await environment({ delayAuth: true }, async env => {
    const joining = env.invoke('multiplayerJoin');
    await until(env.authPending, 'joining sign-in was not requested');
    await env.invoke('multiplayerStop');
    env.resolveAuth(); await joining;
    assert.ok(!env.calls.some(call => call.identified || call.join));
  });
});

test('existing spaces offer keeping the actual scope and an explicit new cutoff', async () => {
  const scope = { space: 'space', mode: 'all', active: true, revision: 1, cutoff_ms: null, legacy_excluded_records: 0 };
  await environment({ scope }, async env => {
    assert.equal((await env.invoke('multiplayerHost')).ok, true);
    assert.ok(env.calls.some(call => call.host && call.backfill === 'keep'));
    const choices = env.calls.find(call => call.choices).choices;
    assert.equal(choices[0].label, 'Keep current sharing scope');
    assert.match(choices[0].detail, /all retained history/);
    assert.match(choices.find(item => item.label === 'Share records added from now on').detail, /Set a new cutoff/);
  });
  await environment({ scope, scopeChoice: 'Share records added from now on' }, async env => {
    assert.equal((await env.invoke('multiplayerHost')).ok, true);
    assert.ok(env.calls.some(call => call.host && call.backfill === false), 'explicit from-now must reach the manager even for an all-history space');
  });
});

test('scope can be changed without repeating invitations, and cancellation preserves it', async () => {
  const scope = { space: 'space', mode: 'all', active: true, revision: 1, cutoff_ms: null, legacy_excluded_records: 0 };
  for (const [scopeChoice, expected] of [['Share records added from now on', false], ['Include existing history', true]]) {
    await environment({ scope, scopeChoice }, async env => {
      assert.equal((await env.invoke('multiplayerScope')).ok, true);
      assert.deepEqual(env.calls.filter(call => call.scopeChange), [{ scopeChange: true, backfill: expected }]);
      assert.ok(!env.calls.some(call => call.host || call.join || call.provider), 'scope selection does not ask for another device or account');
    });
  }
  await environment({ scope, cancelScope: true }, async env => {
    assert.equal((await env.invoke('multiplayerScope')).ok, true);
    assert.ok(!env.calls.some(call => call.scopeChange));
  });
});
