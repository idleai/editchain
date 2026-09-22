'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { GitHubDirectory, DirectorySync, advertisement, advertisementName, repositoryName } = require('../../out/multiplayer/discovery');
const { NativeWorker, PEER_PROTOCOL } = require('../../out/multiplayer/native');
const { fixture, binaries, until } = require('./multiplayerFixture');

async function candidate(files) {
  const worker = new NativeWorker(binaries.peer);
  try {
    const device = await worker.request({ type: 'identity', device_dir: files.directory + '/device' });
    return { version: 1, protocol: PEER_PROTOCOL, encoding: 1, space: 'known-space', device, instance: 'editchain-multiplayer-' + 'a'.repeat(24),
      endpoint: { tunnelId: 'known-tunnel', clusterId: 'use', hostId: 'host', hostPublicKeys: ['YWJj'], clientRelayUri: 'wss://use.rel.tunnels.api.visualstudio.com/tunnel' }, expiresAt: Date.now() + 600_000 };
  } finally { worker.stop(); }
}
const response = (body, status = 200) => new Response(status === 204 ? null : JSON.stringify(body), { status });

test('GitHub publication contains only public fields and upserts only after 404', async () => {
  const files = fixture();
  try {
    const ad = await candidate(files), calls = [];
    const api = new GitHubDirectory('owner/repository', async () => 'private-account-token', async (url, request) => {
      assert.ok(url.startsWith('https://api.github.com/repos/owner/repository/actions/variables'));
      assert.equal(request.redirect, 'error');
      calls.push({ method: request.method, body: request.body });
      return response({}, request.method === 'PATCH' ? 404 : request.method === 'POST' ? 201 : 204);
    });
    await api.publish({ ...ad, connectToken: 'private-invitation-token', arbitrary: 'untrusted extra' });
    assert.deepEqual(calls.map(call => call.method), ['PATCH', 'POST']);
    assert.ok(!JSON.stringify(calls).includes('private-account-token'));
    assert.ok(!JSON.stringify(calls).includes('private-invitation-token'));
    assert.ok(!JSON.stringify(calls).includes('untrusted extra'));
    assert.equal(JSON.parse(calls[1].body).name, advertisementName(ad));
    await api.remove(ad);
    assert.equal(calls[2].method, 'DELETE');
    const forbidden = new GitHubDirectory('owner/repository', async () => '', async (_url, request) => {
      assert.equal(request.method, 'PATCH'); return response({ message: 'secret server details' }, 403);
    });
    await assert.rejects(forbidden.publish(ad), error => /HTTP 403/.test(error.message) && !error.message.includes('secret'));
  } finally { files.stop(); }
});

test('directory ignores stale, incompatible, misnamed and other-space advertisements across pages', async () => {
  const files = fixture();
  try {
    const ad = await candidate(files);
    const variable = value => ({ name: advertisementName(value), value: JSON.stringify(value) });
    let pages = 0;
    const api = new GitHubDirectory('owner/repository', async () => '', async () => {
      pages++;
      return response({ total_count: 35, variables: pages === 1 ? Array.from({ length: 30 }, (_, i) => ({ name: `APP_${i}`, value: 'unrelated' })) : [
        variable({ ...ad, expiresAt: 1 }), variable({ ...ad, protocol: 99 }), variable({ ...ad, space: 'other-space' }),
        { ...variable(ad), name: 'EDITCHAIN_PEER_INCORRECT' }, variable(ad),
      ] });
    });
    assert.deepEqual(await api.read('known-space'), [ad]);
    assert.equal(pages, 2);
    assert.throws(() => repositoryName('owner/repo/../../secret'), /owner\/repository/);
    assert.throws(() => advertisement({ ...ad, protocol: 1 }), /Invalid or stale/);
    assert.throws(() => advertisement({ ...ad, endpoint: { ...ad.endpoint, clientRelayUri: 'https://evil.example/' } }), /Microsoft/);
    const oversized = new GitHubDirectory('owner/repo', async () => '', async () => new Response('x'.repeat(2 * 1024 * 1024 + 1)));
    await assert.rejects(oversized.read(ad.space), /limit/);
    const leaking = new GitHubDirectory('owner/repo', async () => { throw new Error('credential-secret'); });
    await assert.rejects(leaking.read(ad.space), error => !error.message.includes('credential-secret'));
  } finally { files.stop(); }
});

test('directory outage leaves target connections alone; stopping cancels publication and withdraws its own entry', async () => {
  const files = fixture();
  try {
    const ad = await candidate(files), states = [], applied = [];
    let pending = false, removed = false;
    const api = new GitHubDirectory('owner/repo', async () => '', async (_url, request) => {
      if (request.method === 'DELETE') { removed = true; return response({}, 204); }
      pending = true;
      return new Promise((_resolve, reject) => request.signal.addEventListener('abort', () => reject(new Error('cancelled')), { once: true }));
    });
    const sync = new DirectorySync(api, { space: () => ad.space, describe: async () => ad, discover: async items => applied.push(items) }, state => states.push(state));
    const started = sync.start();
    await until(() => pending, 'publication did not begin');
    await sync.stop(); await started;
    assert.equal(removed, true);
    assert.deepEqual(applied, []);
    assert.equal(states.at(-1).state, 'Stopped');
    const unavailable = new DirectorySync(new GitHubDirectory('owner/repo', async () => { throw new Error('offline'); }),
      { space: () => ad.space, describe: async () => undefined, discover: async items => applied.push(items) }, state => states.push(state));
    await unavailable.start();
    assert.equal(states.at(-1).state, 'Unavailable; peer synchronization continues');
    assert.deepEqual(applied, []);
    await unavailable.stop();
  } finally { files.stop(); }
});
