'use strict';

const { test, before } = require('node:test');
const assert = require('node:assert/strict');
const { Duplex, PassThrough } = require('node:stream');
const { SecureStream, SshAlgorithms } = require('@microsoft/dev-tunnels-ssh');
const { CancellationToken, CancellationTokenSource } = require('vscode-jsonrpc');
const { receiveExpected } = require('../../out/devTunnels/probe');
const {
  runSpike, createSpikeServices, encryptedStream, pinHost, safeFailure, SPIKE_PORT,
} = require('../../out/devTunnels/spike');

let key;
let publicKey;
before(async () => {
  key = await SshAlgorithms.publicKey.ecdsaSha2Nistp256.generateKeyPair();
  publicKey = (await key.getPublicKeyBytes()).toString('base64');
});

function wirePair() {
  const a = new PassThrough();
  const b = new PassThrough();
  return [Duplex.from({ readable: a, writable: b }), Duplex.from({ readable: b, writable: a })];
}

function event() {
  const callbacks = new Set();
  return {
    on(callback) { callbacks.add(callback); return { dispose: () => callbacks.delete(callback) }; },
    emit(value) { for (const callback of callbacks) callback(value); },
  };
}

function fixture(options = {}) {
  const calls = [];
  const pending = new Set();
  const hostEvent = event();
  const clientEvent = event();
  const sessions = [];
  const journal = {
    async remember(name) { pending.add(name); },
    async forget(name) { pending.delete(name); },
  };
  let tunnel;
  const management = {
    async createTunnel(value, request, token) {
      calls.push(['create', value, request, token]);
      if (options.createError) throw options.createError;
      tunnel = { ...value, tunnelId: 'test-tunnel', clusterId: 'test', accessTokens: { host: 'fake-host-grant' } };
      return tunnel;
    },
    async getTunnel(_value, request) {
      calls.push(['resolve', request]);
      return {
        ...tunnel, accessTokens: { connect: 'fake-connect-grant' },
        endpoints: [{ connectionMode: 'TunnelRelay', hostId: 'test-host', hostPublicKeys: [publicKey] }],
      };
    },
    async listTunnels() { return tunnel ? [tunnel] : []; },
    async deleteTunnel(value, _request, token) {
      calls.push(['delete', value, token.isCancellationRequested]);
      if (options.cleanupError) throw options.cleanupError;
      return !options.createError;
    },
    async dispose() { calls.push(['management-dispose']); },
  };
  const host = {
    hostPublicKeys: [publicKey],
    forwardedPortConnecting: hostEvent.on,
    async connect(_tunnel, settings, token) {
      calls.push(['host-connect', settings]);
      if (options.stall) return new Promise((_resolve, reject) => token.onCancellationRequested(() => reject(new Error('cancelled'))));
    },
    async dispose() {
      calls.push(['host-dispose']);
      for (const session of sessions) session.dispose();
    },
  };
  const client = {
    forwardedPortConnecting: clientEvent.on,
    async connect(value, settings) { calls.push(['client-connect', value, settings]); },
    async waitForForwardedPort(port) { calls.push(['wait-port', port]); },
    async connectToForwardedPort(port) {
      if (options.downgrade) {
        const raw = new PassThrough();
        const connecting = { port, stream: raw, transformPromise: Promise.resolve(raw) };
        clientEvent.emit(connecting);
        return connecting.transformPromise;
      }
      const [hostWire, clientWire] = wirePair();
      const server = new SecureStream(hostWire, { publicKeys: [key] });
      const peer = new SecureStream(clientWire, { username: 'tunnel' });
      sessions.push(server, peer);
      server.on('error', () => {});
      peer.on('error', () => {});
      server.onAuthenticating(args => { args.authenticationPromise = Promise.resolve({}); });
      peer.onAuthenticating(args => {
        args.authenticationPromise = args.publicKey.getPublicKeyBytes().then(bytes =>
          bytes.toString('base64') === publicKey ? {} : null);
      });
      const serverReady = server.connect();
      const peerReady = peer.connect();
      const incoming = { port, stream: hostWire, transformPromise: Promise.resolve(server) };
      hostEvent.emit(incoming);
      const outgoing = { port, stream: clientWire, transformPromise: peerReady.then(() => peer) };
      clientEvent.emit(outgoing);
      const [, , stream] = await Promise.all([serverReady, incoming.transformPromise, outgoing.transformPromise]);
      return stream;
    },
    async dispose() { calls.push(['client-dispose']); },
  };
  return { services: { management, host, client }, journal, pending, calls };
}

test('real SDK SecureStreams carry the bounded bidirectional probe and RTT samples', { timeout: 15000 }, async () => {
  const f = fixture();
  const logs = [];
  const result = await runSpike(f.services, f.journal, line => logs.push(line), CancellationToken.None, 10000);
  assert.equal(result.bytesEachDirection, 17024);
  assert.equal(result.roundTrips, 20);
  assert.equal(result.tunnelDeleted, true);
  assert.ok(result.rttMs.max >= result.rttMs.p95);
  assert.equal(f.pending.size, 0);
  assert.equal(f.services.host.forwardConnectionsToLocalPorts, false);
  assert.equal(f.services.client.acceptLocalConnectionsForForwardedPorts, false);
  const creation = f.calls.find(call => call[0] === 'create');
  assert.deepEqual(creation[1].ports, [{ portNumber: SPIKE_PORT, protocol: 'tcp' }]);
  assert.equal(creation[1].accessControl, undefined, 'no anonymous ACL');
  const clientTunnel = f.calls.find(call => call[0] === 'client-connect')[1];
  assert.deepEqual(Object.keys(clientTunnel.accessTokens), ['connect']);
  assert.equal(f.calls.filter(call => call[0] === 'delete').length, 1);
  assert.ok(!logs.join('\n').includes('fake-'));
});

test('SDK management obtains a fresh GitHub authorization header on each request', async () => {
  let serial = 0;
  const services = createSpikeServices(async () => `fake-session-${++serial}`);
  const headers = [];
  services.management.adapter = async config => {
    headers.push(config.headers.get('Authorization'));
    return { status: 200, statusText: 'OK', headers: {}, config, data: { name: 'auth-probe' } };
  };
  try {
    await services.management.getTunnel({ tunnelId: 'auth-probe', clusterId: 'use' });
    await services.management.getTunnel({ tunnelId: 'auth-probe', clusterId: 'use' });
    assert.deepEqual(headers, ['github fake-session-1', 'github fake-session-2']);
  } finally {
    await Promise.all(Object.values(services).map(service => service.dispose()));
  }
});

test('unencrypted downgrade fails and the created tunnel is still deleted', async () => {
  const f = fixture({ downgrade: true });
  await assert.rejects(runSpike(f.services, f.journal, () => {}, CancellationToken.None), /encrypted V2/);
  assert.equal(f.pending.size, 0);
  assert.ok(f.calls.some(call => call[0] === 'delete'));
});

test('raw host channels and unrelated ports are refused', async () => {
  await assert.rejects(encryptedStream({ port: SPIKE_PORT, stream: new PassThrough() }), /encrypted V2/);
  const wire = new PassThrough();
  const secure = new SecureStream(wire, { username: 'tunnel' });
  secure.on('error', () => {});
  await assert.rejects(encryptedStream({ port: SPIKE_PORT + 1, transformPromise: Promise.resolve(secure) }), /spike port/);
  secure.dispose();
});

test('missing, mismatched, and ambiguous endpoint keys cannot reach client.connect', () => {
  const base = { accessTokens: { connect: 'fake-connect' } };
  for (const hostPublicKeys of [undefined, [], ['different-key']]) {
    assert.throws(() => pinHost({ ...base, endpoints: [{ connectionMode: 'TunnelRelay', hostPublicKeys }] }, [publicKey]), /matching/);
  }
  const endpoint = { connectionMode: 'TunnelRelay', hostPublicKeys: [publicKey] };
  assert.throws(() => pinHost({ ...base, endpoints: [endpoint, endpoint] }, [publicKey]), /one relay endpoint/);
  assert.throws(() => pinHost({ endpoints: [endpoint] }, [publicKey]), /connect grant/);
});

test('timeout cancels SDK work but deletion receives a fresh cancellation token', async () => {
  const f = fixture({ stall: true });
  await assert.rejects(runSpike(f.services, f.journal, () => {}, CancellationToken.None, 25), /cancelled|timed out/);
  assert.equal(f.calls.find(call => call[0] === 'delete')[2], false);
  assert.equal(f.pending.size, 0);
  assert.ok(f.calls.some(call => call[0] === 'host-dispose'));
  assert.ok(f.calls.some(call => call[0] === 'client-dispose'));
});

test('cancelling before start creates no cloud resource', async () => {
  const f = fixture();
  const source = new CancellationTokenSource();
  const token = source.token;
  source.cancel();
  await assert.rejects(runSpike(f.services, f.journal, () => {}, token), /cancelled/);
  assert.equal(f.calls.some(call => call[0] === 'create'), false);
  assert.equal(f.pending.size, 0);
  source.dispose();
});

test('cleanup failure preserves the recovery record and suppresses credential-bearing errors', async () => {
  const f = fixture({ downgrade: true, cleanupError: new Error('Authorization: github secret-test-token') });
  await assert.rejects(runSpike(f.services, f.journal, () => {}, CancellationToken.None), error => {
    assert.match(error.message, /Retry cleanup/);
    assert.ok(!error.message.includes('secret-test-token'));
    return true;
  });
  assert.equal(f.pending.size, 1);
});

test('uncertain failed creation remains recoverable even if immediate deletion finds nothing', async () => {
  const f = fixture({ createError: { response: { status: 401 }, config: { headers: { Authorization: 'secret' } } } });
  await assert.rejects(runSpike(f.services, f.journal, () => {}, CancellationToken.None), /HTTP 401/);
  assert.equal(f.pending.size, 1);
});

test('fragmented bytes validate, changed or oversized payloads fail, and cancellation removes readers', async () => {
  const stream = new PassThrough();
  const valid = receiveExpected(stream, Buffer.from('abcdef'), CancellationToken.None);
  stream.write(Buffer.from('abc'));
  stream.write(Buffer.from('def'));
  await valid;
  const invalid = receiveExpected(stream, Buffer.from('abc'), CancellationToken.None);
  stream.write(Buffer.from('abcd'));
  await assert.rejects(invalid, /integrity/);
  const source = new CancellationTokenSource();
  const cancelled = receiveExpected(stream, Buffer.from('pending'), source.token);
  source.cancel();
  await assert.rejects(cancelled, /cancelled/);
  assert.equal(stream.listenerCount('data'), 0);
  stream.destroy();
  source.dispose();
});

test('transport error strings are never copied into diagnostic output', () => {
  const error = { response: { status: 403, data: 'private' }, message: 'github super-secret', stack: 'super-secret' };
  const message = safeFailure('Creating tunnel', error);
  assert.match(message, /HTTP 403/);
  assert.ok(!message.includes('super-secret'));
  assert.ok(!message.includes('private'));
});
