'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { Duplex } = require('node:stream');
const { NativeWorker, PeerBridge } = require('../../out/multiplayer/native');
const { LiveSync } = require('../../out/liveSync');
const { fixture, binaries, until } = require('./multiplayerFixture');

async function control(body) {
  const worker = new NativeWorker(binaries.peer);
  try { return await worker.request(body); } finally { worker.stop(); }
}

test('a blocked outgoing acknowledgment cannot delay notification of received history', { timeout: 15_000 }, async () => {
  const files = fixture(), bridges = [], progress = [], receipts = [];
  let blocked = false, holdReceiverWrite = false;
  try {
    const locals = [files.workspace('left'), files.workspace('right')];
    const devices = [];
    for (const local of locals) { await local.start(); devices.push(await control({ type: 'identity', device_dir: local.device })); }
    for (const [index, local] of locals.entries()) {
      await control({ type: 'configure', chain_dir: local.chain, space: 'live-receipts', backfill: false });
      await control({ type: 'approve', chain_dir: local.chain, space: 'live-receipts', certificate: devices[1 - index].certificate });
    }
    const streams = [0, 1].map(index => new Duplex({ read() {}, write(bytes, _encoding, done) {
      if (index === 1 && holdReceiverWrite) { blocked = true; return; }
      streams[1 - index].push(bytes); done();
    } }));
    for (const [index, local] of locals.entries()) bridges.push(new PeerBridge(binaries.peer, streams[index], {
      chain_dir: local.chain, device_dir: local.device, space: 'live-receipts', remote: index ? devices[0].certificate : undefined,
    }, (value, _device, durable) => { progress[index] = value; if (index === 1 && durable) receipts.push(value); }, () => {}, 100));
    // Observe the real worker result only to choose which transport write to
    // stall. Native storage, TLS, replication, and the public callback are real.
    const worker = bridges[1].worker, request = worker.request.bind(worker);
    worker.request = async body => {
      const result = await request(body);
      if (result.progress?.records > 0) holdReceiverWrite = true;
      return result;
    };
    await Promise.all(bridges.map(bridge => bridge.start()));
    await until(() => progress.length === 2 && progress.every(value => value.accepted), 'peers did not authenticate');
    await locals[0].edit('before\n', 'new shared edit\n');
    await until(() => blocked, 'receiver did not reach the blocked acknowledgment');
    assert.ok(receipts.some(value => value.records > 0), 'the view must hear about durable records while the ACK write is blocked');
    assert.equal(progress[0].sent_records, 0, 'sender cannot count the undelivered acknowledgment');
  } finally { for (const bridge of bridges) bridge.stop(); files.stop(); }
});

test('an already open live view resolves content received after records without reopening', async () => {
  const files = fixture();
  try {
    const a = files.workspace('sender'), b = files.workspace('receiver');
    await a.start(); await a.edit('before remote edit\n', 'after remote edit\n');
    const opened = await b.call({ OpenLivePaged: { workspace_path: b.root, chain_dir: '.editchain' } });
    let snapshot = opened.snapshot_id, revision = opened.live.revision;
    const sync = async () => {
      const update = await b.call({ SyncLive: { epoch: opened.live.epoch, after_revision: revision, codex: null } });
      revision = update.revision;
      if (update.deltas.length) snapshot = update.deltas.at(-1).snapshot_id;
      return update;
    };
    // Model the protocol's two durable phases deterministically. The receiver
    // has no local captures or blobs directory; its live epoch stays open.
    for (const name of fs.readdirSync(a.chain).filter(name => name.endsWith('.eclog'))) {
      fs.copyFileSync(path.join(a.chain, name), path.join(b.chain, name));
    }
    assert.ok((await sync()).deltas.length > 0, 'received records must publish live rows');
    const window = await b.call({ GetWindow: { snapshot_id: snapshot, offset: 0, limit: 200, include_layout: false } });
    const change = window.rows.find(row => row.file_change?.path === 'shared.ts')?.file_change;
    assert.ok(change, 'received edit must be visible in the original live epoch');
    const before = await b.call({ GetFileDiff: { snapshot_id: snapshot, change } });
    assert.equal(before.partial, true, 'content is not available before it arrives');
    fs.mkdirSync(path.join(b.chain, 'blobs'));
    for (const name of fs.readdirSync(path.join(a.chain, 'blobs'))) {
      fs.copyFileSync(path.join(a.chain, 'blobs', name), path.join(b.chain, 'blobs', name));
    }
    const update = await sync();
    assert.equal(update.work.chain_records, 0, 'content hydration must not require new records');
    const after = await b.call({ GetFileDiff: { snapshot_id: snapshot, change } });
    assert.equal(after.before, 'before remote edit\n');
    assert.equal(after.after, 'after remote edit\n');
    assert.equal(after.partial, false);
    assert.equal(fs.readFileSync(path.join(b.root, 'shared.ts'), 'utf8'), 'Working tree stays local.\n');
  } finally { files.stop(); }
});

test('a missing Codex helper names the executable and leaves received rows live', { timeout: 15_000 }, async () => {
  const files = fixture(), statuses = [];
  let loop;
  try {
    const a = files.workspace('sender'), b = files.workspace('receiver');
    await a.start(); await a.edit('before\n', 'received while Codex is unavailable\n');
    const opened = await b.call({ OpenLivePaged: { workspace_path: b.root, chain_dir: '.editchain' } });
    for (const name of fs.readdirSync(a.chain).filter(name => name.endsWith('.eclog'))) {
      fs.copyFileSync(path.join(a.chain, name), path.join(b.chain, name));
    }
    let revision = opened.live.revision, snapshot = opened.snapshot_id, providerError;
    const missingHelper = path.join(files.directory, 'missing-codex-session-exporter');
    const sync = async codex => {
      const response = await b.client.request({ SyncLive: { epoch: opened.live.epoch, after_revision: revision, codex } });
      if (response.Error) { providerError = response.Error.message; throw new Error(providerError); }
      revision = response.Ok.revision;
      if (response.Ok.deltas.length) snapshot = response.Ok.deltas.at(-1).snapshot_id;
    };
    loop = new LiveSync({
      capture: async () => ({ sessions: new Map([['rollout.jsonl', '1']]), titles: '', history: '' }),
      importFiles: paths => sync({ sessions_root: files.directory, helper: missingHelper, paths }),
      publish: () => sync(null), pollNative: true, status: value => statuses.push(value),
    }, 60_000);
    loop.wake();
    await until(() => statuses.at(-1)?.startsWith('Live · Codex import retry:'), 'provider failure did not preserve live history');
    assert.match(providerError, /codex helper .* could not be spawned/);
    assert.ok(providerError.includes(missingHelper), 'diagnostic identifies the unavailable executable');
    assert.ok(revision > opened.live.revision, 'queued external records publish after the failed provider call');
    const window = await b.call({ GetWindow: { snapshot_id: snapshot, offset: 0, limit: 200, include_layout: false } });
    assert.ok(window.rows.some(row => row.file_change?.path === 'shared.ts'));
  } finally { loop?.dispose(); files.stop(); }
});
