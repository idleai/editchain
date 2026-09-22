'use strict';

const { test, before } = require('node:test');
const assert = require('node:assert/strict');
const { Duplex, PassThrough } = require('node:stream');
const {
  SecureStream, SshAlgorithms, SshClientSession, SshServerSession, SshSessionConfiguration, SshStream, NodeStream,
} = require('@microsoft/dev-tunnels-ssh');
const { TunnelRelayTunnelClient } = require('@microsoft/dev-tunnels-connections');
const { CancellationToken, CancellationTokenSource } = require('vscode-jsonrpc');
const { receiveExpected } = require('../../out/devTunnels/probe');
const {
  runSpike, createSpikeServices, cleanupSpike, encryptedStream, pinHost, safeFailure, SPIKE_PORT,
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

async function sshPair(sessions, configure = () => {}) {
  const [hostWire, clientWire] = wirePair();
  const config = new SshSessionConfiguration();
  configure(config);
  const server = new SshServerSession(config);
  const peer = new SshClientSession(config);
  sessions.push(server, peer);
  server.credentials = { publicKeys: [key] };
  server.onAuthenticating(args => { args.authenticationPromise = Promise.resolve({}); });
  peer.onAuthenticating(args => {
    args.authenticationPromise = args.publicKey.getPublicKeyBytes().then(bytes =>
      bytes.toString('base64') === publicKey ? {} : null);
  });
  await Promise.all([server.connect(new NodeStream(hostWire)), peer.connect(new NodeStream(clientWire))]);
  assert.equal(await peer.authenticate({ username: 'tunnel' }), true);
  const [incoming, outgoing] = await Promise.all([server.acceptChannel(), peer.openChannel()]);
  const streams = [new SshStream(incoming), new SshStream(outgoing)];
  streams.forEach(stream => stream.on('error', () => {}));
  return streams;
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
      if (value.ports.some(port => !['auto', 'http', 'https'].includes(port.protocol))) {
        throw { response: { status: 400, data: { detail: 'The requested protocol is not supported.' } } };
      }
      if (value.name) {
        throw { response: { status: 403, data: { detail: 'The use of custom tunnel names is disabled.' } } };
      }
      if (options.createError) throw options.createError;
      tunnel = { ...value, tunnelId: 'test-tunnel', clusterId: 'test', accessTokens: { host: 'fake-host-grant' } };
      if (options.loseCreateResponse) throw new Error('Connection closed before the create response.');
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
    connectionProtocol: options.v1 ? 'tunnel-relay-host' : 'tunnel-relay-host-v2-dev',
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
    connectionProtocol: options.v1 ? 'tunnel-relay-client' : 'tunnel-relay-client-v2-dev',
    forwardedPortConnecting: clientEvent.on,
    async connect(value, settings) { calls.push(['client-connect', value, settings]); },
    async waitForForwardedPort(port) { calls.push(['wait-port', port]); },
    async connectToForwardedPort(port) {
      if (options.v1) {
        const [incoming, outgoing] = options.downgrade ? [new PassThrough(), new PassThrough()] : await sshPair(sessions, options.configureSsh);
        const connecting = { port, stream: incoming };
        hostEvent.emit(connecting);
        await connecting.transformPromise;
        // The SDK's V1 client does not raise forwardedPortConnecting.
        return options.differentSession ? (await sshPair(sessions))[1] : outgoing;
      }
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
  assert.equal(result.relayProtocol, 'V2');
  assert.ok(result.rttMs.max >= result.rttMs.p95);
  assert.equal(f.pending.size, 0);
  assert.equal(f.services.host.forwardConnectionsToLocalPorts, false);
  assert.equal(f.services.client.acceptLocalConnectionsForForwardedPorts, false);
  const creation = f.calls.find(call => call[0] === 'create');
  assert.deepEqual(creation[1].ports, [{ portNumber: SPIKE_PORT, protocol: 'auto' }]);
  assert.equal(creation[1].name, undefined, 'let the service assign an ID without a custom DNS alias');
  assert.equal(creation[1].labels[0], 'editchain-spike');
  assert.match(creation[1].labels[1], /^editchain-spike-[a-f0-9]{24}$/);
  assert.equal(creation[1].accessControl, undefined, 'no anonymous ACL');
  const clientTunnel = f.calls.find(call => call[0] === 'client-connect')[1];
  assert.deepEqual(Object.keys(clientTunnel.accessTokens), ['connect']);
  assert.equal(f.calls.filter(call => call[0] === 'delete').length, 1);
  assert.ok(!logs.join('\n').includes('fake-'));
});

test('V1 verifies both encrypted peer sessions and carries the full probe without a client transform event', { timeout: 15000 }, async () => {
  const f = fixture({ v1: true });
  const result = await runSpike(f.services, f.journal, () => {}, CancellationToken.None, 10000);
  assert.equal(result.relayProtocol, 'V1');
  assert.equal(result.bytesEachDirection, 17024);
  assert.equal(result.roundTrips, 20);
  assert.equal(result.tunnelDeleted, true);
  assert.equal(f.pending.size, 0);
});

test('V1 refuses raw streams, absent encryption or integrity, and separate encrypted sessions', { timeout: 15000 }, async () => {
  const cases = [
    { downgrade: true },
    { configureSsh: config => { config.encryptionAlgorithms.splice(0, Infinity, null); } },
    { configureSsh: config => {
      config.encryptionAlgorithms.splice(0, Infinity, SshAlgorithms.encryption.aes256Ctr);
      config.hmacAlgorithms.splice(0, Infinity, null);
    } },
    { differentSession: true },
  ];
  for (const options of cases) {
    const f = fixture({ v1: true, ...options });
    await assert.rejects(runSpike(f.services, f.journal, () => {}, CancellationToken.None, 3000), /encrypted V1 SSH|same encrypted SSH session/);
    assert.equal(f.pending.size, 0);
    assert.ok(f.calls.some(call => call[0] === 'delete'));
  }
});

test('the real SDK V1 client rejects a host whose key differs from the pinned endpoint', { timeout: 10000 }, async () => {
  const [hostWire, clientWire] = wirePair();
  const server = new SshServerSession(new SshSessionConfiguration());
  server.credentials = { publicKeys: [key] };
  server.onAuthenticating(args => { args.authenticationPromise = Promise.resolve({}); });
  const peer = new TunnelRelayTunnelClient();
  peer.streamFactory = {
    async createRelayStream() { return { stream: new NodeStream(clientWire), protocol: 'tunnel-relay-client' }; },
  };
  const connected = server.connect(new NodeStream(hostWire));
  try {
    await assert.rejects(peer.connect({
      tunnelId: 'test', clusterId: 'test', accessTokens: { connect: 'fake-connect' },
      endpoints: [{ connectionMode: 'TunnelRelay', hostId: 'host', hostPublicKeys: ['wrong-key'], clientRelayUri: 'wss://relay.invalid' }],
    }, { enableRetry: false, enableReconnect: false }, CancellationToken.None), /authentication failed/);
    await connected;
  } finally {
    await peer.dispose();
    server.dispose();
  }
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

test('definitive create rejections clear absent resources without a misleading cleanup failure', async () => {
  for (const status of [400, 403]) {
    const f = fixture({ createError: { response: { status } } });
    await assert.rejects(runSpike(f.services, f.journal, () => {}, CancellationToken.None), error => {
      assert.match(error.message, new RegExp(`HTTP ${status}`));
      assert.doesNotMatch(error.message, /Creation was interrupted|Retry cleanup/);
      return true;
    });
    assert.equal(f.pending.size, 0);
    assert.equal(f.calls.some(call => call[0] === 'host-connect'), false);
  }
});

test('a rejected create retains recovery when the cleanup check itself fails', async () => {
  const f = fixture({ createError: { response: { status: 400 } } });
  f.services.management.listTunnels = async () => { throw new Error('private-service-detail'); };
  await assert.rejects(runSpike(f.services, f.journal, () => {}, CancellationToken.None), error => {
    assert.match(error.message, /HTTP 400.*Retry cleanup/);
    assert.doesNotMatch(error.message, /private-service-detail/);
    return true;
  });
  assert.equal(f.pending.size, 1);
});

test('a lost create response recovers the generated tunnel through its journal label', async () => {
  const f = fixture({ loseCreateResponse: true });
  await assert.rejects(runSpike(f.services, f.journal, () => {}, CancellationToken.None), error => {
    assert.match(error.message, /Creating private tunnel failed/);
    assert.doesNotMatch(error.message, /Retry cleanup/);
    return true;
  });
  assert.equal(f.calls.filter(call => call[0] === 'delete').length, 1);
  assert.equal(f.pending.size, 0);
});

test('recovery deletes only the marked tunnel and accepts legacy DNS-alias records', async () => {
  const name = `editchain-spike-${'a'.repeat(24)}`;
  const unrelated = { tunnelId: 'unrelated', clusterId: 'test', labels: ['editchain-spike'] };
  for (const identity of [{ labels: ['editchain-spike', name] }, { name, labels: ['editchain-spike'] }]) {
    const deleted = [];
    const forgotten = [];
    await cleanupSpike({
      async listTunnels() { return [unrelated, { tunnelId: 'owned', clusterId: 'test', ...identity }]; },
      async deleteTunnel(locator) { deleted.push(locator); return true; },
    }, name, { async forget(marker) { forgotten.push(marker); } });
    assert.deepEqual(deleted, [{ tunnelId: 'owned', clusterId: 'test' }]);
    assert.deepEqual(forgotten, [name]);
  }
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

test('recognized service failures produce fixed hints without exposing response details', () => {
  const cases = [
    [400, 'Protocol tcp is not supported.', /service rejected the requested port protocol/],
    [400, 'Invalid protocol.', /service rejected the requested port protocol/],
    [403, 'The use of custom tunnel names is disabled.', /service has disabled custom tunnel names/],
    [403, 'Tunnel creation has been disabled.', /service reports that tunneling is disabled/],
  ];
  for (const [status, detail, expected] of cases) {
    const error = {
      response: { status, data: { title: 'secret-title', detail: `${detail} github secret-token https://private/?token=secret-query` } },
      config: { headers: { Authorization: 'secret-header' } },
    };
    const message = safeFailure('Creating tunnel', error);
    assert.match(message, expected);
    assert.doesNotMatch(message, /secret-|https:\/\//);
  }
  const unrelated = safeFailure('Creating tunnel', {
    response: { status: 403, data: { detail: 'Permission denied. secret-token' } },
  });
  assert.doesNotMatch(unrelated, /disabled|protocol|secret-token/);
});
