'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { Duplex, PassThrough } = require('node:stream');
const { NativeWorker, PeerBridge } = require('../../out/multiplayer/native');
const { MultiplayerManager } = require('../../out/multiplayer/manager');
const { encodeInvitation, parseRequest, parseInvitation, validateEndpoint } = require('../../out/multiplayer/invitation');
const { FrameDecoder } = require('../../out/frameDecoder');
const { fixture, binaries, until, blobs, rows, diffs } = require('./multiplayerFixture');

async function control(body) {
  const worker = new NativeWorker(binaries.peer);
  try { return await worker.request(body); } finally { worker.stop(); }
}

test('opaque production bridges deliver captured history and historical content between independent stores', { timeout: 30_000 }, async () => {
  const files = fixture();
  const a = files.workspace('alice'), b = files.workspace('bob');
  const bridges = [];
  const failures = [];
  const progress = new Map();
  try {
    await a.start(); await b.start();
    const ai = await control({ type: 'identity', device_dir: a.device });
    const bi = await control({ type: 'identity', device_dir: b.device });
    for (const [local, remote] of [[a, bi], [b, ai]]) {
      await control({ type: 'configure', chain_dir: local.chain, space: 'bridge-space', backfill: true });
      await control({ type: 'approve', chain_dir: local.chain, space: 'bridge-space', certificate: remote.certificate });
    }
    await a.edit('before\n', 'A shared revision\n'.repeat(12_000));
    await b.edit('before B\n', 'B simultaneous revision\n'.repeat(12_000));
    const left = new PassThrough({ highWaterMark: 1024 }), right = new PassThrough({ highWaterMark: 1024 });
    const streams = [Duplex.from({ readable: left, writable: right }), Duplex.from({ readable: right, writable: left })];
    for (const [index, local, remote] of [[0, a, undefined], [1, b, ai.certificate]]) {
      const bridge = new PeerBridge(binaries.peer, streams[index], { chain_dir: local.chain, device_dir: local.device, space: 'bridge-space', remote },
        value => progress.set(local.root, value), error => { if (error) failures.push(error.message); }, 100);
      bridges.push(bridge);
    }
    await Promise.all(bridges.map(bridge => bridge.start()));
    await until(() => {
      assert.deepEqual(failures, [], 'bridge failed before content hydration');
      return progress.get(b.root)?.blobs >= 5 && blobs(a.chain).every(name => blobs(b.chain).includes(name));
    }, 'bridge did not hydrate captured content');
    assert.deepEqual(failures, []);
    const visible = await rows(b);
    assert.ok(visible.some(row => row.file_change?.path === 'shared.ts'), 'remote history has its native file-change affordance');
    assert.ok((await diffs(b)).some(diff => diff.before === 'before\n' && diff.after === 'A shared revision\n'.repeat(12_000)), 'native historical diff resolves exact remote revisions');
    assert.equal(fs.readFileSync(path.join(b.root, 'shared.ts'), 'utf8'), 'Working tree stays local.\n');
    // Local capture must still work while received source blobs/derivations exist.
    await b.edit('bob before\n', 'bob after\n');
    await until(() => {
      assert.deepEqual(failures, [], 'bridge failed during concurrent local capture');
      return blobs(b.chain).every(name => blobs(a.chain).includes(name));
    }, 'bidirectional capture did not settle');
    assert.deepEqual(failures, []);
  } finally { for (const bridge of bridges) bridge.stop(); files.stop(); }
});

test('invitation parsing refuses arbitrary endpoints, wrong devices and altered fingerprints', async () => {
  const files = fixture();
  const manager = new MultiplayerManager({ binary: binaries.peer, chain: path.join(files.directory, 'chain'),
    deviceDirectory: path.join(files.directory, 'device'), githubToken: async () => { throw new Error('network forbidden'); },
    journal: { remember: async () => {}, forget: async () => {} }, saveSpace: async () => {}, changed: () => {} });
  try {
    const request = await manager.joinRequest();
    const decoded = parseRequest(request);
    assert.equal((await manager.inspectRequest(request)).device.fingerprint, decoded.device.fingerprint);
    decoded.device.fingerprint = '0'.repeat(64);
    await assert.rejects(manager.inspectRequest(encodeInvitation(decoded)), /fingerprint/);
    assert.throws(() => validateEndpoint({ tunnelId: 'x', clusterId: 'use', hostId: 'h', hostPublicKeys: ['YWJj'], clientRelayUri: 'wss://127.0.0.1/secret' }), /Microsoft/);
    assert.throws(() => parseInvitation(encodeInvitation({ version: 1, kind: 'invite', expiresAt: 1 })), /expired/);
  } finally { await manager.stop(); files.stop(); }
});

test('native client rejects oversized responses before allocation and is cancellable', async () => {
  const decoder = new FrameDecoder(512 * 1024);
  const length = Buffer.alloc(4); length.writeUInt32LE(0xffffffff);
  assert.throws(() => [...decoder.push(length)], /limit/);
  const worker = new NativeWorker(binaries.peer);
  const pending = worker.request({ type: 'turn', bytes: '', tick: true });
  worker.stop();
  await assert.rejects(pending, /stopped/);
});
