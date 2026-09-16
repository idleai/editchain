'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { randomUUID, randomBytes } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { Duplex, PassThrough } = require('node:stream');
const { MultiplayerManager } = require('../../out/multiplayer/manager');
const { parseInvitation, savedInvitation } = require('../../out/multiplayer/invitation');
const { fixture, binaries, until, blobs, diffs } = require('./multiplayerFixture');

// Fault-inject only the byte transport. Managers, TLS, native workers and stores are real.
function network() {
  const hosts = new Map(), streams = new Set();
  let attempts = 0, starts = 0, lastEndpoint;
  const provider = {
    host(incoming, failed) {
      let lease;
      const owned = new Set();
      const suspend = async () => {
        for (const stream of owned) stream.destroy();
        if (hosts.get(lease?.tunnelId)?.incoming === incoming) hosts.delete(lease.tunnelId);
      };
      return {
        async start(previous) {
          starts++;
          lease = previous ?? { marker: 'editchain-multiplayer-' + randomBytes(12).toString('hex'), tunnelId: randomUUID(), clusterId: 'use' };
          hosts.set(lease.tunnelId, { incoming, owned, failed });
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
          attempts++;
          lastEndpoint = invitation.endpoint;
          const host = hosts.get(invitation.endpoint.tunnelId);
          if (!host) throw new Error('fixture host offline');
          const a = new PassThrough({ highWaterMark: 1024 }), b = new PassThrough({ highWaterMark: 1024 });
          const remote = Duplex.from({ readable: a, writable: b });
          stream = Duplex.from({ readable: b, writable: a });
          for (const item of [remote, stream]) {
            item.on('error', () => {}); streams.add(item); host.owned.add(item);
            item.once('close', () => { streams.delete(item); host.owned.delete(item); });
          }
          host.incoming(remote); return stream;
        },
        async stop() { stream?.destroy(); },
      };
    },
    async remove(lease) { hosts.delete(lease.tunnelId); },
  };
  return { provider, drop() { for (const stream of streams) stream.destroy(); }, attempts: () => attempts,
    terminateHost() { for (const host of [...hosts.values()]) host.failed('terminal fixture disconnect', true); }, starts: () => starts, endpoint: () => lastEndpoint };
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

test('durable space and private baseline survive lost workspace metadata and a moved chain', async () => {
  const env = environment();
  try {
    const a = env.files.workspace('a'), b = env.files.workspace('b');
    await a.start();
    const host = env.create(a), guest = env.create(b), request = await guest.joinRequest();
    const first = parseInvitation(await host.hostHistory(request, false));
    const ledger = fs.readFileSync(path.join(a.chain, 'multiplayer/scope.json'));
    assert.ok(JSON.parse(ledger).excluded.length > 0);
    await host.suspend();
    env.spaces.delete(a.root);
    const restored = env.create(a);
    const second = parseInvitation(await restored.hostHistory(request, false));
    assert.equal(second.space, first.space);
    assert.deepEqual(fs.readFileSync(path.join(a.chain, 'multiplayer/scope.json')), ledger);
    const saved = env.saved.get(a.root);
    await restored.suspend();
    const moved = { ...a, chain: path.join(env.files.directory, 'moved-chain') };
    fs.renameSync(a.chain, moved.chain);
    env.spaces.delete(a.root);
    const resumed = env.create(moved);
    await resumed.resume(saved);
    assert.equal(resumed.status().space, first.space);
    assert.equal(resumed.status().hosting, true);
    assert.equal((await resumed.devices()).length, 1);
    assert.deepEqual(fs.readFileSync(path.join(moved.chain, 'multiplayer/scope.json')), ledger);
  } finally { await env.stop(); }
});

test('stale workspace metadata cannot replace a durable space binding', async () => {
  const env = environment();
  try {
    const a = env.files.workspace('a'), b = env.files.workspace('b');
    const host = env.create(a), guest = env.create(b), request = await guest.joinRequest();
    await host.hostHistory(request, false);
    await host.suspend();
    const ledger = fs.readFileSync(path.join(a.chain, 'multiplayer/scope.json'));
    env.spaces.set(a.root, 'wrong-space');
    const stale = env.create(a);
    await assert.rejects(stale.hostHistory(request, false), /different collaboration space/);
    assert.deepEqual(fs.readFileSync(path.join(a.chain, 'multiplayer/scope.json')), ledger);
  } finally { await env.stop(); }
});

test('automatic reconnect repairs a broken stream; restart never reenrolls a removed device', { timeout: 30_000 }, async () => {
  const env = environment();
  try {
    const a = env.files.workspace('a'), b = env.files.workspace('b');
    await a.start(); await b.start();
    const host = env.create(a), guest = env.create(b);
    const invitation = await host.hostHistory(await guest.joinRequest(), true);
    await guest.joinHistory(invitation, true);
    await until(() => live(guest) === 1, 'initial authentication');
    env.wire.drop();
    await a.edit('offline baseline\n', 'repair after disconnect\n');
    await until(() => env.wire.attempts() >= 2 && live(guest) === 1 && blobs(a.chain).every(name => blobs(b.chain).includes(name)), 'automatic repair');
    assert.ok((await diffs(b)).some(diff => diff.after === 'repair after disconnect\n'));
    env.wire.terminateHost();
    await a.edit('host retry baseline\n', 'host relay recovered\n');
    await until(() => env.wire.starts() >= 2 && live(guest) === 1 && blobs(a.chain).every(name => blobs(b.chain).includes(name)), 'host terminal recovery');
    const snapshot = env.saved.get(b.root);
    await guest.revoke(parseInvitation(invitation).host.fingerprint);
    await guest.suspend();
    const restarted = env.create(b);
    await restarted.resume(snapshot);
    assert.deepEqual(await restarted.devices(), []);
    assert.deepEqual(restarted.status().peers, []);
  } finally { await env.stop(); }
});

test('simultaneous opposite invitations settle on one authenticated edge at both ends', { timeout: 30_000 }, async () => {
  const env = environment();
  try {
    const a = env.files.workspace('a'), b = env.files.workspace('b');
    await a.start(); await b.start();
    const left = env.create(a), initial = env.create(b);
    const toB = await left.hostHistory(await initial.joinRequest(), true);
    env.spaces.set(b.root, parseInvitation(toB).space);
    await initial.stop();
    const right = env.create(b);
    const toA = await right.hostHistory(await left.joinRequest(), true);
    await Promise.all([left.joinHistory(toA, true), right.joinHistory(toB, true)]);
    await until(() => live(left) === 1 && live(right) === 1 && left.status().peers.length === 1 && right.status().peers.length === 1, 'duplicates did not settle');
    await a.edit('mesh before\n', 'mesh after\n');
    await until(() => blobs(a.chain).every(name => blobs(b.chain).includes(name)), 'settled edge stopped transferring');
    assert.ok((await diffs(b)).some(diff => diff.after === 'mesh after\n'));
  } finally { await env.stop(); }
});

test('saved grants outlive the initial approval window but expired grants cannot reconnect', async () => {
  const env = environment();
  try {
    const a = env.files.workspace('a'), b = env.files.workspace('b');
    const left = env.create(a), right = env.create(b);
    const encoded = await left.hostHistory(await right.joinRequest(), true);
    const invitation = parseInvitation(encoded), later = invitation.expiresAt + 1;
    assert.throws(() => parseInvitation(encoded, later), /expired/);
    assert.equal(savedInvitation(invitation, later).guest, invitation.guest);
    assert.throws(() => savedInvitation(invitation, later + 7200_000), /expired/);
  } finally { await env.stop(); }
});

test('discovery refreshes a known endpoint without admitting unknown devices or moving a grant to another tunnel', { timeout: 30_000 }, async () => {
  const env = environment();
  try {
    const a = env.files.workspace('a'), b = env.files.workspace('b'), c = env.files.workspace('c');
    const host = env.create(a), guest = env.create(b), stranger = env.create(c);
    await guest.joinHistory(await host.hostHistory(await guest.joinRequest(), true), true);
    await until(() => live(guest) === 1, 'initial discovery edge');
    const ad = await host.describe();
    assert.ok(ad);
    assert.ok(!('connectToken' in ad));
    const unknown = (await stranger.inspectRequest(await stranger.joinRequest())).device;
    await guest.discover([
      { ...ad, device: unknown },
      { ...ad, device: { ...ad.device, fingerprint: 'f'.repeat(64) } },
      { ...ad, space: 'wrong-space' },
      { ...ad, endpoint: { ...ad.endpoint, tunnelId: 'another-resource' } },
      { ...ad, expiresAt: 1 },
    ]);
    assert.deepEqual(await guest.devices(), [ad.device]);
    assert.equal(guest.status().peers.length, 1);
    const refreshed = { ...ad, endpoint: { ...ad.endpoint, hostId: 'fresh-host-instance' } };
    await guest.discover([refreshed]);
    await guest.reconnect();
    await until(() => live(guest) === 1 && env.wire.endpoint().hostId === 'fresh-host-instance', 'known endpoint did not refresh');
    assert.equal(env.wire.endpoint().tunnelId, ad.endpoint.tunnelId);
  } finally { await env.stop(); }
});
