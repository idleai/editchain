import { randomBytes } from 'node:crypto';
import { Duplex } from 'node:stream';
import { ConnectionStatus, TunnelRelayTunnelClient, TunnelRelayTunnelHost } from '@microsoft/dev-tunnels-connections';
import { Tunnel, TunnelAccessScopes, TunnelProtocol, TunnelRelayTunnelEndpoint } from '@microsoft/dev-tunnels-contracts';
import { ManagementApiVersions, TunnelAccessTokenProperties, TunnelManagementHttpClient } from '@microsoft/dev-tunnels-management';
import { CancellationToken, CancellationTokenSource } from 'vscode-jsonrpc';
import { bounded, encryptedHostStream, encryptedStream, encryptedV1SessionId, pinHost, ProbeError, safeFailure } from '../devTunnels/spike';
import { Invitation, invitationTunnel, MULTIPLAYER_PORT, RelayEndpoint, validateEndpoint } from './invitation';

export type RelayJournal = { remember(marker: string): Promise<void>; forget(marker: string): Promise<void> };
export type HostLease = { marker: string; tunnelId: string; clusterId: string };
export type HostTransport = Pick<RelayHost, 'start' | 'descriptor' | 'lease' | 'stop' | 'suspend'>;
export type ClientTransport = Pick<RelayClient, 'connect' | 'stop'>;
const DEADLINE = 60_000;
const CLEANUP = 15_000;
const OPTIONS = { enableRetry: false, enableReconnect: false };

export function managementClient(token: () => Promise<string>): TunnelManagementHttpClient {
  return new TunnelManagementHttpClient({ name: 'editchain-multiplayer', version: '0.1.0' },
    ManagementApiVersions.Version20230927preview, async () => `github ${await token()}`);
}

/** Claim ownership before deleting a saved resource without an active host. */
export async function removeSavedRelay(lease: HostLease, journal: RelayJournal, token: () => Promise<string>): Promise<void> {
  validateLease(lease);
  await journal.remember(lease.marker);
  const management = managementClient(token);
  try {
    // Resolve the actual service label instead of trusting a saved locator.
    await cleanupRelay(management, lease.marker, journal);
  } finally { await management.dispose(); }
}

/** Host resources belong to exactly one explicit sharing session. */
export class RelayHost {
  private readonly host: TunnelRelayTunnelHost;
  private readonly cancellation = new CancellationTokenSource();
  private marker = `editchain-multiplayer-${randomBytes(12).toString('hex')}`;
  private tunnel?: Tunnel;
  private recorded = false;
  private closed = false;
  private subscription?: { dispose(): void };
  private statusSubscription?: { dispose(): void };
  private streams = new Set<Duplex>();
  private stopped?: Promise<void>;
  private creationRejected = false;

  constructor(private readonly management: TunnelManagementHttpClient, private readonly journal: RelayJournal,
    private readonly incoming: (stream: Duplex) => void, private readonly failed: (message: string, disconnected?: boolean) => void) {
    this.host = new TunnelRelayTunnelHost(management);
    this.host.forwardConnectionsToLocalPorts = false;
    this.host.enableE2EEncryption = true;
    void this.cancellation.token;
  }

  async start(previous?: HostLease): Promise<void> {
    if (previous) this.marker = validateLease(previous).marker;
    try {
      await bounded(async token => {
        await this.journal.remember(this.marker);
        this.recorded = true;
        try {
          if (previous) {
            const tunnel = await this.management.getTunnel(previous, { includePorts: true, tokenScopes: [TunnelAccessScopes.Host] }, token);
            if (!tunnel?.labels?.includes(this.marker)) throw new ProbeError('Saved hosting resource is unavailable. Host again to issue a new invitation.');
            this.tunnel = tunnel;
          } else {
            this.tunnel = await this.management.createTunnel({ labels: ['editchain-multiplayer', this.marker], customExpiration: 86400,
              ports: [{ portNumber: MULTIPLAYER_PORT, protocol: TunnelProtocol.Auto }] }, { tokenScopes: [TunnelAccessScopes.Host] }, token);
          }
        } catch (error) {
          this.creationRejected = [400, 403].includes((error as { response?: { status?: number } })?.response?.status ?? 0);
          throw error;
        }
        if (this.closed) throw new ProbeError('Sharing was stopped.');
        this.subscription = this.host.forwardedPortConnecting(event => {
          event.stream.on('error', () => {});
          const secure = encryptedHostStream(event, this.host.connectionProtocol, MULTIPLAYER_PORT);
          event.transformPromise = secure.then(stream => {
            if (!stream) return null;
            stream.on('error', () => {});
            if (this.closed || this.streams.size >= 8) { stream.destroy(); return null; }
            this.streams.add(stream);
            stream.once('close', () => this.streams.delete(stream));
            stream.pause();
            this.incoming(stream);
            return stream;
          });
          void event.transformPromise.catch(() => this.failed('An incoming relay stream failed encryption checks.'));
        });
        this.statusSubscription = this.host.connectionStatusChanged(event => {
          if (this.closed) return;
          if (event.status === ConnectionStatus.Connecting) this.failed('Connecting the hosting relay…');
          if (event.status === ConnectionStatus.Connected) this.failed('Hosting relay connected.');
          if (event.status === ConnectionStatus.Disconnected) this.failed('Hosting relay disconnected; waiting to reconnect.', true);
        });
        await this.host.connect(this.tunnel, { enableRetry: true, enableReconnect: true }, token);
      }, this.cancellation.token, DEADLINE);
    } catch (error) {
      const message = safeFailure('Starting multiplayer relay', error);
      try { await (previous ? this.suspend() : this.stop()); } catch { throw new ProbeError(`${message} Temporary tunnel cleanup is pending.`); }
      throw new ProbeError(message);
    }
  }

  lease(): HostLease {
    return validateLease({ marker: this.marker, tunnelId: this.tunnel?.tunnelId, clusterId: this.tunnel?.clusterId });
  }

  async descriptor(): Promise<{ endpoint: RelayEndpoint; connectToken: string; expiresAt: number }> {
    if (this.closed || !this.tunnel) throw new ProbeError('Multiplayer host is not running.');
    try {
      const resolved = await bounded(token => this.management.getTunnel(this.tunnel!, {
        includePorts: true, tokenScopes: [TunnelAccessScopes.Connect] }, token), this.cancellation.token, DEADLINE);
      const tunnel = pinHost(resolved, this.host.hostPublicKeys);
      const relay = tunnel.endpoints![0] as TunnelRelayTunnelEndpoint;
      const endpoint = validateEndpoint({ tunnelId: tunnel.tunnelId, clusterId: tunnel.clusterId,
        hostId: relay.hostId, clientRelayUri: relay.clientRelayUri, hostPublicKeys: relay.hostPublicKeys });
      const connectToken = tunnel.accessTokens![TunnelAccessScopes.Connect];
      const expiration = TunnelAccessTokenProperties.tryParse(connectToken)?.expiration?.getTime();
      if (!expiration || expiration <= Date.now() + 60_000) throw new ProbeError('The service returned an unusable connect grant.');
      return { endpoint, connectToken, expiresAt: Math.min(expiration, Date.now() + 60 * 60 * 1000) };
    } catch (error) { throw new ProbeError(safeFailure('Creating multiplayer invitation', error)); }
  }

  stop(): Promise<void> {
    this.stopped ??= this.dispose(true);
    return this.stopped;
  }

  suspend(): Promise<void> {
    this.stopped ??= this.dispose(false);
    return this.stopped;
  }

  private async dispose(remove: boolean): Promise<void> {
    this.closed = true;
    this.cancellation.cancel();
    this.subscription?.dispose();
    this.statusSubscription?.dispose();
    for (const stream of this.streams) stream.destroy();
    this.streams.clear();
    const errors: string[] = [];
    try { await bounded(() => this.host.dispose(), CancellationToken.None, CLEANUP); }
    catch { errors.push('Closing the relay host failed.'); }
    try {
      if (remove && this.recorded) await cleanupRelay(this.management, this.marker, this.journal, this.tunnel, this.creationRejected);
    } catch { errors.push('Tunnel cleanup is pending; run EditChain: Clean Up Multiplayer Tunnels.'); }
    try { await bounded(() => this.management.dispose(), CancellationToken.None, CLEANUP); }
    catch { errors.push('Closing relay management failed.'); }
    this.cancellation.dispose();
    if (errors.length) throw new ProbeError(errors.join(' '));
  }
}

export function validateLease(input: unknown): HostLease {
  const value = input as Partial<HostLease> | null;
  if (!value || typeof value.marker !== 'string' || !/^editchain-multiplayer-[a-f0-9]{24}$/.test(value.marker) ||
    ![value.tunnelId, value.clusterId].every(id => typeof id === 'string' && /^[a-zA-Z0-9-]{1,128}$/.test(id))) {
    throw new ProbeError('Invalid saved hosting resource.');
  }
  return { marker: value.marker, tunnelId: value.tunnelId!, clusterId: value.clusterId! };
}

export async function cleanupRelay(management: TunnelManagementHttpClient, marker: string, journal: RelayJournal, locator?: Tunnel, allowMissing = true): Promise<void> {
  if (!/^editchain-multiplayer-[a-f0-9]{24}$/.test(marker)) throw new ProbeError('Invalid multiplayer cleanup record.');
  await bounded(async token => {
    const candidates = locator ? [locator] : await management.listTunnels(undefined, undefined, { labels: [marker], requireAllLabels: true }, token);
    const matches = candidates.filter(tunnel => tunnel.labels?.includes(marker));
    if (matches.length > 1) throw new ProbeError('Ambiguous multiplayer cleanup record.');
    const tunnel = matches[0];
    if (tunnel) {
      if (!tunnel.tunnelId || !tunnel.clusterId) throw new ProbeError('Missing multiplayer cleanup locator.');
      await management.deleteTunnel({ tunnelId: tunnel.tunnelId, clusterId: tunnel.clusterId }, undefined, token);
    } else if (locator === undefined && !allowMissing) {
      // An interrupted create may finish after cancellation. A later explicit
      // cleanup rechecks absence; its caller controls when the journal is cleared.
      throw new ProbeError('Creation outcome is uncertain; retry cleanup shortly.');
    }
    await journal.forget(marker);
  }, CancellationToken.None, CLEANUP);
}

export class RelayClient {
  private readonly client = new TunnelRelayTunnelClient();
  private readonly cancellation = new CancellationTokenSource();
  private subscription?: { dispose(): void };
  private stream?: Duplex;
  private stopped?: Promise<void>;
  private closed = false;
  private v2Encrypted = false;
  private readonly management = new TunnelManagementHttpClient({ name: 'editchain-multiplayer', version: '0.1.0' }, ManagementApiVersions.Version20230927preview);

  constructor() { void this.cancellation.token; }

  async connect(invitation: Invitation): Promise<Duplex> {
    this.client.acceptLocalConnectionsForForwardedPorts = false;
    this.client.enableE2EEncryption = true;
    this.subscription = this.client.forwardedPortConnecting(event => {
      event.stream.on('error', () => {});
      event.transformPromise = encryptedStream(event, MULTIPLAYER_PORT).then(stream => {
        this.v2Encrypted = !!stream;
        return stream;
      });
      void event.transformPromise.catch(() => {});
    });
    try {
      return await bounded(async token => {
        // The connect-only grant can read this same tunnel's public endpoints.
        // This supports a fresh host process/SSH key while Rust still pins the
        // originally approved device certificate before any history exchange.
        const resolved = await this.management.getTunnel(invitationTunnel(invitation), { includePorts: true }, token);
        if (resolved?.tunnelId !== invitation.endpoint.tunnelId || resolved.clusterId !== invitation.endpoint.clusterId) throw new ProbeError('Approved relay resource is unavailable.');
        const endpoints = resolved.endpoints?.filter(endpoint => endpoint.connectionMode === 'TunnelRelay') ?? [];
        if (endpoints.length !== 1) throw new ProbeError('Approved host endpoint is unavailable or ambiguous.');
        const relay = endpoints[0] as TunnelRelayTunnelEndpoint;
        const endpoint = validateEndpoint({ tunnelId: resolved.tunnelId, clusterId: resolved.clusterId,
          hostId: relay.hostId, clientRelayUri: relay.clientRelayUri, hostPublicKeys: relay.hostPublicKeys });
        await this.client.connect(invitationTunnel({ ...invitation, endpoint }), OPTIONS, token);
        await this.client.waitForForwardedPort(MULTIPLAYER_PORT, token);
        const stream = await this.client.connectToForwardedPort(MULTIPLAYER_PORT, token);
        stream.on('error', () => {});
        if (this.client.connectionProtocol === 'tunnel-relay-client') encryptedV1SessionId(stream);
        else if (this.client.connectionProtocol !== 'tunnel-relay-client-v2-dev' || !this.v2Encrypted) { stream.destroy(); throw new ProbeError('Unsupported relay encryption.'); }
        if (this.closed) { stream.destroy(); throw new ProbeError('Joining was cancelled.'); }
        this.stream = stream;
        stream.pause();
        return stream;
      }, this.cancellation.token, DEADLINE);
    } catch (error) { await this.stop(); throw new ProbeError(safeFailure('Joining multiplayer relay', error)); }
  }

  stop(): Promise<void> {
    this.stopped ??= this.dispose();
    return this.stopped;
  }

  private async dispose(): Promise<void> {
    this.closed = true;
    this.cancellation.cancel();
    this.subscription?.dispose();
    this.stream?.destroy();
    try { await bounded(async () => { await this.client.dispose(); await this.management.dispose(); }, CancellationToken.None, CLEANUP); }
    finally { this.cancellation.dispose(); }
  }
}
