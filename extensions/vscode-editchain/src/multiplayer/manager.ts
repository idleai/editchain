import { randomUUID } from 'node:crypto';
import { Duplex } from 'node:stream';
import { NativeWorker, PeerBridge, PeerOptions, PeerProgress, PublicDevice, PEER_PROTOCOL } from './native';
import { encodeInvitation, Invitation, JoinRequest, parseInvitation, parseRequest, savedInvitation } from './invitation';
import { ClientTransport, HostLease, HostTransport, managementClient, RelayClient, RelayHost, RelayJournal, removeSavedRelay, validateLease } from './relay';
import { ProbeError } from '../devTunnels/probe';
import { advertisement, Advertisement } from './discovery';

export type SharingStatus = { space?: string; enabled?: boolean; hosting: boolean; peers: { connection?: string; fingerprint?: string; state: string; progress?: PeerProgress }[]; message?: string };
/** Contains bearer grants: store only in private application secret storage. */
export type SavedSharing = { version: 1; space: string; host?: HostLease; peers: Invitation[] };
export type RelayProvider = {
  host(incoming: (stream: Duplex) => void, failed: (message: string, disconnected?: boolean) => void): HostTransport;
  client(): ClientTransport;
  remove(lease: HostLease): Promise<void>;
};
export type ManagerOptions = {
  binary: string; chain: string; deviceDirectory: string; space?: string;
  githubToken(): Promise<string>; journal: RelayJournal;
  saveSpace(space: string): Promise<void>;
  saveSession?(session: SavedSharing | undefined): Promise<void>;
  changed(status: SharingStatus, durableChange: boolean): void;
  relay?: RelayProvider;
};
type Edge = { bridge: PeerBridge; outbound: boolean; clientKey?: string; device?: PublicDevice; progress?: PeerProgress };
type Peer = { invitation: Invitation; attempts: number; state: string; timer?: NodeJS.Timeout };

/** Workspace owner of authenticated history replication and transport recovery. */
export class MultiplayerManager {
  private space?: string;
  private identity?: Promise<PublicDevice>;
  private fingerprint?: string;
  private host?: HostTransport;
  private lease?: HostLease;
  private hostTimer?: NodeJS.Timeout;
  private hostAttempts = 0;
  private pendingCleanup = new Set<HostTransport>();
  private starting?: HostTransport;
  private readonly relay: RelayProvider;
  private clients = new Map<string, ClientTransport>();
  private edges = new Map<string, Edge>();
  private peers = new Map<string, Peer>();
  private generation = 0;
  private enabled = false;
  private message?: string;
  private saved = Promise.resolve();

  constructor(private readonly options: ManagerOptions) {
    this.space = options.space;
    this.relay = options.relay ?? {
      host: (incoming, failed) => new RelayHost(() => managementClient(options.githubToken), options.journal, incoming, failed),
      client: () => new RelayClient(),
      remove: lease => removeSavedRelay(lease, options.journal, options.githubToken),
    };
  }

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
    return this.verifyInvitation(parseInvitation(text));
  }

  async hostHistory(requestText: string, backfill: boolean): Promise<string> {
    const generation = this.generation;
    const guest = (await this.inspectRequest(requestText)).device;
    const identity = await this.device();
    if (guest.fingerprint === identity.fingerprint) throw new ProbeError('Use a different VS Code profile or device for the joining replica.');
    await this.configure(undefined, backfill);
    await this.approve(guest);
    this.requireGeneration(generation);
    this.enabled = true;
    await this.startHost(generation);
    const descriptor = await this.host!.descriptor();
    this.requireGeneration(generation);
    await this.persist();
    this.message = 'Hosting. Give the invitation to the approved device.'; this.publish();
    return encodeInvitation({ version: 1, kind: 'invite', space: this.requiredSpace(), host: identity,
      guest: guest.fingerprint, ...descriptor });
  }

  async joinHistory(text: string, backfill: boolean): Promise<void> {
    const generation = this.generation;
    const invitation = await this.inspectInvitation(text);
    await this.configure(invitation.space, backfill);
    await this.approve(invitation.host);
    this.requireGeneration(generation);
    this.enabled = true;
    const key = invitation.host.fingerprint;
    const previous = this.peers.get(key);
    clearTimeout(previous?.timer);
    this.peers.set(key, { invitation, attempts: 0, state: 'Connecting' });
    await this.persist();
    await this.connect(key, generation);
  }

  /** Reopen only an already bound space and its still-approved devices. */
  async resume(input: SavedSharing): Promise<void> {
    if (this.enabled) return;
    const generation = this.generation;
    if (input?.version !== 1 || typeof input.space !== 'string' || !input.space || !Array.isArray(input.peers) || input.peers.length > 32 ||
      input.space !== await this.recoverSpace()) {
      throw new ProbeError('Saved sharing does not match this workspace.');
    }
    const approved = await this.devices();
    await this.device();
    const peers: Invitation[] = [];
    for (const value of input.peers) {
      try {
        const invitation = await this.verifyInvitation(savedInvitation(value));
        if (invitation.space === this.space && approved.some(device => device.certificate === invitation.host.certificate)) peers.push(invitation);
      } catch { this.message = 'A saved connection needs a fresh invitation.'; }
    }
    this.lease = input.host ? validateLease(input.host) : undefined;
    this.requireGeneration(generation);
    this.enabled = true;
    for (const invitation of peers) this.peers.set(invitation.host.fingerprint, { invitation, attempts: 0, state: 'Reconnecting' });
    if (this.lease) {
      try { await this.startHost(generation); }
      catch { if (generation === this.generation) this.message = 'Hosting is unavailable; retrying automatically.'; }
    }
    this.requireGeneration(generation);
    for (const key of this.peers.keys()) this.schedule(key, 0);
    this.publish();
  }

  /** Restart connections from their durable inventories; also useful after network changes. */
  async reconnect(): Promise<void> {
    if (!this.enabled) throw new ProbeError('Sharing is stopped. Join or host to enable it.');
    const generation = this.generation;
    if (this.lease && !this.host) {
      try { await this.startHost(generation); }
      catch { /* Hosting retries independently of outbound peer connections. */ }
    }
    this.requireGeneration(generation);
    for (const edge of [...this.edges.values()]) edge.bridge.stop();
    for (const key of this.peers.keys()) this.schedule(key, 0);
    this.message = this.lease && !this.host ? 'Hosting is unavailable; retrying automatically. Reconnecting approved devices…' : 'Reconnecting approved devices…'; this.publish();
  }

  async devices(): Promise<PublicDevice[]> {
    if (!this.space) await this.recoverSpace();
    if (!this.space) return [];
    return this.control({ type: 'devices', chain_dir: this.options.chain, space: this.space });
  }

  /** A public endpoint description deliberately excludes the connect grant. */
  async describe(): Promise<Advertisement | undefined> {
    const host = this.host, generation = this.generation;
    if (!this.enabled || !host || !this.lease) return undefined;
    const { endpoint } = await host.descriptor();
    if (generation !== this.generation || host !== this.host) return undefined;
    return advertisement({ version: 1, protocol: PEER_PROTOCOL, encoding: 1, space: this.requiredSpace(),
      device: await this.device(), instance: this.lease.marker, endpoint, expiresAt: Date.now() + 10 * 60_000 });
  }

  /** Discovery refreshes existing grants only; it cannot approve a new device. */
  async discover(candidates: Advertisement[]): Promise<void> {
    if (!this.enabled) return;
    const generation = this.generation, approved = await this.devices();
    for (const input of candidates.slice(0, 32)) {
      try {
        const candidate = advertisement(input);
        if (candidate.space !== this.space) continue;
        const device = await this.verify(candidate.device);
        if (!approved.some(known => known.certificate === device.certificate)) continue;
        const peer = this.peers.get(device.fingerprint);
        if (!peer || candidate.endpoint.tunnelId !== peer.invitation.endpoint.tunnelId || candidate.endpoint.clusterId !== peer.invitation.endpoint.clusterId) continue;
        if (!this.enabled || generation !== this.generation) return;
        peer.invitation = { ...peer.invitation, endpoint: candidate.endpoint };
        this.schedule(device.fingerprint, 0);
      } catch { /* Untrusted or stale directory metadata has no authority. */ }
    }
  }

  async revoke(fingerprint: string): Promise<void> {
    const generation = this.generation;
    await this.control({ type: 'revoke', chain_dir: this.options.chain, space: this.requiredSpace(), fingerprint });
    // A Stop or suspend during the native revocation retires this session: close()
    // has already cleared or preserved its state, so the late Remove must not delete
    // peers, stop transports, or rewrite a possibly newer saved session.
    if (generation !== this.generation) return;
    clearTimeout(this.peers.get(fingerprint)?.timer);
    this.peers.delete(fingerprint);
    for (const edge of [...this.edges.values()]) if (edge.device?.fingerprint === fingerprint || edge.clientKey === fingerprint) edge.bridge.stop();
    const client = this.clients.get(fingerprint);
    this.clients.delete(fingerprint);
    await client?.stop();
    if (generation !== this.generation) return;
    await this.persist();
    this.message = 'Device removed from this replica. Previously shared copies remain with participants.';
    this.publish();
  }

  async stop(): Promise<void> { await this.close(true); }
  async suspend(): Promise<void> { await this.close(false); }

  status(): SharingStatus {
    const edges = [...this.edges.entries()];
    return { space: this.space, enabled: this.enabled, hosting: !!this.host, message: this.message,
      peers: [ ...edges.map(([connection, edge]) => ({ connection, fingerprint: edge.device?.fingerprint ?? edge.clientKey,
        state: !edge.progress?.accepted ? 'Authenticating' : edge.progress.synchronizing ? 'Catching up'
          : edge.progress.unavailable ? 'Waiting for content' : 'Live', progress: edge.progress })),
      ...[...this.peers.entries()].filter(([key]) => !edges.some(([, edge]) => edge.device?.fingerprint === key || edge.clientKey === key))
        .map(([fingerprint, peer]) => ({ fingerprint, state: peer.state })) ] };
  }

  private async startHost(generation: number): Promise<void> {
    if (this.host) return;
    clearTimeout(this.hostTimer); this.hostTimer = undefined;
    const host = this.relay.host(stream => {
      if (!this.enabled || generation !== this.generation) { stream.destroy(); return; }
      this.attach(stream, false);
    }, (message, disconnected) => {
      if (generation !== this.generation) return;
      this.message = message; this.publish();
      if (disconnected && this.host === host) {
        this.host = undefined;
        void host.suspend().catch(() => {}).then(() => this.scheduleHost(generation));
      }
    });
    this.host = host;
    // Own the host from creation: a Stop issued while a first connect is still
    // unwinding must be able to reach and await its resource cleanup.
    this.starting = host;
    this.message = 'Starting private relay…'; this.publish();
    try {
      await host.start(this.lease);
      if (generation !== this.generation) { await (this.lease ? host.suspend() : host.stop()); throw new ProbeError('Sharing was stopped.'); }
      this.lease = host.lease();
      this.hostAttempts = 0;
      if (this.starting === host) this.starting = undefined;
      await this.persist();
    } catch (error) {
      if (this.host === host) this.host = undefined;
      const removing = !this.lease;
      try { await (removing ? host.stop() : host.suspend()); }
      catch {
        // Keep the only reference to a resource whose removal failed so an explicit
        // Stop can retry the deletion instead of leaking the tunnel.
        if (removing) this.pendingCleanup.add(host);
      }
      if (this.starting === host) this.starting = undefined;
      if (generation === this.generation) {
        this.scheduleHost(generation);
        this.message = 'Hosting failed.'; this.publish();
      }
      throw error;
    }
  }

  private scheduleHost(generation: number): void {
    if (!this.enabled || generation !== this.generation || !this.lease || this.host || this.hostTimer) return;
    const delay = Math.min(30_000, 1000 * 2 ** Math.min(this.hostAttempts++, 5));
    this.hostTimer = setTimeout(() => {
      this.hostTimer = undefined;
      if (this.enabled && generation === this.generation) void this.startHost(generation).catch(() => {});
    }, delay);
    this.hostTimer.unref();
  }

  private async connect(key: string, generation: number): Promise<void> {
    const peer = this.peers.get(key);
    if (!peer || !this.enabled || generation !== this.generation || this.clients.has(key) || this.hasEdge(key)) return;
    clearTimeout(peer.timer); peer.timer = undefined;
    const client = this.relay.client();
    this.clients.set(key, client);
    peer.state = 'Connecting'; this.publish();
    try {
      const invitation = savedInvitation(peer.invitation);
      const approved = await this.devices();
      // A Stop or suspend during the approval check retires this session (possibly
      // reusing this manager); a stale attempt must not delete a newer peer entry.
      if (generation !== this.generation || this.peers.get(key) !== peer) return;
      if (!approved.some(device => device.certificate === invitation.host.certificate)) {
        this.peers.delete(key); await this.persist(); return;
      }
      const stream = await client.connect(invitation);
      if (generation !== this.generation || this.peers.get(key) !== peer) { stream.destroy(); return; }
      this.attach(stream, true, invitation.host.certificate, key, client);
    } catch {
      peer.attempts++;
      peer.state = 'Waiting to reconnect';
      try { savedInvitation(peer.invitation); }
      catch { peer.state = 'Invitation expired'; }
    } finally {
      if (!this.hasEdge(key)) {
        if (this.clients.get(key) === client) this.clients.delete(key);
        await client.stop().catch(() => {});
        if (generation === this.generation) this.schedule(key);
      }
      this.publish();
    }
  }

  private schedule(key: string, delay?: number): void {
    const peer = this.peers.get(key);
    if (!this.enabled || !peer || peer.timer || this.hasEdge(key) || this.clients.has(key) || peer.state === 'Invitation expired') return;
    peer.state = 'Waiting to reconnect';
    const generation = this.generation;
    const backoff = Math.min(30_000, 1000 * 2 ** Math.min(peer.attempts, 5));
    peer.timer = setTimeout(() => {
      peer.timer = undefined;
      void this.connect(key, generation).catch(() => {});
    }, delay ?? backoff + Math.floor(Math.random() * 250));
    peer.timer.unref();
    this.publish();
  }

  private hasEdge(key: string): boolean {
    return [...this.edges.values()].some(edge => edge.device?.fingerprint === key || edge.clientKey === key);
  }

  private async close(remove: boolean): Promise<void> {
    this.generation++; this.enabled = false;
    clearTimeout(this.hostTimer); this.hostTimer = undefined;
    for (const peer of this.peers.values()) clearTimeout(peer.timer);
    const host = this.host, lease = this.lease;
    this.host = undefined;
    const clients = [...this.clients.values()]; this.clients.clear();
    // A host created by an in-flight start is owned until its cleanup finishes, even
    // after the disconnect callback cleared this.host and before its lease exists.
    const pending = remove
      ? [...new Set([...this.pendingCleanup, ...(this.starting ? [this.starting] : [])])].filter(value => value !== host)
      : [];
    if (remove) { this.pendingCleanup.clear(); this.starting = undefined; }
    for (const edge of [...this.edges.values()]) edge.bridge.stop();
    this.edges.clear();
    if (remove) { this.peers.clear(); this.lease = undefined; }
    this.message = remove ? 'Sharing stopped.' : 'Sharing paused until this workspace reopens.'; this.publish();
    const results = await Promise.allSettled([
      remove ? this.persist(true) : this.saved,
      host ? (remove ? host.stop() : host.suspend()) : remove && lease ? this.relay.remove(lease) : undefined,
      ...pending.map(value => value.stop()),
      ...clients.map(client => client.stop()),
    ]);
    // Index 0 is the saved session and index 1 the active host; a failed removal
    // stays tracked so the next explicit Stop retries it instead of leaking.
    pending.forEach((value, index) => { if (results[2 + index].status === 'rejected') this.pendingCleanup.add(value); });
    if (remove && host && results[1].status === 'rejected') this.pendingCleanup.add(host);
    if (results.some(result => result.status === 'rejected')) {
      this.message = 'Sharing closed; saved state or tunnel cleanup needs attention.'; this.publish();
      throw new ProbeError('Sharing closed; run EditChain: Clean Up Multiplayer Tunnels if cleanup is pending.');
    }
  }

  private async device(): Promise<PublicDevice> {
    this.identity ??= this.control({ type: 'identity', device_dir: this.options.deviceDirectory });
    try { const device = await this.identity; this.fingerprint = device.fingerprint; return device; }
    catch (error) { this.identity = undefined; throw error; }
  }

  private async verify(device: PublicDevice): Promise<PublicDevice> {
    const checked = await this.control<PublicDevice>({ type: 'verify', certificate: device.certificate });
    if (checked.fingerprint !== device.fingerprint) throw new ProbeError('Device fingerprint does not match its certificate.');
    return checked;
  }

  private async verifyInvitation(invitation: Invitation): Promise<Invitation> {
    invitation.host = await this.verify(invitation.host);
    if (invitation.guest !== (await this.device()).fingerprint) throw new ProbeError('This invitation is for a different device. Create a join request in this VS Code profile.');
    return invitation;
  }

  private async recoverSpace(): Promise<string | undefined> {
    const binding = await this.control<{ space: string | null }>({ type: 'scope', chain_dir: this.options.chain });
    if (binding.space) {
      if (this.space && this.space !== binding.space) throw new ProbeError('This workspace is already bound to a different collaboration space. Use a separate workspace replica.');
      this.space = binding.space;
      await this.options.saveSpace(this.space);
    }
    return binding.space ?? undefined;
  }

  private async configure(space: string | undefined, backfill: boolean): Promise<void> {
    const durable = await this.recoverSpace();
    space ??= durable ?? this.space ?? randomUUID();
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

  private requireGeneration(generation: number): void {
    if (generation !== this.generation) throw new ProbeError('Sharing was stopped.');
  }

  private attach(stream: Duplex, outbound: boolean, remote?: string, clientKey?: string, client?: ClientTransport): void {
    if (this.edges.size >= 8) { stream.destroy(); return; }
    const key = randomUUID();
    const options: PeerOptions = { chain_dir: this.options.chain, device_dir: this.options.deviceDirectory, space: this.requiredSpace(), remote };
    const bridge = new PeerBridge(this.options.binary, stream, options, (progress, device, durableChange) => {
      const edge = this.edges.get(key);
      if (!edge) return;
      edge.device = device ?? undefined; edge.progress = progress;
      if (device) {
        const peer = this.peers.get(device.fingerprint);
        if (peer) {
          if (progress.accepted && progress.rounds > 0) peer.attempts = 0;
          clearTimeout(peer.timer); peer.timer = undefined;
        }
        const duplicate = [...this.edges.entries()].find(([id, other]) => id !== key && other.device?.fingerprint === device.fingerprint);
        if (duplicate) {
          // Both ends choose the connection initiated by the smaller fingerprint.
          // Keep a sole nonpreferred edge until a preferred one actually arrives.
          const preferred = this.fingerprint! < device.fingerprint;
          if (duplicate[1].outbound === outbound || outbound !== preferred) { bridge.stop(); return; }
          duplicate[1].bridge.stop();
        }
      }
      this.message = undefined; this.publish(durableChange);
    }, error => {
      const device = this.edges.get(key)?.device;
      this.edges.delete(key);
      if (clientKey && this.clients.get(clientKey) === client) {
        this.clients.delete(clientKey); void client?.stop().catch(() => {});
      }
      if (error) this.message = error.message;
      const remoteKey = clientKey ?? device?.fingerprint;
      if (remoteKey) {
        const peer = this.peers.get(remoteKey);
        if (error && peer) peer.attempts++;
        this.schedule(remoteKey);
      }
      this.publish();
    });
    this.edges.set(key, { bridge, outbound, clientKey });
    void bridge.start().catch(() => {});
    this.publish();
  }

  private persist(remove = false): Promise<void> {
    // Only an active session owns saved state. A stopped or paused session is
    // cleared (remove) or preserved by close(), never rewritten by late work.
    if (!remove && !this.enabled) return Promise.resolve();
    const snapshot: SavedSharing | undefined = remove || !this.space ? undefined : {
      version: 1, space: this.space, host: this.lease, peers: [...this.peers.values()].map(peer => peer.invitation),
    };
    const work = this.saved.then(() => this.options.saveSession?.(snapshot));
    this.saved = work.catch(() => {});
    return work;
  }

  private publish(durableChange = false): void { this.options.changed(this.status(), durableChange); }

  private async control<T>(body: unknown): Promise<T> {
    const worker = new NativeWorker(this.options.binary);
    try { return await worker.request<T>(body); }
    finally { worker.stop(); }
  }
}
