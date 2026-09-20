'use strict';
// Offline regression for relay resource cleanup. The Dev Tunnels SDK host is
// stubbed so the exact production status ordering (Disconnected emitted from
// finishConnecting() before connect rejects) is reproducible without network.
// MultiplayerManager, RelayHost, journal handling and cleanupRelay stay real.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const Module = require('node:module');
const { ConnectionStatus } = require('@microsoft/dev-tunnels-connections');
const { fixture, binaries, until } = require('./multiplayerFixture');

const sdk = { behavior: {}, hosts: [] };

class StubHost {
  constructor() {
    this.forwardConnectionsToLocalPorts = undefined;
    this.enableE2EEncryption = undefined;
    this.hostPublicKeys = ['SE9TVC1LRVk='];
    this.connectionProtocol = 'tunnel-relay-host-v2-dev';
    sdk.hosts.push(this);
  }
  forwardedPortConnecting() { return { dispose() {} }; }
  connectionStatusChanged(callback) { this.statusCallback = callback; return { dispose() {} }; }
  async connect() {
    const behavior = sdk.behavior;
    if (behavior.statuses !== false) {
      this.statusCallback?.({ status: ConnectionStatus.Connecting });
      // The SDK emits this from finishConnecting() before connect() rejects.
      this.statusCallback?.({ status: ConnectionStatus.Disconnected });
    }
    if (behavior.connectGate) await behavior.connectGate;
    if (behavior.succeed) return;
    throw new Error('relay connect failed');
  }
  async dispose() {
    const behavior = sdk.behavior;
    behavior.hostDisposeStarted = (behavior.hostDisposeStarted ?? 0) + 1;
    if (behavior.hostDisposeGate) await behavior.hostDisposeGate;
    if (behavior.hostDisposeError) throw new Error(behavior.hostDisposeError);
    if (behavior.hostDisposeFails > 0) { behavior.hostDisposeFails--; throw new Error('host dispose failed'); }
  }
}

class FakeManagement {
  constructor(state, behavior) { this.state = state; this.behavior = behavior; this.disposed = false; }
  // A disposed client must never be reused: the factory path has to recreate one.
  use(operation) { if (this.disposed) throw new Error(`management client used after dispose during ${operation}`); }
  async createTunnel(tunnel) {
    this.use('createTunnel');
    const value = { tunnelId: `tunnel-${this.state.created.length + 1}`, clusterId: 'use', labels: tunnel.labels, ports: tunnel.ports };
    this.state.created.push(value.tunnelId);
    this.state.tunnels.push(value);
    return value;
  }
  async getTunnel(locator) {
    this.use('getTunnel');
    this.state.getTunnel.push(locator);
    const tunnel = this.state.tunnels.find(value => value.tunnelId === locator?.tunnelId);
    if (!tunnel) return undefined;
    const connectToken = ['e30', Buffer.from(JSON.stringify({ exp: Math.ceil(Date.now() / 1000) + 3600 })).toString('base64url'), 'signature'].join('.');
    return { ...tunnel, endpoints: [{ id: 'e1', hostId: 'host', connectionMode: 'TunnelRelay', hostPublicKeys: ['SE9TVC1LRVk='], clientRelayUri: 'wss://use.rel.tunnels.api.visualstudio.com/tunnel' }], accessTokens: { connect: connectToken } };
  }
  async listTunnels() { this.use('listTunnels'); return [...this.state.tunnels]; }
  async deleteTunnel(tunnel) {
    this.use('deleteTunnel');
    this.state.deleteAttempts.push(tunnel.tunnelId);
    await this.behavior.beforeDelete?.(this.state.deleteAttempts.length);
    if (this.behavior.deleteError) throw new Error(this.behavior.deleteError);
    if (this.behavior.deleteFails > 0) { this.behavior.deleteFails--; throw new Error('delete failed'); }
    this.state.deleted.push(tunnel.tunnelId);
    this.state.tunnels = this.state.tunnels.filter(value => value.tunnelId !== tunnel.tunnelId);
    return true;
  }
  async dispose() {
    if (this.disposed) return;
    this.state.disposeAttempts++;
    if (this.behavior.disposeError) throw new Error(this.behavior.disposeError);
    this.disposed = true; this.state.disposed++;
  }
}

function state() { return { created: [], deleted: [], deleteAttempts: [], tunnels: [], getTunnel: [], disposed: 0, disposeAttempts: 0 }; }

// relay.js imports the SDK host at load, so the stub must be installed for that require.
function loadModules() {
  const real = require('@microsoft/dev-tunnels-connections');
  const original = Module._load;
  Module._load = function (name, parent, ...rest) {
    if (name === '@microsoft/dev-tunnels-connections') return { ...real, TunnelRelayTunnelHost: StubHost };
    return original.call(this, name, parent, ...rest);
  };
  try {
    const relay = require.resolve('../../out/multiplayer/relay');
    const manager = require.resolve('../../out/multiplayer/manager');
    delete require.cache[relay]; delete require.cache[manager];
    return { RelayHost: require(relay).RelayHost, MultiplayerManager: require(manager).MultiplayerManager };
  } finally { Module._load = original; }
}

function journal(rejectOwnership) {
  const remembered = new Set();
  return { remembered,
    remember: async marker => { if (rejectOwnership) throw new Error('This hosting session is active in another window.'); remembered.add(marker); },
    forget: async marker => { remembered.delete(marker); } };
}

function relay(behavior = {}) {
  const store = state();
  sdk.behavior = behavior;
  const { RelayHost } = loadModules();
  const log = journal(behavior.rejectOwnership);
  return { store, journal: log,
    create: management => new RelayHost(management ?? new FakeManagement(store, behavior), log, () => {}, () => {}),
    factory: () => new RelayHost(() => new FakeManagement(store, behavior), log, () => {}, () => {}) };
}

function hosted(behavior = {}) {
  const store = state();
  sdk.behavior = behavior;
  sdk.hosts.length = 0;
  const { RelayHost, MultiplayerManager } = loadModules();
  const files = fixture(), log = journal(false), a = files.workspace('a');
  const published = [];
  let saved;
  const host = new MultiplayerManager({ binary: binaries.peer, chain: a.chain, deviceDirectory: a.device,
    githubToken: async () => { throw new Error('no account in this repro'); }, journal: log,
    saveSpace: async () => {}, saveSession: async session => { saved = session; }, changed: value => published.push(value.message),
    relay: { host: (incoming, failed) => new RelayHost(new FakeManagement(store, behavior), log, incoming, failed),
      client: () => ({ connect: async () => { throw new Error('unused'); }, stop: async () => {} }),
      remove: async () => {} } });
  const guest = new MultiplayerManager({ binary: binaries.peer, chain: path.join(files.directory, 'guest-chain'),
    deviceDirectory: path.join(files.directory, 'guest-device'), githubToken: async () => { throw new Error('unused'); },
    journal: log, saveSpace: async () => {}, saveSession: async () => {}, changed: () => {} });
  return { store, journal: log, host, guest, a, files, published, saved: () => saved };
}

test('R1: a Disconnected status before a rejected connect still deletes the created tunnel', { timeout: 30_000 }, async () => {
  const env = hosted({ statuses: true });
  try {
    await env.a.start();
    for (const attempt of [1, 2]) await assert.rejects(env.host.hostHistory(await env.guest.joinRequest(), false));
    assert.equal(env.store.created.length, 2, 'both attempts created a tunnel');
    assert.deepEqual(env.store.deleted, env.store.created, 'every created tunnel must be deleted');
    assert.equal(env.journal.remembered.size, 0, 'the journal must not retain a leaked marker');
    assert.ok(sdk.hosts.length > 0 && sdk.hosts.every(host => host.enableE2EEncryption === true && host.forwardConnectionsToLocalPorts === false),
      'hosting must keep E2E encryption and no local port forwarding');
    await env.host.stop();
    assert.equal(env.store.deleteAttempts.length, 2, 'an explicit Stop must not repeat a completed removal');
  } finally { env.files.stop(); }
});

test('R2: a rejected connect without a Disconnected status still deletes the created tunnel', { timeout: 30_000 }, async () => {
  const env = hosted({ statuses: false });
  try {
    await env.a.start();
    await assert.rejects(env.host.hostHistory(await env.guest.joinRequest(), false));
    assert.deepEqual(env.store.deleted, env.store.created);
    assert.equal(env.journal.remembered.size, 0);
  } finally { env.files.stop(); }
});

test('R3: suspend preserves the lease and a reload reuses the same tunnel', async () => {
  const env = relay({ succeed: true });
  const host = env.create();
  await host.start();
  const lease = host.lease();
  await host.suspend();
  assert.deepEqual(env.store.deleted, [], 'suspend must not delete the tunnel');
  assert.equal(env.journal.remembered.size, 1, 'suspend must retain the journal marker');
  const reload = env.create();
  await reload.start(lease);
  assert.equal(env.store.created.length, 1, 'a reload must not create a second tunnel');
  assert.deepEqual(env.store.getTunnel.map(value => value.tunnelId), [lease.tunnelId]);
  await reload.stop();
  assert.deepEqual(env.store.deleted, [lease.tunnelId]);
  assert.equal(env.journal.remembered.size, 0);
});

test('R4: stop after a completed suspend deletes once and stays idempotent', async () => {
  for (const form of ['instance', 'factory']) {
    const behavior = { succeed: true };
    const store = state();
    sdk.behavior = behavior;
    const { RelayHost } = loadModules();
    const log = journal(false);
    const host = form === 'instance'
      ? new RelayHost(new FakeManagement(store, behavior), log, () => {}, () => {})
      : new RelayHost(() => new FakeManagement(store, behavior), log, () => {}, () => {});
    await host.start();
    await host.suspend();
    await host.stop();
    assert.deepEqual(store.deleted, ['tunnel-1'], `${form}: stop deletes the suspended tunnel`);
    assert.equal(log.remembered.size, 0, `${form}: stop forgets the marker`);
    await host.stop();
    assert.equal(store.deleteAttempts.length, 1, `${form}: a repeated Stop is idempotent`);
    // A factory lets suspend release the client and stop recreate one for deletion.
    assert.equal(store.disposed, form === 'factory' ? 2 : 1, `${form}: client lifecycle`);
  }
});

test('R5: stop during an in-flight suspend deletes the tunnel exactly once', async () => {
  let release; const gate = new Promise(resolve => { release = resolve; });
  const behavior = { succeed: true, hostDisposeGate: gate };
  const env = relay(behavior);
  const host = env.create();
  await host.start();
  const suspending = host.suspend();
  await until(() => behavior.hostDisposeStarted === 1, 'suspend did not start');
  const stopping = host.stop();
  release();
  await Promise.all([suspending, stopping]);
  assert.deepEqual(env.store.deleted, ['tunnel-1']);
  assert.equal(env.journal.remembered.size, 0);
  assert.equal(env.store.deleteAttempts.length, 1);
});

test('R6: a failed removal is retried by the next Stop', async () => {
  const env = relay({ succeed: true, deleteFails: 1 });
  const host = env.create();
  await host.start();
  await assert.rejects(host.stop());
  assert.equal(env.journal.remembered.size, 1, 'the marker must survive a pending cleanup');
  assert.deepEqual(env.store.deleted, [], 'the failed attempt deleted nothing');
  await host.stop();
  assert.deepEqual(env.store.deleted, ['tunnel-1'], 'the retry deletes the tunnel');
  assert.equal(env.journal.remembered.size, 0);
  assert.equal(env.store.deleteAttempts.length, 2);
  await host.stop();
  assert.equal(env.store.deleteAttempts.length, 2, 'a completed removal is not repeated');
});

test('R7: a failed host teardown still deletes the tunnel and is retryable', async () => {
  const behavior = { succeed: true, hostDisposeFails: 1 };
  const env = relay(behavior);
  const host = env.create();
  await host.start();
  await assert.rejects(host.stop(), /Closing the relay host failed/);
  assert.deepEqual(env.store.deleted, ['tunnel-1'], 'resource removal must not depend on host teardown');
  assert.equal(env.journal.remembered.size, 0);
  await host.stop();
  assert.equal(env.store.deleteAttempts.length, 1, 'the retry must not delete twice');
  assert.equal(behavior.hostDisposeStarted, 2, 'the failed host disposal must be retried');
});

test('R8: a foreign journal owner blocks any relay resource access', async () => {
  const env = relay({ succeed: true, rejectOwnership: true });
  const host = env.create();
  await assert.rejects(host.start({ marker: 'editchain-multiplayer-' + 'a'.repeat(24), tunnelId: 'other-tunnel', clusterId: 'use' }));
  assert.deepEqual(env.store.created, [], 'must not create a tunnel it cannot own');
  assert.deepEqual(env.store.getTunnel, [], 'must not read another window resource');
  assert.deepEqual(env.store.deleteAttempts, [], 'must not delete another window resource');
  assert.equal(env.store.disposed, 1, 'an ownership rejection must release the local client');
});

test('R9: an explicit Stop retries a removal that failed during the host attempt', { timeout: 30_000 }, async () => {
  // RelayHost.start retries once internally; the second failure reaches the manager.
  const env = hosted({ statuses: true, deleteFails: 2 });
  try {
    await env.a.start();
    await assert.rejects(env.host.hostHistory(await env.guest.joinRequest(), false));
    assert.deepEqual(env.store.deleted, [], 'precondition: the removal is still pending');
    assert.equal(env.journal.remembered.size, 1, 'precondition: the marker is still pending');
    await env.host.stop();
    assert.deepEqual(env.store.deleted, ['tunnel-1'], 'the explicit Stop must retry the removal');
    assert.equal(env.journal.remembered.size, 0);
    assert.equal(env.store.deleteAttempts.length, 3);
  } finally { env.files.stop(); }
});

test('R10: an explicit Stop retries a removal that failed while hosting', { timeout: 30_000 }, async () => {
  const env = hosted({ statuses: false, succeed: true, deleteFails: 1 });
  try {
    await env.a.start();
    await env.host.hostHistory(await env.guest.joinRequest(), false);
    assert.ok(env.host.status().hosting, 'precondition: hosting started');
    await assert.rejects(env.host.stop());
    assert.deepEqual(env.store.deleted, [], 'precondition: the first removal failed');
    assert.equal(env.journal.remembered.size, 1, 'precondition: the marker is still pending');
    await env.host.stop();
    assert.deepEqual(env.store.deleted, ['tunnel-1'], 'the repeated Stop must retry the removal');
    assert.equal(env.journal.remembered.size, 0);
    assert.equal(env.store.deleteAttempts.length, 2);
  } finally { env.files.stop(); }
});

test('R11: a failed client disposal is retried without re-deleting the tunnel', async () => {
  const env = relay({ succeed: true, disposeError: 'dispose unavailable' });
  const host = env.create();
  await host.start();
  await assert.rejects(host.stop());
  assert.deepEqual(env.store.deleted, ['tunnel-1'], 'the tunnel removal still happened');
  assert.equal(env.journal.remembered.size, 0);
  assert.equal(env.store.disposeAttempts, 1, 'precondition: the first disposal failed');
  delete sdk.behavior.disposeError;
  await host.stop();
  assert.equal(env.store.deleteAttempts.length, 1, 'the retry must not delete the tunnel twice');
  assert.equal(env.store.disposeAttempts, 2, 'the failed client disposal must be retried');
  assert.equal(env.store.disposed, 1);
});

test('R12: an explicit Stop waits for a first host still in disconnect cleanup', { timeout: 30_000 }, async () => {
  let release; const gate = new Promise(resolve => { release = resolve; });
  const env = hosted({ statuses: true, hostDisposeGate: gate });
  try {
    await env.a.start();
    const hosting = env.host.hostHistory(await env.guest.joinRequest(), false);
    await until(() => sdk.behavior.hostDisposeStarted === 1, 'disconnect cleanup did not start');
    const stopping = env.host.stop();
    let settled = false; stopping.then(() => { settled = true; }, () => { settled = true; });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(settled, false, 'Stop must wait for in-flight resource cleanup');
    release();
    await assert.rejects(hosting);
    await stopping;
    assert.deepEqual(env.store.deleted, env.store.created, 'the created tunnel must be deleted');
    assert.equal(env.journal.remembered.size, 0);
  } finally { env.files.stop(); }
});

test('R13: an explicit Stop surfaces and retries a deletion failure for that host', { timeout: 30_000 }, async () => {
  let release; const gate = new Promise(resolve => { release = resolve; });
  const env = hosted({ statuses: true, hostDisposeGate: gate, deleteError: 'deletion unavailable' });
  try {
    await env.a.start();
    const hosting = env.host.hostHistory(await env.guest.joinRequest(), false);
    await until(() => sdk.behavior.hostDisposeStarted === 1, 'disconnect cleanup did not start');
    const stopping = env.host.stop();
    release();
    await assert.rejects(hosting);
    await assert.rejects(stopping, /cleanup/i);
    assert.deepEqual(env.store.deleted, [], 'precondition: the deletion failed');
    assert.equal(env.journal.remembered.size, 1, 'precondition: the marker is still pending');
    delete sdk.behavior.deleteError;
    await env.host.stop();
    assert.deepEqual(env.store.deleted, ['tunnel-1'], 'the repeated Stop must retry the deletion');
    assert.equal(env.journal.remembered.size, 0);
  } finally { env.files.stop(); }
});

test('R14: cleanup failures never expose SDK error text', async () => {
  const secret = 'gho_sentinel-secret-0123456789';
  const sanitized = pattern => error => {
    assert.ok(!String(error?.message).includes(secret), `SDK error text leaked: ${error?.message}`);
    assert.match(String(error?.message), pattern);
    return true;
  };
  const cases = [
    ['delete', { succeed: true, deleteError: secret }, /Tunnel cleanup is pending/],
    ['management dispose', { succeed: true, disposeError: secret }, /Tunnel cleanup is pending/],
    ['host dispose', { succeed: true, hostDisposeError: secret }, /Closing the relay host failed/],
  ];
  for (const [name, behavior, pattern] of cases) {
    const env = relay(behavior);
    const host = env.create();
    await host.start();
    await assert.rejects(host.stop(), sanitized(pattern), `${name} failure must be sanitized`);
  }
  const env = relay({ succeed: true, deleteError: secret });
  const host = env.factory();
  await host.start();
  await host.suspend();
  await assert.rejects(host.stop(), sanitized(/Tunnel cleanup is pending/), 'factory cleanup failure must be sanitized');
});

test('R15: Stop waits for a startup cleanup retry and keeps its final status', { timeout: 30_000 }, async () => {
  let release; const gate = new Promise(resolve => { release = resolve; });
  const env = hosted({ statuses: true, deleteFails: 1,
    beforeDelete: attempt => attempt === 2 ? gate : undefined });
  try {
    await env.a.start();
    const hosting = assert.rejects(env.host.hostHistory(await env.guest.joinRequest(), false));
    await until(() => env.store.deleteAttempts.length === 2, 'startup did not retry its failed deletion');
    const stopping = env.host.stop();
    let settled = false; stopping.then(() => { settled = true; }, () => { settled = true; });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(settled, false, 'Stop must wait for the second deletion attempt');
    release();
    await Promise.all([hosting, stopping]);
    assert.deepEqual(env.store.deleted, env.store.created);
    assert.equal(env.store.deleteAttempts.length, 2, 'Stop shares the pending deletion');
    assert.equal(env.journal.remembered.size, 0);
    assert.equal(env.host.status().message, 'Sharing stopped.');
  } finally { release(); env.files.stop(); }
});

test('R16: a resumed startup that loses to Stop leaves the stopped status', { timeout: 30_000 }, async () => {
  let release; const gate = new Promise(resolve => { release = resolve; });
  const env = hosted({ statuses: false, succeed: true });
  try {
    await env.a.start();
    await env.host.hostHistory(await env.guest.joinRequest(), false);
    const session = env.saved();
    assert.ok(session?.host, 'precondition: a host session was saved');
    await env.host.suspend();
    // The resumed startup now fails and its cleanup is held open across the Stop.
    sdk.behavior = { statuses: false, succeed: false, hostDisposeGate: gate };
    const resuming = env.host.resume(session);
    await until(() => sdk.behavior.hostDisposeStarted === 1, 'resumed startup did not reach its cleanup');
    const stopping = env.host.stop();
    release();
    await assert.rejects(resuming, /Sharing was stopped/);
    await stopping;
    assert.equal(env.host.status().message, 'Sharing stopped.', 'a retired resume must not rewrite the stopped status');
    assert.equal(env.published[env.published.length - 1], 'Sharing stopped.', 'the stopped status must remain the last published state');
    assert.ok(!env.published.includes('Hosting is unavailable; retrying automatically.'), 'no automatic-retry status may be published after Stop');
  } finally { release(); env.files.stop(); }
});
