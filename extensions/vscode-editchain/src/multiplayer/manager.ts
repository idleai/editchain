import { randomUUID } from 'node:crypto';
import { Duplex } from 'node:stream';
import { NativeWorker, PeerBridge, PeerOptions, PeerProgress, PublicDevice } from './native';
import { encodeInvitation, Invitation, JoinRequest, parseInvitation, parseRequest } from './invitation';
import { managementClient, RelayClient, RelayHost, RelayJournal } from './relay';
import { ProbeError } from '../devTunnels/probe';

export type SharingStatus = { space?: string; hosting: boolean; peers: { fingerprint?: string; state: string; progress?: PeerProgress }[]; message?: string };
export type ManagerOptions = {
  binary: string; chain: string; deviceDirectory: string; space?: string;
  githubToken(): Promise<string>; journal: RelayJournal;
  saveSpace(space: string): Promise<void>;
  changed(status: SharingStatus, durableChange: boolean): void;
};
type Edge = { bridge: PeerBridge; outbound: boolean; device?: PublicDevice; progress?: PeerProgress };

/** Workspace owner of sharing, with no VS Code or transport test shortcuts. */
export class MultiplayerManager {
  private space?: string;
  private identity?: Promise<PublicDevice>;
  private host?: RelayHost;
  private clients = new Map<string, RelayClient>();
  private edges = new Map<string, Edge>();
  private generation = 0;
  private message?: string;

  constructor(private readonly options: ManagerOptions) { this.space = options.space; }

  async joinRequest(): Promise<string> {
    const request: JoinRequest = { version: 1, kind: 'request', device: await this.device() };
    return encodeInvitation(request);
  }

  async inspectRequest(text: string): Promise<JoinRequest> {
    const request = parseRequest(text);
    request.device = await this.verify(request.device);
    return request;
  }

  async inspectInvitation(text: string): Promise<Invitation> {
    const invitation = parseInvitation(text);
    invitation.host = await this.verify(invitation.host);
    if (invitation.guest !== (await this.device()).fingerprint) throw new ProbeError('This invitation is for a different device. Create a join request in this VS Code profile.');
    return invitation;
  }

  async hostHistory(requestText: string, backfill: boolean): Promise<string> {
    const generation = this.generation;
    const guest = (await this.inspectRequest(requestText)).device;
    const identity = await this.device();
    if (guest.fingerprint === identity.fingerprint) throw new ProbeError('Use a different VS Code profile or device for the joining replica.');
    await this.configure(this.space ?? randomUUID(), backfill);
    await this.approve(guest);
    if (generation !== this.generation) throw new ProbeError('Sharing was stopped.');
    if (!this.host) {
      const host = new RelayHost(managementClient(this.options.githubToken), this.options.journal,
        stream => this.attach(stream, false), message => { this.message = message; this.publish(); });
      this.host = host;
      this.message = 'Starting private relay…'; this.publish();
      try { await host.start(); }
      catch (error) { if (this.host === host) this.host = undefined; this.message = 'Hosting failed.'; this.publish(); throw error; }
      if (generation !== this.generation) { await host.stop(); throw new ProbeError('Sharing was stopped.'); }
    }
    const descriptor = await this.host.descriptor();
    this.message = 'Hosting. Give the invitation to the approved device.';
    this.publish();
    return encodeInvitation({ version: 1, kind: 'invite', space: this.requiredSpace(), host: identity,
      guest: guest.fingerprint, ...descriptor });
  }

  async joinHistory(text: string, backfill: boolean): Promise<void> {
    const generation = this.generation;
    const invitation = await this.inspectInvitation(text);
    await this.configure(invitation.space, backfill);
    await this.approve(invitation.host);
    if (generation !== this.generation) throw new ProbeError('Joining was stopped.');
    const key = invitation.host.fingerprint;
    if (this.clients.has(key) || [...this.edges.values()].some(edge => edge.device?.fingerprint === key)) {
      throw new ProbeError('Already connected or connecting to this device.');
    }
    const client = new RelayClient();
    this.clients.set(key, client);
    this.message = 'Connecting to the approved host…'; this.publish();
    try {
      const stream = await client.connect(invitation);
      if (generation !== this.generation) { await client.stop(); return; }
      this.attach(stream, true, invitation.host.certificate, key);
    } catch (error) {
      if (this.clients.get(key) === client) this.clients.delete(key);
      this.message = 'Connection failed. The invitation may need to be renewed.'; this.publish();
      throw error;
    }
  }

  async devices(): Promise<PublicDevice[]> {
    if (!this.space) return [];
    return this.control({ type: 'devices', chain_dir: this.options.chain, space: this.space });
  }

  async revoke(fingerprint: string): Promise<void> {
    await this.control({ type: 'revoke', chain_dir: this.options.chain, space: this.requiredSpace(), fingerprint });
    for (const edge of this.edges.values()) if (edge.device?.fingerprint === fingerprint) edge.bridge.stop();
    const client = this.clients.get(fingerprint);
    this.clients.delete(fingerprint);
    await client?.stop();
    this.message = 'Device removed from this replica. Previously shared copies remain with participants.';
    this.publish();
  }

  async stop(): Promise<void> {
    this.generation++;
    const host = this.host;
    this.host = undefined;
    const clients = [...this.clients.values()];
    this.clients.clear();
    for (const edge of this.edges.values()) edge.bridge.stop();
    this.edges.clear();
    this.message = 'Sharing stopped.'; this.publish();
    const results = await Promise.allSettled([host?.stop(), ...clients.map(client => client.stop())]);
    if (results.some(result => result.status === 'rejected')) {
      this.message = 'Sharing stopped; tunnel cleanup is pending.'; this.publish();
      throw new ProbeError('Tunnel cleanup is pending. Run EditChain: Clean Up Multiplayer Tunnels.');
    }
  }

  status(): SharingStatus {
    return { space: this.space, hosting: !!this.host, message: this.message,
      peers: [...this.edges.values()].map(edge => ({ fingerprint: edge.device?.fingerprint,
        state: !edge.progress?.accepted ? 'Authenticating' : edge.progress.synchronizing ? 'Catching up'
          : edge.progress.unavailable ? 'Waiting for content' : 'Live', progress: edge.progress })) };
  }

  private async device(): Promise<PublicDevice> {
    this.identity ??= this.control({ type: 'identity', device_dir: this.options.deviceDirectory });
    try { return await this.identity; } catch (error) { this.identity = undefined; throw error; }
  }

  private async verify(device: PublicDevice): Promise<PublicDevice> {
    const checked = await this.control<PublicDevice>({ type: 'verify', certificate: device.certificate });
    if (checked.fingerprint !== device.fingerprint) throw new ProbeError('Device fingerprint does not match its certificate.');
    return checked;
  }

  private async configure(space: string, backfill: boolean): Promise<void> {
    if (this.space && this.space !== space) throw new ProbeError('This workspace is already bound to a different collaboration space. Use a separate workspace replica.');
    await this.control({ type: 'configure', chain_dir: this.options.chain, space, backfill });
    this.space = space;
    await this.options.saveSpace(space);
  }

  private async approve(device: PublicDevice): Promise<void> {
    await this.control({ type: 'approve', chain_dir: this.options.chain, space: this.requiredSpace(), certificate: device.certificate });
  }

  private requiredSpace(): string {
    if (!this.space) throw new ProbeError('No collaboration space is configured.');
    return this.space;
  }

  private attach(stream: Duplex, outbound: boolean, remote?: string, clientKey?: string): void {
    const key = randomUUID();
    const options: PeerOptions = { chain_dir: this.options.chain, device_dir: this.options.deviceDirectory, space: this.requiredSpace(), remote };
    const bridge = new PeerBridge(this.options.binary, stream, options, (progress, device, durableChange) => {
      const edge = this.edges.get(key);
      if (!edge) return;
      edge.device = device ?? undefined;
      edge.progress = progress;
      if (device) {
        const duplicate = [...this.edges.entries()].find(([id, other]) => id !== key && other.device?.fingerprint === device.fingerprint);
        if (duplicate) {
          // Keep the established edge. A later mesh checkpoint resolves opposite
          // directions consistently when both devices initiate together.
          bridge.stop(); return;
        }
      }
      this.message = undefined;
      this.publish(durableChange);
    }, error => {
      this.edges.delete(key);
      if (clientKey) {
        const client = this.clients.get(clientKey); this.clients.delete(clientKey);
        void client?.stop().catch(() => {});
      }
      if (error) this.message = error.message;
      this.publish();
    });
    this.edges.set(key, { bridge, outbound });
    void bridge.start().catch(() => {});
    this.publish();
  }

  private publish(durableChange = false): void { this.options.changed(this.status(), durableChange); }

  private async control<T>(body: unknown): Promise<T> {
    const worker = new NativeWorker(this.options.binary);
    try { return await worker.request<T>(body); }
    finally { worker.stop(); }
  }
}
