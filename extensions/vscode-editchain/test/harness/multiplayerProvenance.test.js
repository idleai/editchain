'use strict';

// A peer can independently supply an exact copy of history this device authored
// and deliberately withheld. Receiving those records back is not evidence that
// the local recorder never authored them: on a cold derived-index rebuild the
// recorder must still replay its own baseline so dwell policy, buffer revisions
// and the stream frontier survive. This regression drives the real native
// workers, mutual TLS and the production manager over a disposable in-memory
// byte transport, then removes only the disposable `editor-v1` cache and
// resumes the same recorder session through the public history service.
//
// Run: node --test test/harness/multiplayerProvenance.test.js

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { randomUUID, randomBytes } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { Duplex, PassThrough } = require('node:stream');
const { MultiplayerManager } = require('../../out/multiplayer/manager');
const { NativeWorker } = require('../../out/multiplayer/native');
const { fixture, binaries, until, blobs, diffs } = require('./multiplayerFixture');

// Fault-inject only the byte transport. Managers, TLS, native workers and the
// durable stores are the production implementations.
function relay() {
  const hosts = new Map();
  const streams = new Set();
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
          lease = previous ?? { marker: 'editchain-multiplayer-' + randomBytes(12).toString('hex'), tunnelId: randomUUID(), clusterId: 'use' };
          hosts.set(lease.tunnelId, { incoming, owned, failed });
        },
        lease: () => lease,
        async descriptor() {
          const expiresAt = Date.now() + 60_000;
          const connectToken = ['e30', Buffer.from(JSON.stringify({ exp: Math.ceil(expiresAt / 1000) + 3600 })).toString('base64url'), 'signature'].join('.');
          return { endpoint: { tunnelId: lease.tunnelId, clusterId: lease.clusterId, hostId: 'host', hostPublicKeys: ['YWJj'], clientRelayUri: 'wss://use.rel.tunnels.api.visualstudio.com/test' }, connectToken, expiresAt };
        },
        suspend,
        stop: suspend,
      };
    },
    client() {
      let stream;
      return {
        async connect(invitation) {
          const host = hosts.get(invitation.endpoint.tunnelId);
          if (!host) throw new Error('fixture host offline');
          const left = new PassThrough({ highWaterMark: 1024 });
          const right = new PassThrough({ highWaterMark: 1024 });
          const remote = Duplex.from({ readable: left, writable: right });
          stream = Duplex.from({ readable: right, writable: left });
          for (const item of [remote, stream]) {
            item.on('error', () => {});
            streams.add(item);
            host.owned.add(item);
            item.once('close', () => { streams.delete(item); host.owned.delete(item); });
          }
          host.incoming(remote);
          return stream;
        },
        async stop() { stream?.destroy(); },
      };
    },
    async remove(lease) { hosts.delete(lease.tunnelId); },
  };
  return { provider, drop() { for (const stream of streams) stream.destroy(); } };
}

function environment() {
  const files = fixture();
  const wire = relay();
  const spaces = new Map();
  const managers = new Set();
  const create = local => {
    const manager = new MultiplayerManager({ binary: binaries.peer, chain: local.chain, deviceDirectory: local.device,
      space: spaces.get(local.root), relay: wire.provider, githubToken: async () => { throw new Error('no real service'); },
      journal: { remember: async () => {}, forget: async () => {} }, changed: () => {},
      saveSpace: async space => { spaces.set(local.root, space); } });
    managers.add(manager);
    return manager;
  };
  return {
    files, create,
    async stopSharing() { await Promise.all([...managers].map(manager => manager.stop())); managers.clear(); },
    stop() { wire.drop(); files.stop(); },
  };
}

const live = manager => manager.status().peers.filter(peer => peer.state === 'Live').length;
const ledger = chain => JSON.parse(fs.readFileSync(path.join(chain, 'multiplayer/scope.json'), 'utf8'));
const keyOf = entry => JSON.stringify([entry.id, entry.digest]);

function recorder(local) {
  const identity = { kind: 'unsigned', guid: '11111111-1111-4111-8111-111111111111', stream: 'aaaaaaaaaaaaaaaaaaaaaaaa' };
  const session = randomUUID();
  const document = { id: 'buffer', uri: `file://${path.join(local.root, 'shared.ts')}`, path: 'shared.ts', version: 1 };
  let sequence = 0;
  const send = async events => {
    const response = await local.client.request({ RecordEditorEvents: { workspace_path: local.root, chain_dir: '.editchain',
      events: events.map(event => ({ schema: 1, session, sequence: ++sequence, time_ms: 1000 + sequence, identity, event })) } },
    { timeoutMs: 30_000 });
    if (!response?.Ok) throw new Error(`record failed: ${response?.Error?.code || 'invalid_response'}`);
    return response.Ok;
  };
  const changed = async (before, after, version) => {
    const change = sequence + 1;
    return send([
      { type: 'document_changed', document: { ...document, version }, before_version: version - 1, before, after,
        changes: [{ offset: 0, length: before.length, text: after }], reason: 'undo' },
      { type: 'human_edit', change, signal: 'undo' },
    ]);
  };
  return { document, send, changed };
}

async function open(local) {
  const opened = await local.call({ Open: { workspace_path: local.root, chain_dir: '.editchain' } });
  const window = await local.call({ GetWindow: { snapshot_id: opened.snapshot_id, offset: 0, limit: 200, include_layout: false } });
  return { opened, rows: window.rows };
}

for (const policy of ['legacy', 'cutoff']) test(`a peer-supplied copy of a withheld baseline cannot erase recorder state on a cold rebuild (${policy})`, { timeout: 90_000 }, async () => {
  const env = environment();
  const a = env.files.workspace('a'), b = env.files.workspace('b');
  try {
    const author = recorder(a);
    await author.send([{ type: 'tracking_started', dwell_ms: 2000, vscode_version: '1.137.0', activity_schema: 3 },
      { type: 'workspace_context', workspace_path: a.root, observed_ms: 1000, repositories: [] }]);
    await author.send([{ type: 'document_snapshot', document: author.document, text: 'baseline\n' }]);
    await author.changed('baseline\n', 'baseline revised\n', 2);

    // Only the pre-sharing durable chain travels: segments and content blobs,
    // never this device's keys or its disposable derived cache.
    fs.mkdirSync(b.chain, { recursive: true });
    fs.cpSync(a.chain, b.chain, { recursive: true, filter: source => path.basename(source) !== 'editor-v1' });
    const copiedBlobs = blobs(b.chain);
    assert.ok(copiedBlobs.length > 0, 'the copied baseline must carry content blobs');
    const baselineRecords = (await open(a)).opened.diagnostics.chain.records;

    // Local work recorded after the copy stays private to this device.
    await author.changed('baseline revised\n', 'private draft\n', 3);

    const host = env.create(a), guest = env.create(b);
    // Exercise both the persisted legacy boundary and migration to an explicit cutoff.
    const worker = new NativeWorker(binaries.peer);
    try { await worker.request({ type: 'configure', chain_dir: a.chain, space: 'provenance-space', backfill: false }); }
    finally { worker.stop(); }
    const withheld = ledger(a.chain).excluded.map(keyOf);
    const invitation = await host.hostHistory(await guest.joinRequest(), policy === 'legacy' ? 'keep' : false);
    assert.ok(withheld.length > 0, 'sharing only new history must withhold the authored baseline');
    assert.equal(ledger(a.chain).received.length, 0, 'nothing has been supplied by the peer yet');
    await guest.joinHistory(invitation, true);

    await until(() => {
      const scope = ledger(a.chain);
      return live(host) === 1 && live(guest) === 1 && scope.received.length === baselineRecords
        && scope.received_blobs.length === copiedBlobs.length
        && (policy === 'cutoff' || scope.received.length + scope.excluded.length === withheld.length);
    }, 'the peer never supplied the independently copied baseline');

    // Every record the peer held was delivered back as an exact receipt, while
    // the post-copy local work stayed withheld.
    const scope = ledger(a.chain);
    assert.ok(scope.received.map(keyOf).every(key => withheld.includes(key)), 'every receipt must match withheld baseline evidence');
    assert.equal(scope.received.length, baselineRecords, 'the peer must supply its complete independently copied baseline');
    if (policy === 'legacy') assert.ok(scope.excluded.length > 0, 'the withheld local work must not be published by receiving a copy of it');
    else {
      assert.ok(scope.cutoff.first_segment > 0, 'the append cutoff remains in force after receipts');
      assert.equal(host.status().peers[0].progress.outgoing.total_records, 0, 'independent receipts cannot re-export pre-cutoff work');
    }
    assert.equal(scope.received_blobs.length, copiedBlobs.length, 'only peer-supplied content may become publishable');
    assert.deepEqual(blobs(b.chain), copiedBlobs, 'the exchange must not publish withheld local blobs to the peer');
    await env.stopSharing();

    const baseline = await open(a);
    const quarantined = baseline.opened.diagnostics.chain.quarantined;

    // Lose only the disposable derived cache. Every authoritative record stays.
    fs.rmSync(path.join(a.chain, 'editor-v1'), { recursive: true, force: true });
    const resumed = await author.send([{ type: 'code_read', document: { ...author.document, version: 3 }, editor: 'view',
      ranges: [{ start: [0, 0], end: [0, 7] }], started_ms: 9000, duration_ms: 3000 }]);
    assert.equal(resumed.accepted, 1);

    const after = await open(a);
    assert.equal(after.opened.diagnostics.chain.quarantined, quarantined, 'resumed capture must not quarantine new evidence');
    const edit = after.rows.find(row => row.kind === 'file');
    const read = after.rows.find(row => row.kind === 'read');
    assert.ok(edit, 'the withheld edit must still render after a cold rebuild');
    assert.ok(read, 'a resumed read must survive a cold derived-index rebuild');
    assert.deepEqual(read.parents, [edit.op_id], 'the resumed read must settle on the recorder frontier, not restart it');

    const details = await a.call({ GetNodeDetails: { snapshot_id: after.opened.snapshot_id, op_id: read.op_id } });
    assert.deepEqual(details.parents, [edit.op_id], 'node details must agree on the frontier');
    const record = JSON.parse(details.summary);
    assert.ok(record.before && record.after, 'the resumed read must resolve its observed buffer revision');
    assert.equal(record.before.version, 3, 'the resumed read must resolve the revision the buffer actually held');
    assert.deepEqual(record.after.content, record.before.content);
    const history = await diffs(a);
    assert.ok(history.some(diff => diff.before === 'baseline\n' && diff.after === 'baseline revised\n'), 'authored baseline diff must resolve');
    assert.ok(history.some(diff => diff.before === 'baseline revised\n' && diff.after === 'private draft\n'), 'withheld local diff must resolve');
  } finally { await env.stopSharing(); env.stop(); }
});
