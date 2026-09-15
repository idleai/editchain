'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { randomUUID, randomBytes } = require('node:crypto');
const { Duplex, PassThrough } = require('node:stream');
const { MultiplayerManager } = require('../../out/multiplayer/manager');
const { parseInvitation, savedInvitation } = require('../../out/multiplayer/invitation');
const { fixture, binaries, until, blobs, diffs } = require('./multiplayerFixture');

// Fault-inject only the byte transport. Managers, TLS, native workers and stores are real.
function network() {
  const hosts = new Map(), streams = new Set();
  let attempts = 0, starts = 0;
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
    terminateHost() { for (const host of [...hosts.values()]) host.failed('terminal fixture disconnect', true); }, starts: () => starts };
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
