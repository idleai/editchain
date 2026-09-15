import { randomBytes } from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { Duplex } from 'node:stream';
import { TunnelRelayTunnelClient, TunnelRelayTunnelHost } from '@microsoft/dev-tunnels-connections';
import { Tunnel, TunnelAccessScopes, TunnelConnectionMode, TunnelProtocol } from '@microsoft/dev-tunnels-contracts';
import { ManagementApiVersions, TunnelManagementHttpClient } from '@microsoft/dev-tunnels-management';
import { SecureStream, SshStream } from '@microsoft/dev-tunnels-ssh';
import { ForwardedPortConnectingEventArgs } from '@microsoft/dev-tunnels-ssh-tcp';
import { CancellationToken, CancellationTokenSource } from 'vscode-jsonrpc';
import { ProbeError, ProbeMetrics, probeStreams } from './probe';
export { ProbeError } from './probe';

export const SPIKE_PORT = 43187;
export const GITHUB_SCOPES = ['read:user', 'read:org'] as const;
export const SDK_VERSION = '1.3.56';
export const SPIKE_TIMEOUT_MS = 90_000;
const CLEANUP_TIMEOUT_MS = 15_000;

type Management = Pick<TunnelManagementHttpClient, 'createTunnel' | 'getTunnel' | 'listTunnels' | 'deleteTunnel' | 'dispose'>;
type Host = Pick<TunnelRelayTunnelHost, 'connect' | 'dispose' | 'forwardedPortConnecting' |
  'forwardConnectionsToLocalPorts' | 'enableE2EEncryption' | 'hostPublicKeys' | 'connectionProtocol'>;
type Client = Pick<TunnelRelayTunnelClient, 'connect' | 'dispose' | 'forwardedPortConnecting' |
  'acceptLocalConnectionsForForwardedPorts' | 'enableE2EEncryption' |
  'waitForForwardedPort' | 'connectToForwardedPort' | 'connectionProtocol'>;

export type SpikeServices = { management: Management; host: Host; client: Client };
export type SpikeJournal = {
  remember(name: string): Promise<void>;
  forget(name: string): Promise<void>;
};
export type SpikeResult = ProbeMetrics & {
  sdkVersion: string; setupMs: number; relayProtocol: 'V1' | 'V2'; tunnelDeleted: true;
};

export function createSpikeServices(githubToken: () => Promise<string>): SpikeServices {
  const management = new TunnelManagementHttpClient(
    { name: 'editchain-vscode-spike', version: '0.1.0' },
    ManagementApiVersions.Version20230927preview,
    async () => `github ${await githubToken()}`
  );
  // No SDK trace callbacks: HTTP error objects and diagnostic traces can contain credentials.
  // The client receives a connect grant and pinned keys, without a user-token callback.
  return { management, host: new TunnelRelayTunnelHost(management), client: new TunnelRelayTunnelClient() };
}

/** Retain the SDK's V2 SecureStream transform; refuse unencrypted channels. */
export async function encryptedStream(event: ForwardedPortConnectingEventArgs): Promise<Duplex | null> {
  const transformed = await event.transformPromise;
  if (event.port !== SPIKE_PORT || !(transformed instanceof SecureStream)) {
    transformed?.destroy();
    throw new ProbeError('Expected an encrypted V2 stream on the spike port.');
  }
  return transformed;
}

/** V1 encrypts the entire peer SSH session, rather than each forwarded stream. */
function encryptedV1SessionId(stream: Duplex): Buffer {
  if (stream instanceof SshStream) {
    const session = stream.channel.session;
    const algorithms = session.algorithms;
    if (session.isConnected && session.principal && session.sessionId?.length &&
      algorithms?.cipher && algorithms.decipher && algorithms.messageSigner && algorithms.messageVerifier) {
      return session.sessionId;
    }
  }
  stream.destroy();
  throw new ProbeError('Expected an authenticated, encrypted V1 SSH session.');
}

async function encryptedHostStream(event: ForwardedPortConnectingEventArgs, protocol?: string): Promise<Duplex | null> {
  if (protocol !== 'tunnel-relay-host') return encryptedStream(event);
  if (event.port !== SPIKE_PORT) {
    event.stream.destroy();
    throw new ProbeError('Unexpected forwarded port.');
  }
  encryptedV1SessionId(event.stream);
  return event.stream;
}

export function pinHost(tunnel: Tunnel | null, keys: string[] | undefined): Tunnel {
  if (!tunnel || !keys?.length) throw new ProbeError('The host did not publish an identity key.');
  const endpoints = tunnel.endpoints?.filter(endpoint =>
    endpoint.connectionMode === TunnelConnectionMode.TunnelRelay &&
    endpoint.hostPublicKeys?.length && endpoint.hostPublicKeys.every(key => keys.includes(key)));
  if (endpoints?.length !== 1) throw new ProbeError('Expected one relay endpoint matching the new host key.');
  const token = tunnel.accessTokens?.[TunnelAccessScopes.Connect];
  if (!token) throw new ProbeError('The service did not issue a connect grant.');
  return { ...tunnel, endpoints, accessTokens: { [TunnelAccessScopes.Connect]: token } };
}

function httpStatus(error: unknown): number | undefined {
  const status = (error as { response?: { status?: unknown } } | null)?.response?.status;
  return typeof status === 'number' && status >= 100 && status <= 599 ? status : undefined;
}

/** Classify recognized service failures without copying any remote text into output. */
function serviceFailureHint(error: unknown, status: number | undefined): string {
  const data = (error as { response?: { data?: unknown } } | null)?.response?.data;
  if (!data || typeof data !== 'object') return '';
  const problem = data as { title?: unknown; detail?: unknown };
  const text = [problem.title, problem.detail]
    .filter((value): value is string => typeof value === 'string')
    .map(value => value.slice(0, 2048)).join(' ');
  if (status === 400 && /\bprotocol\b/i.test(text) && /\binvalid\b|\bunsupported\b|\bnot supported\b/i.test(text)) {
    return ' The service rejected the requested port protocol.';
  }
  if (status === 403 && /\btunnel(?:s|ing)?\b/i.test(text) && /\bdisabled\b/i.test(text)) {
    if (/\bcustom\b/i.test(text) && /\bnames?\b/i.test(text)) {
      return ' The service has disabled custom tunnel names.';
    }
    return ' The service reports that tunneling is disabled.';
  }
  return '';
}

/** Only controlled error text/status can reach Output, notifications, or test results. */
export function safeFailure(stage: string, error: unknown): string {
  if (error instanceof ProbeError) return `${stage}: ${error.message}`;
  const status = httpStatus(error);
  const suffix = status === undefined ? '' : ` (HTTP ${status})`;
  return `${stage} failed${suffix}.${serviceFailureHint(error, status)} SDK error details were omitted to protect credentials.`;
}

export async function bounded<T>(
  action: (token: CancellationToken) => Promise<T>, parent: CancellationToken, timeoutMs: number
): Promise<T> {
  const source = new CancellationTokenSource();
  // JSON-RPC 4's source cannot be disposed safely if cancelled before its lazy token is read.
  const token = source.token;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let subscription: { dispose(): void } | undefined;
  const cancelled = new Promise<never>((_resolve, reject) => {
    const cancel = () => {
      source.cancel();
      reject(new ProbeError('Operation cancelled or timed out.'));
    };
    subscription = parent.onCancellationRequested(cancel);
    timer = setTimeout(cancel, timeoutMs);
    if (parent.isCancellationRequested) cancel();
  });
  try {
    if (token.isCancellationRequested) return await cancelled;
    return await Promise.race([action(token), cancelled]);
  } finally {
    clearTimeout(timer);
    subscription?.dispose();
    source.cancel();
    source.dispose();
  }
}

/** Delete only a resource marked by this spike, using a fresh deadline. */
export async function cleanupSpike(
  management: Management, name: string, journal: SpikeJournal, allowMissing = true,
  locator?: Tunnel
): Promise<void> {
  if (!/^editchain-spike-[a-f0-9]{24}$/.test(name)) throw new ProbeError('Invalid spike cleanup record.');
  const deleted = await bounded(async token => {
    // Resolve the generated locator through the recovery label. Retain support for
    // old records that used the marker as a DNS alias instead of a label.
    const candidates = locator ? [locator] : await management.listTunnels(undefined, undefined,
      { labels: ['editchain-spike'], requireAllLabels: true }, token);
    const matches = candidates.filter(tunnel => tunnel.labels?.includes(name) || tunnel.name === name);
    if (matches.length > 1) throw new ProbeError('Ambiguous spike cleanup record.');
    if (!matches.length) return false;
    const tunnel = matches[0];
    if (!tunnel.tunnelId || !tunnel.clusterId) throw new ProbeError('Missing spike cleanup locator.');
    return management.deleteTunnel({ tunnelId: tunnel.tunnelId, clusterId: tunnel.clusterId }, undefined, token);
  }, CancellationToken.None, CLEANUP_TIMEOUT_MS);
  // A cancelled create can have an uncertain server outcome. Retain its journal entry
  // if deletion saw no resource, so a later cleanup can check again.
  if (deleted || allowMissing) await journal.forget(name);
  else throw new ProbeError(`Creation was interrupted; retry cleanup for ${name}.`);
}

export async function runSpike(
  services: SpikeServices, journal: SpikeJournal, log: (line: string) => void,
  cancellation: CancellationToken, timeoutMs = SPIKE_TIMEOUT_MS
): Promise<SpikeResult> {
  const { management, host, client } = services;
  const name = `editchain-spike-${randomBytes(12).toString('hex')}`;
  const streams = new Set<Duplex>();
  const subscriptions: { dispose(): void }[] = [];
  let stage = 'Preparing spike';
  let created = false;
  let creationRejected = false;
  let createdTunnel: Tunnel | undefined;
  let recorded = false;
  let failure: string | undefined;
  let metrics: ProbeMetrics | undefined;
  let setupMs = 0;
  let relayProtocol: 'V1' | 'V2' = 'V2';
  const step = (value: string) => { stage = value; log(value); };

  try {
    await bounded(async token => {
      await journal.remember(name);
      recorded = true;
      step('Creating private tunnel');
      const started = performance.now();
      const tunnel = await management.createTunnel({
        // `name` on the service contract is a custom DNS alias, which is disabled.
        // Let the SDK generate the ID and use a label for interrupted-run recovery.
        labels: ['editchain-spike', name], customExpiration: 3600,
        // The service accepts auto/http/https, despite the SDK also exposing Tcp.
        ports: [{ portNumber: SPIKE_PORT, protocol: TunnelProtocol.Auto }],
      }, { tokenScopes: [TunnelAccessScopes.Host] }, token);
      created = true;
      createdTunnel = tunnel;

      host.forwardConnectionsToLocalPorts = false;
      host.enableE2EEncryption = true;
      client.acceptLocalConnectionsForForwardedPorts = false;
      client.enableE2EEncryption = true;
      let accept!: (stream: Duplex) => void;
      let reject!: (error: unknown) => void;
      let accepted = false;
      let clientEncrypted = false;
      const incoming = new Promise<Duplex>((resolve, fail) => { accept = resolve; reject = fail; });
      // A host can fail before the client reaches the await below.
      void incoming.catch(() => {});
      const track = (stream: Duplex) => {
        if (!streams.has(stream)) {
          stream.on('error', () => {}); // Keep late transport errors handled through teardown.
          streams.add(stream);
        }
        return stream;
      };
      subscriptions.push(host.forwardedPortConnecting(event => {
        track(event.stream);
        const secure = encryptedHostStream(event, host.connectionProtocol);
        event.transformPromise = secure.then(stream => {
          if (!stream || accepted || token.isCancellationRequested) { stream?.destroy(); return null; }
          accepted = true;
          accept(track(stream));
          return stream;
        });
        void event.transformPromise.catch(reject);
      }));
      subscriptions.push(client.forwardedPortConnecting(event => {
        track(event.stream);
        event.transformPromise = encryptedStream(event).then(stream => {
          if (stream) { track(stream); clientEncrypted = true; }
          return stream;
        });
      }));

      step('Connecting tunnel host');
      const options = { enableRetry: false, enableReconnect: false };
      await host.connect(tunnel, options, token);
      step('Resolving endpoint and verifying host key');
      const resolved = await management.getTunnel(tunnel, {
        includePorts: true, tokenScopes: [TunnelAccessScopes.Connect],
      }, token);
      const pinned = pinHost(resolved, host.hostPublicKeys);
      step('Connecting relay client');
      await client.connect(pinned, options, token);
      await client.waitForForwardedPort(SPIKE_PORT, token);
      const outbound = track(await client.connectToForwardedPort(SPIKE_PORT, token));
      const inbound = await incoming;
      if (host.connectionProtocol === 'tunnel-relay-host' && client.connectionProtocol === 'tunnel-relay-client') {
        // Both endpoints run here, so compare their actual SSH exchange IDs as well
        // as checking encryption/MACs. Separate relay-terminated sessions cannot pass.
        if (!encryptedV1SessionId(inbound).equals(encryptedV1SessionId(outbound))) {
          throw new ProbeError('The V1 endpoints did not share the same encrypted SSH session.');
        }
        relayProtocol = 'V1';
      } else if (host.connectionProtocol !== 'tunnel-relay-host-v2-dev' ||
        client.connectionProtocol !== 'tunnel-relay-client-v2-dev' || !clientEncrypted) {
        throw new ProbeError('Client encryption was not verified.');
      }
      log(`Verified ${relayProtocol} end-to-end encryption.`);
      setupMs = Math.round(performance.now() - started);
      step('Verifying bidirectional bytes and 20 round trips');
      metrics = await probeStreams(inbound, outbound, token);
    }, cancellation, timeoutMs);
  } catch (error) {
    failure = safeFailure(stage, error);
    creationRejected = stage === 'Creating private tunnel' && [400, 403].includes(httpStatus(error) ?? 0);
  } finally {
    for (const subscription of subscriptions) subscription.dispose();
    for (const stream of streams) stream.destroy();
    // Teardown gets its own deadline even when the user cancelled the test.
    const closed = await Promise.allSettled([host, client].map(connection =>
      bounded(() => connection.dispose(), CancellationToken.None, CLEANUP_TIMEOUT_MS)));
    if (closed.some(result => result.status === 'rejected')) failure ??= 'Closing relay connections failed.';
    if (recorded) {
      log('Deleting temporary tunnel');
      try {
        await cleanupSpike(management, name, journal, created || creationRejected, createdTunnel);
      } catch (error) {
        failure = `${failure ? failure + ' ' : ''}${safeFailure('Tunnel cleanup', error)} Retry cleanup for ${name}.`;
      }
    }
    try {
      await bounded(() => management.dispose(), CancellationToken.None, CLEANUP_TIMEOUT_MS);
    } catch { failure ??= 'Closing the management client failed.'; }
  }
  if (failure || !metrics) throw new ProbeError(failure ?? 'The probe did not finish.');
  return { ...metrics, sdkVersion: SDK_VERSION, setupMs, relayProtocol, tunnelDeleted: true };
}
