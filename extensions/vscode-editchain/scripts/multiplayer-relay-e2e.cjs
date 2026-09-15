#!/usr/bin/env node
'use strict';

// Authorized temporary live-service check. GitHub credentials stay in memory;
// output contains counts and timings only. Uses the production manager and workers.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { MultiplayerManager } = require(path.join(process.env.EDITCHAIN_MULTIPLAYER_TEST_EXTENSION || path.resolve(__dirname, '..'), 'out/multiplayer/manager'));
const { fixture, binaries, until, blobs, rows, diffs } = require('../test/harness/multiplayerFixture');

async function run() {
  const files = fixture();
  const a = files.workspace('alice'), b = files.workspace('bob');
  const pending = new Set();
  const managers = [];
  let githubToken;
  const token = async () => {
    githubToken ??= execFileSync('gh', ['auth', 'token', '--hostname', 'github.com'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
    return githubToken;
  };
  const create = local => {
    const manager = new MultiplayerManager({ binary: binaries.peer, chain: local.chain, deviceDirectory: local.device,
      githubToken: token, journal: { remember: async marker => { pending.add(marker); }, forget: async marker => { pending.delete(marker); } },
      saveSpace: async () => {}, changed: () => {} });
    managers.push(manager); return manager;
  };
  const start = Date.now();
  let summary;
  try {
    await a.start(); await b.start();
    const host = create(a), guest = create(b);
    console.log('Creating private relay and exchanging pinned device invitations.');
    const invitation = await host.hostHistory(await guest.joinRequest(), true);
    await guest.joinHistory(invitation, true);
    await until(() => guest.status().peers.some(peer => peer.state === 'Live'), 'relay peers did not authenticate', 90_000);
    const setupMs = Date.now() - start;
    console.log('Capturing and replicating a large historical revision over the live relay.');
    await a.edit('original source\n', 'Shared relay revision\n'.repeat(12_000));
    await until(() => blobs(a.chain).every(name => blobs(b.chain).includes(name)), 'relay content did not hydrate', 90_000);
    const visible = await rows(b);
    assert.ok(visible.some(row => row.file_change?.path === 'shared.ts'));
    assert.ok((await diffs(b)).some(diff => diff.before === 'original source\n' && diff.after === 'Shared relay revision\n'.repeat(12_000)));
    await b.edit('bob original\n', 'bob relay contribution\n');
    await until(() => blobs(b.chain).every(name => blobs(a.chain).includes(name)), 'return relay content did not hydrate', 90_000);
    assert.equal(fs.readFileSync(path.join(b.root, 'shared.ts'), 'utf8'), 'Working tree stays local.\n');
    summary = { setupMs, totalMs: Date.now() - start, contentBlobs: blobs(a.chain).length,
      nativeProcesses: 2, historiesVisible: true, historicalDiffVerified: true, workingTreeUnchanged: true, scope: 'same-account, same-machine, real Microsoft relay' };
  } finally {
    const closed = await Promise.allSettled(managers.map(manager => manager.stop()));
    githubToken = undefined;
    files.stop();
    if (pending.size || closed.some(value => value.status === 'rejected')) {
      // Markers are non-secret recovery labels, never tunnel tokens or private invitations.
      console.error('Pending temporary multiplayer cleanup labels: ' + [...pending].join(', '));
      throw new Error('Temporary relay cleanup incomplete');
    }
  }
  console.log('PASS: ' + JSON.stringify({ ...summary, tunnelDeleted: true }));
}

run().catch(() => { console.error('FAIL: multiplayer relay E2E. Check controlled stage output; credentials and SDK errors are omitted.'); process.exitCode = 1; });
