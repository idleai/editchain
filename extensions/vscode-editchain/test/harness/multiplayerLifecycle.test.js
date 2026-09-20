'use strict';
// Explicit Stop must win over an in-flight Remove. This file drives the real
// MultiplayerManager with real native workers; only the relay byte transport is
// fault-injected. The commands-layer companion lives in
// multiplayerCommandsLifecycle.test.js (kept separate so the fixture's stubbed
// `vscode` binding cannot leak into the command module cache).
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { randomUUID, randomBytes } = require('node:crypto');
const { Duplex, PassThrough } = require('node:stream');
const { MultiplayerManager } = require('../../out/multiplayer/manager');
const { fixture, binaries, until } = require('./multiplayerFixture');

// Fault-inject only the byte transport. Managers, TLS, native workers and stores
// are real. Only the operations these regressions exercise are modeled.
function network() {
  const hosts = new Map();
  const provider = {
    host(incoming) {
      let lease;
      const owned = new Set();
      const suspend = async () => {
        for (const stream of owned) stream.destroy();
        if (hosts.get(lease?.tunnelId)?.incoming === incoming) hosts.delete(lease.tunnelId);
      };
      return {
        async start(previous) {
          lease = previous ?? { marker: 'editchain-multiplayer-' + randomBytes(12).toString('hex'), tunnelId: randomUUID(), clusterId: 'use' };
          hosts.set(lease.tunnelId, { incoming, owned });
        },
        lease: () => lease,
        async descriptor() {
          const expiresAt = Date.now() + 60_000;
          const connectToken = ['e30', Buffer.from(JSON.stringify({ exp: Math.ceil(expiresAt / 1000) + 3600 })).toString('base64url'), 'signature'].join('.');
          return { endpoint: { tunnelId: lease.tunnelId, clusterId: lease.clusterId, hostId: 'host', hostPublicKeys: ['YWJj'], clientRelayUri: 'wss://use.rel.tunnels.api.visualstudio.com/test' }, connectToken, expiresAt };
        },
        suspend, stop: suspend,
      };
    },
    client() {
      let stream;
      return {
        async connect(invitation) {
          const host = hosts.get(invitation.endpoint.tunnelId);
          if (!host) throw new Error('fixture host offline');
          const a = new PassThrough({ highWaterMark: 1024 }), b = new PassThrough({ highWaterMark: 1024 });
          const remote = Duplex.from({ readable: a, writable: b });
          stream = Duplex.from({ readable: b, writable: a });
          for (const item of [remote, stream]) {
            item.on('error', () => {}); host.owned.add(item);
            item.once('close', () => host.owned.delete(item));
          }
          host.incoming(remote); return stream;
        },
        async stop() { stream?.destroy(); },
      };
    },
    async remove(lease) { hosts.delete(lease.tunnelId); },
  };
  return { provider };
}

function environment() {
  const files = fixture(), wire = network(), saved = new Map(), spaces = new Map(), managers = new Set();
  const create = local => {
    const manager = new MultiplayerManager({ binary: binaries.peer, chain: local.chain, deviceDirectory: local.device,
      space: spaces.get(local.root), relay: wire.provider, githubToken: async () => { throw new Error('no real service'); },
      journal: { remember: async () => {}, forget: async () => {} }, changed: () => {},
      saveSpace: async space => { spaces.set(local.root, space); }, saveSession: async session => { saved.set(local.root, session); } });
    managers.add(manager); return manager;
  };
  return { files, wire, saved, spaces, create, async stop() { await Promise.all([...managers].map(manager => manager.stop())); files.stop(); } };
}
const live = manager => manager.status().peers.filter(peer => peer.state === 'Live').length;

async function shared(env) {
  const a = env.files.workspace('a'), b = env.files.workspace('b');
  await a.start(); await b.start();
  const host = env.create(a), guest = env.create(b);
  await guest.joinHistory(await host.hostHistory(await guest.joinRequest(), true), true);
  await until(() => live(guest) === 1, 'initial edge');
  return { a, host };
}

test('A1: a Remove finishing after Stop leaves no saved session', { timeout: 30_000 }, async () => {
  const env = environment();
  try {
    const { a, host } = await shared(env);
    const approved = await host.devices();
    assert.equal(approved.length, 1);
    const revoking = host.revoke(approved[0].fingerprint);
    await host.stop();
    assert.equal(host.status().enabled, false);
    assert.equal(env.saved.get(a.root), undefined, 'Stop must clear the saved session');
    await revoking;
    assert.equal(env.saved.get(a.root), undefined, 'the late Remove re-saved the retired session');
  } finally { await env.stop(); }
});

test('A2: a stale Remove cannot rewrite a newer sharing session', { timeout: 30_000 }, async () => {
  const env = environment();
  try {
    const { a, host } = await shared(env);
    const approved = await host.devices();
    const captured = env.saved.get(a.root);
    assert.ok(captured?.host, 'precondition: a host session was saved');
    // Hold the native revocation so the Stop and a fresh Host both finish first.
    const native = host.control.bind(host);
    let release; const held = new Promise(resolve => { release = resolve; });
    host.control = async body => { if (body.type === 'revoke') await held; return native(body); };
    const revoking = host.revoke(approved[0].fingerprint);
    await host.stop();
    const fresh = env.create(a);
    await fresh.resume(captured);
    const newer = env.saved.get(a.root);
    assert.ok(newer?.host, 'precondition: the fresh session persisted');
    release();
    await revoking;
    assert.deepEqual(env.saved.get(a.root), newer, 'the retired Remove rewrote the newer session');
  } finally { await env.stop(); }
});

test('A3: a valid Remove still persists the revoked outbound invitation', { timeout: 30_000 }, async () => {
  const env = environment();
  try {
    const a = env.files.workspace('a'), b = env.files.workspace('b');
    await a.start(); await b.start();
    const host = env.create(a), guest = env.create(b);
    await guest.joinHistory(await host.hostHistory(await guest.joinRequest(), true), true);
    await until(() => live(guest) === 1, 'initial edge');
    const before = env.saved.get(b.root);
    assert.equal(before?.peers.length, 1, 'precondition: the guest saved its outbound invitation');
    const fingerprint = before.peers[0].host.fingerprint;
    assert.ok((await guest.devices()).some(device => device.fingerprint === fingerprint), 'precondition: the host is an approved member');
    await guest.revoke(fingerprint);
    const after = env.saved.get(b.root);
    assert.ok(after, 'the guest session must still be saved after removing one peer');
    assert.deepEqual(after.peers, [], 'the removed outbound invitation must not remain in the saved session');
    assert.ok(!(await guest.devices()).some(device => device.fingerprint === fingerprint), 'the removed member must be gone from native membership');
  } finally { await env.stop(); }
});

test('A4: suspend during a Remove preserves the private session', { timeout: 30_000 }, async () => {
  const env = environment();
  try {
    const { a, host } = await shared(env);
    const approved = await host.devices();
    const revoking = host.revoke(approved[0].fingerprint);
    await host.suspend();
    await revoking;
    const kept = env.saved.get(a.root);
    assert.ok(kept?.host, 'suspend must keep the private host session for the next window');
    assert.equal(host.status().enabled, false);
  } finally { await env.stop(); }
});

test('A5: a stale connect approval check cannot delete a peer after suspend', { timeout: 30_000 }, async () => {
  const env = environment();
  try {
    const a = env.files.workspace('a'), b = env.files.workspace('b');
    await a.start(); await b.start();
    const host = env.create(a), guest = env.create(b);
    await guest.joinHistory(await host.hostHistory(await guest.joinRequest(), true), true);
    await until(() => live(guest) === 1, 'initial edge');
    const fingerprint = (await guest.devices())[0].fingerprint;
    // Withdraw native approval only; the in-memory outbound peer entry must survive.
    await guest.control({ type: 'revoke', chain_dir: b.chain, space: guest.status().space, fingerprint });
    const native = guest.control.bind(guest);
    let release; const held = new Promise(resolve => { release = resolve; });
    let armed = true;
    guest.control = async body => {
      if (body.type === 'devices' && armed) { armed = false; await held; }
      return native(body);
    };
    const connect = guest.connect.bind(guest);
    let reconnect;
    guest.connect = (...args) => { reconnect = connect(...args); return reconnect; };
    void guest.reconnect().catch(() => {});
    await until(() => !armed, 'the connect approval check did not run');
    assert.ok(reconnect, 'the pending connection must be observed');
    await guest.suspend();
    release();
    await reconnect;
    assert.ok(guest.status().peers.some(peer => peer.fingerprint === fingerprint), 'the stale connect deleted a peer preserved by suspend');
  } finally { await env.stop(); }
});
