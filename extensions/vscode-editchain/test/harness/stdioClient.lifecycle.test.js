// Node unit tests for the StdioClient process lifecycle.
//
// Loads the COMPILED extension host client (out/stdioClient.js, built by
// `npm run compile`) against REAL child processes (temp `#!/usr/bin/env node`
// scripts — StdioClient spawns with an empty argv), asserting that:
//   - a killed child's late 'exit' event cannot tear down a replacement
//     process (identity/generation guard);
//   - a killed child's stale stdout (partial frame + late full response)
//     cannot resolve or poison the replacement process's framing;
//   - stdin write failures reject the relevant pending requests instead of
//     surfacing as an unhandled extension-host stream error;
//   - pending requests are rejected when their process exits (with the exit
//     code) and when the client is stopped;
//   - responses with unmatched numeric ids are dropped, while only id-less
//     messages reach the unsolicited-message handler.
// Also covers the harness serviceBridge: the startup Open is forwarded with
// timeout 0 (unbounded) while regular requests keep the bounded default.
//
// Run: node --test test/harness/stdioClient.lifecycle.test.js
'use strict';

const { test, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const vm = require('node:vm');
const { EventEmitter } = require('node:events');
const { execFileSync } = require('node:child_process');

const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'editchain-stdio-test-'));

function tmpScript(name, body) {
  const p = path.join(tmpDir, name);
  fs.writeFileSync(p, '#!/usr/bin/env node\n' + body);
  fs.chmodSync(p, 0o755);
  return p;
}

// Responds to every framed request with the echo, then ALSO emits a response
// for an unknown id (a stale response nobody is waiting on) and an id-less
// message (the only kind that may be treated as an unsolicited update).
const RESPONDER = tmpScript('responder.js', `
let buf = Buffer.alloc(0);
function send(id, body) {
  const payload = Buffer.from(JSON.stringify({ id, body }), 'utf8');
  const header = Buffer.alloc(4);
  header.writeUInt32LE(payload.length, 0);
  process.stdout.write(Buffer.concat([header, payload]));
}
process.stdin.on('data', (c) => {
  buf = Buffer.concat([buf, c]);
  while (buf.length >= 4) {
    const len = buf.readUInt32LE(0);
    if (buf.length < 4 + len) break;
    const json = buf.subarray(4, 4 + len).toString('utf8');
    buf = buf.subarray(4 + len);
    let msg;
    try { msg = JSON.parse(json); } catch { continue; }
    send(msg.id, { echo: msg.body, id: msg.id });
    send(msg.id + 1000, { stale: true });
    send(undefined, { update: true });
  }
});
`);

// Exits with code 3 shortly after spawning, stranding any in-flight request.
const EXIT3 = tmpScript('exit3.js', `setTimeout(() => process.exit(3), 30);`);

// Reads stdin but never answers, so a request can only settle via stop()/exit.
const MUTE = tmpScript('mute.js', `process.stdin.on('data', () => {});`);

// Poisons the old generation's stdout: leaves a PARTIAL frame (header claims
// 64 bytes, none sent), then — after a short, bounded delay — emits a COMPLETE
// stale response for id 1 (the replacement's first request id after a restart)
// and self-exits. Ignores SIGTERM so the stale frame lands after the
// replacement is running; with a shared buffer / missing generation guard this
// would either splice garbage into the replacement's stream or resolve its
// first request with stale data. The bounded self-exit keeps cleanup
// deterministic; the suite-level sweeper SIGKILLs any straggler.
const POISON = tmpScript('poison.js', `
process.on('SIGTERM', () => {});
const header = Buffer.alloc(4);
header.writeUInt32LE(64, 0);
process.stdout.write(header);
setTimeout(() => {
  const payload = Buffer.from(JSON.stringify({ id: 1, body: { stale: true } }), 'utf8');
  const h = Buffer.alloc(4);
  h.writeUInt32LE(payload.length, 0);
  process.stdout.write(Buffer.concat([h, payload]));
  process.exit(0);
}, 250);
`);

// Closes its stdin read end (fd 0) at boot — EPIPE on the parent's next write
// — while staying alive a little longer, so ONLY the stdin-error path can
// reject the in-flight request (no exit, no timeout, no unhandled error).
// `process.stdin.destroy()` is NOT enough here: it does not release the pipe's
// read end, so the parent's write would silently succeed. The client's
// teardown kills the child on the stdin error; the bounded self-exit and the
// suite-level sweeper are deterministic backstops.
const STDIN_DIE = tmpScript('stdin-die.js', `
const fs = require('fs');
fs.closeSync(0);
setTimeout(() => process.exit(0), 800);
`);

// The client module imports 'vscode' (only used by resolveServicePath); the
// real module only exists inside the extension host, so resolve it to a stub.
const fakeVscode = path.join(tmpDir, 'fake-vscode.js');
fs.writeFileSync(
  fakeVscode,
  'module.exports = { workspace: { getConfiguration: () => ({ get: (_k, d) => d }) } };\n'
);
const origResolveFilename = Module._resolveFilename;
Module._resolveFilename = function (request, ...rest) {
  if (request === 'vscode') return fakeVscode;
  return origResolveFilename.call(this, request, ...rest);
};

const {
  StdioClient,
  resolveDefaultServicePath,
} = require(path.join(__dirname, '..', '..', 'out', 'stdioClient.js'));

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test('default service path prefers an existing release build', () => {
  const root = path.join(tmpDir, 'workspace');
  const release = path.join(root, 'target', 'release', 'editchain-vscode-service');
  const resolved = resolveDefaultServicePath(root, (candidate) => candidate === release);
  assert.equal(resolved, release);
});

test('default service path falls back to debug when release is absent', () => {
  const root = path.join(tmpDir, 'workspace');
  const debug = path.join(root, 'target', 'debug', 'editchain-vscode-service');
  assert.equal(resolveDefaultServicePath(root, () => false), debug);
});

test('killed child\'s late exit event cannot tear down a replacement process', async (t) => {
  const client = new StdioClient();
  t.after(() => client.stop());
  client.start(RESPONDER);
  assert.equal(client.isRunning(), true);
  // Kill and replace in the SAME tick: the killed child's 'exit' event is only
  // delivered on a later event-loop turn, after the replacement is installed.
  client.stop();
  client.start(RESPONDER);
  assert.equal(client.isRunning(), true);
  await sleep(100); // let the old child's exit event fire
  assert.equal(
    client.isRunning(),
    true,
    'stale exit event must not clear the replacement process'
  );
  const resp = await client.request({ ping: true });
  assert.deepEqual(resp.echo, { ping: true });
  assert.equal(resp.id, 1);
});

test('stale stdout from a killed child cannot resolve or poison the replacement process', async (t) => {
  const client = new StdioClient();
  t.after(() => client.stop());
  client.start(POISON);
  await sleep(100); // deliver the old child's partial frame into gen 1 framing
  client.stop();
  client.start(RESPONDER);
  // The replacement's first request must resolve with the REPLACEMENT's
  // response — never the old process's stale { stale: true } body — and the
  // old partial frame must not corrupt the new stream's framing.
  const resp = await client.request({ fresh: true });
  assert.deepEqual(resp.echo, { fresh: true });
  assert.equal(resp.id, 1);
  // Wait past the old process's self-exit (250ms) with margin, then prove the
  // stream is still intact: a later request round-trips cleanly even after the
  // stale frame landed.
  await sleep(500);
  const resp2 = await client.request({ again: true });
  assert.deepEqual(resp2.echo, { again: true });
  assert.equal(client.isRunning(), true);
});

test('stdin write failures reject pending requests instead of surfacing as an unhandled stream error', async (t) => {
  const client = new StdioClient();
  t.after(() => client.stop());
  client.start(STDIN_DIE);
  await sleep(500); // let the child boot and close its stdin read end first
  // If the stdin 'error' listener were missing, this write would raise an
  // unhandled stream 'error' and crash the test process; it must instead
  // reject the pending request and clear the (unusable) process slot.
  const rejection = await client.request({ write: true }).then(
    () => null,
    (e) => e.message
  );
  assert.match(rejection, /stdin error/);
  assert.equal(client.isRunning(), false);
});

test('a synchronous stdin write failure tears down the generation and clears the process slot', async (t) => {
  // Drive the request() sync-write catch deterministically: stub spawn() with
  // a fake child whose stdin.write() throws (a destroyed stdin stream), which
  // the compiled client reaches via property access at call time.
  const cp = require('node:child_process');
  const origSpawn = cp.spawn;
  const fakeStdin = new EventEmitter();
  fakeStdin.write = () => {
    throw new Error('stream destroyed');
  };
  const fakeChild = new EventEmitter();
  fakeChild.stdin = fakeStdin;
  fakeChild.stdout = new EventEmitter();
  fakeChild.stderr = new EventEmitter();
  fakeChild.kill = () => {};
  cp.spawn = () => fakeChild;
  t.after(() => {
    cp.spawn = origSpawn;
  });

  const client = new StdioClient();
  client.start('/fake/binary');
  assert.equal(client.isRunning(), true);
  const rejection = await client.request({ doomed: true }).then(
    () => null,
    (e) => e.message
  );
  assert.match(rejection, /write failed/);
  assert.equal(
    client.isRunning(),
    false,
    'a write failure must clear the current process slot so the next start can respawn'
  );
  const second = await client.request({ again: true }).then(
    () => null,
    (e) => e.message
  );
  assert.match(second, /service is not running/, 'the cleared slot must reject further requests');
});

test('pending requests are rejected when their process exits with a code', async (t) => {
  const client = new StdioClient();
  t.after(() => client.stop());
  client.start(EXIT3);
  const rejection = await client.request({ hang: true }).then(
    () => null,
    (e) => e.message
  );
  assert.match(rejection, /service exited with code 3/);
  assert.equal(client.isRunning(), false);
});

test('pending requests are rejected on stop()', async (t) => {
  const client = new StdioClient();
  t.after(() => client.stop());
  client.start(MUTE);
  const rejectionPromise = client.request({ never: true });
  client.stop();
  const rejection = await rejectionPromise.then(
    () => null,
    (e) => e.message
  );
  assert.match(rejection, /service stopped/);
});

test('responses with unmatched numeric ids are dropped; id-less messages reach onMessage', async (t) => {
  const client = new StdioClient();
  t.after(() => client.stop());
  const updates = [];
  client.setMessageHandler((m) => updates.push(m));
  client.start(RESPONDER);
  const resp = await client.request({ ping: 1 });
  assert.deepEqual(resp.echo, { ping: 1 });
  assert.equal(resp.id, 1);
  await sleep(20); // all three frames arrive in one chunk; settle before asserting
  assert.equal(updates.length, 1, 'exactly one unsolicited (id-less) message expected');
  assert.equal(updates[0].body.update, true);
  assert.equal(updates[0].body.stale === undefined, true, 'unknown-id response must be dropped, not forwarded');
});

test('a request issued when the service is not running rejects immediately', async () => {
  const client = new StdioClient();
  const rejection = await client.request({ nope: true }).then(
    () => null,
    (e) => e.message
  );
  assert.match(rejection, /service is not running/);
});

test('serviceBridge Open and Refresh forward timeout 0; regular requests keep the bounded default', async () => {
  // Load serviceBridge.js in a VM "browser" whose __editchainService records
  // every send(body, timeoutMs) call, then drive the startup handshake and a
  // regular request through window.vscode.
  const calls = [];
  const sandbox = {
    __editchainWorkspace: '/ws',
    __editchainChainDir: '.editchain',
    __editchainService: {
      send(body, timeoutMs) {
        calls.push({ kind: 'send', body, timeoutMs });
        return Promise.resolve({ Ok: { protocol_version: 2, snapshot_id: 'fixture' } });
      },
    },
    MessageEvent: class MessageEvent {
      constructor(type, init) {
        this.type = type;
        this.data = init && init.data;
      }
    },
    dispatchEvent() {},
  };
  sandbox.window = sandbox; // serviceBridge references the bare `window` global
  vm.createContext(sandbox);
  const bridge = fs.readFileSync(
    path.join(__dirname, '..', '..', 'test', 'harness', 'serviceBridge.js'),
    'utf8'
  );
  vm.runInContext(bridge, sandbox);

  sandbox.__editchainStart();
  const open = calls.find((c) => c.body.Open !== undefined);
  assert.ok(open, 'startup handshake must issue an Open request');
  assert.equal(open.body.Open.workspace_path, '/ws');
  assert.equal(open.timeoutMs, 0, 'startup Open must be unbounded (timeout 0)');

  await Promise.resolve();
  sandbox.vscode.postMessage({ type: 'refreshHistory' });
  const refresh = calls.find((c) => c.body.Refresh !== undefined);
  assert.ok(refresh, 'negotiated renderer can refresh the opened snapshot');
  assert.equal(refresh.body.Refresh.workspace_path, '/ws');
  assert.equal(refresh.timeoutMs, 0);

  sandbox.vscode.postMessage({ id: 7, body: { Query: {} } });
  const regular = calls.find((c) => c.body.Query !== undefined);
  assert.ok(regular, 'regular requests must go through the same send path');
  assert.equal(
    regular.timeoutMs,
    undefined,
    'regular requests must NOT pass a timeout (bounded default applies)'
  );
});

// Deterministic suite-level cleanup: every child spawned from this run's tmp
// dir must be gone once the suite completes — a straggler (e.g. a
// SIGTERM-ignoring script whose self-exit timer hasn't fired) would otherwise
// keep the runner alive forever. Per-test t.after stop() handles the normal
// path; this SIGKILL sweep is the backstop. The tmp dir appears only in the
// spawned children's command lines, so the sweep cannot touch the runner.
after(() => {
  try {
    execFileSync('pkill', ['-9', '-f', tmpDir], { stdio: 'ignore' });
  } catch {
    // pkill exits 1 when nothing matched — the expected case.
  }
});
