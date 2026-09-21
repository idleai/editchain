import { createHash } from 'node:crypto';
import { ProbeError } from '../devTunnels/probe';
import { publicDevice, RelayEndpoint, validateEndpoint } from './invitation';
import type { PublicDevice } from './native';
import { PEER_PROTOCOL } from './native';

export type Advertisement = { version: 1; protocol: typeof PEER_PROTOCOL; encoding: 1; space: string; device: PublicDevice;
  instance: string; endpoint: RelayEndpoint; expiresAt: number };
export type DiscoveryStatus = { repository: string; state: string; candidates: number };
const LIMIT = 32 * 1024;
const RESPONSE_LIMIT = 2 * 1024 * 1024;
const PREFIX = 'EDITCHAIN_PEER_';

export function repositoryName(value: string): string {
  if (!/^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9_.-]{1,100}$/.test(value) || value.endsWith('/.') || value.endsWith('/..')) {
    throw new ProbeError('Use a GitHub repository name in owner/repository form.');
  }
  return value;
}

/** Reconstruct only the public schema; never forward arbitrary fields. */
export function advertisement(input: unknown, now = Date.now()): Advertisement {
  const value = input as Partial<Advertisement> | null;
  if (value?.version !== 1 || value.protocol !== PEER_PROTOCOL || value.encoding !== 1 ||
    typeof value.space !== 'string' || !/^[A-Za-z0-9-]{1,128}$/.test(value.space) ||
    typeof value.instance !== 'string' || !/^editchain-multiplayer-[a-f0-9]{24}$/.test(value.instance) ||
    !Number.isSafeInteger(value.expiresAt) || value.expiresAt! <= now || value.expiresAt! > now + 15 * 60_000) {
    throw new ProbeError('Invalid or stale multiplayer advertisement.');
  }
  return { version: 1, protocol: PEER_PROTOCOL, encoding: 1, space: value.space, instance: value.instance,
    device: publicDevice(value.device), endpoint: validateEndpoint(value.endpoint), expiresAt: value.expiresAt! };
}

export function advertisementName(value: Advertisement): string {
  // One variable per hosting resource. Separate windows cannot delete each
  // other's advertisements; a resumed resource replaces its own stale entry.
  return PREFIX + createHash('sha256').update(`${value.space}:${value.device.fingerprint}:${value.instance}`).digest('hex').slice(0, 40).toUpperCase();
}

/** Optional public directory. It never receives invitation or account-grant payloads. */
export class GitHubDirectory {
  readonly repository: string;
  private requests = new Set<AbortController>();

  constructor(repository: string, private readonly token: () => Promise<string>, private readonly fetcher: typeof fetch = fetch) {
    this.repository = repositoryName(repository);
  }

  async read(space: string): Promise<Advertisement[]> {
    const found: Advertisement[] = [];
    // GitHub's repository-variable endpoint currently pages at most 30 entries.
    for (let page = 1; page <= 20; page++) {
      const { value } = await this.request('GET', `?per_page=30&page=${page}`);
      if (!Array.isArray(value?.variables) || value.variables.length > 30 || !Number.isSafeInteger(value.total_count)) throw new ProbeError('Invalid GitHub directory response.');
      for (const variable of value.variables) {
        if (typeof variable?.name !== 'string' || !variable.name.startsWith(PREFIX) || typeof variable.value !== 'string' || variable.value.length > LIMIT) continue;
        try {
          const candidate = advertisement(JSON.parse(variable.value));
          if (candidate.space === space && variable.name === advertisementName(candidate) && found.length < 32) found.push(candidate);
        } catch { /* Invalid, incompatible or expired metadata is not a candidate. */ }
      }
      if (value.variables.length < 30 || page * 30 >= value.total_count) return found;
    }
    throw new ProbeError('The GitHub directory exceeds the supported page limit.');
  }

  async publish(input: Advertisement): Promise<void> {
    const candidate = advertisement(input);
    const name = advertisementName(candidate), value = JSON.stringify(candidate);
    if (Buffer.byteLength(value) > LIMIT) throw new ProbeError('Multiplayer advertisement exceeds the limit.');
    const result = await this.request('PATCH', '/' + name, { name, value }, true);
    if (result.status === 404) await this.request('POST', '', { name, value });
  }

  async remove(candidate: Advertisement): Promise<void> {
    await this.request('DELETE', '/' + advertisementName(candidate), undefined, true);
  }

  cancel(): void { for (const controller of this.requests) controller.abort(); }

  private async request(method: string, suffix: string, body?: unknown, allowMissing = false): Promise<{ status: number; value?: any }> {
    const controller = new AbortController(); this.requests.add(controller);
    const timer = setTimeout(() => controller.abort(), 10_000);
    try {
      const token = await this.token();
      const response = await this.fetcher(`https://api.github.com/repos/${this.repository}/actions/variables${suffix}`, {
        method, redirect: 'error', signal: controller.signal,
        headers: { Accept: 'application/vnd.github+json', Authorization: `Bearer ${token}`,
          'X-GitHub-Api-Version': '2026-03-10', 'Content-Type': 'application/json', 'User-Agent': 'editchain-multiplayer' },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      if (!response.ok && !(allowMissing && response.status === 404)) {
        await response.body?.cancel();
        throw new ProbeError(`GitHub discovery request failed (HTTP ${response.status}). Existing peer connections are independent of discovery.`);
      }
      if (method !== 'GET' || response.status === 404) { await response.body?.cancel(); return { status: response.status }; }
      const reader = response.body?.getReader();
      if (!reader) throw new ProbeError('Empty GitHub directory response.');
      const chunks: Buffer[] = []; let length = 0;
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          length += value.length;
          if (length > RESPONSE_LIMIT) throw new ProbeError('GitHub directory response exceeds the limit.');
          chunks.push(Buffer.from(value));
        }
      } finally { await reader.cancel().catch(() => {}); }
      return { status: response.status, value: JSON.parse(Buffer.concat(chunks).toString('utf8')) };
    } catch (error) {
      if (error instanceof ProbeError) throw error;
      throw new ProbeError('GitHub discovery is unavailable. Account and response details were omitted.');
    } finally { clearTimeout(timer); this.requests.delete(controller); }
  }
}

export type DirectoryTarget = {
  describe(): Promise<Advertisement | undefined>;
  discover(candidates: Advertisement[]): Promise<void>;
  space(): string | undefined;
};

/** Serialized periodic publication and lookup; failures never stop peer streams. */
export class DirectorySync {
  private timer?: NodeJS.Timeout;
  private active = true;
  private pending?: Promise<void>;
  private published?: Advertisement;

  constructor(private readonly directory: GitHubDirectory, private readonly target: DirectoryTarget,
    private readonly changed: (status: DiscoveryStatus) => void) {}

  async start(): Promise<void> {
    await this.refresh();
    if (this.active) {
      this.timer = setInterval(() => { void this.refresh(); }, 60_000);
      this.timer.unref();
    }
  }

  refresh(): Promise<void> {
    if (!this.active) return Promise.resolve();
    this.pending ??= this.update().finally(() => { this.pending = undefined; });
    return this.pending;
  }

  async stop(): Promise<void> {
    this.active = false; clearInterval(this.timer); this.directory.cancel();
    await this.pending;
    if (this.published) {
      try { await this.directory.remove(this.published); }
      catch { this.changed({ repository: this.directory.repository, state: 'Stopped; advertisement cleanup pending (expires within ten minutes)', candidates: 0 }); return; }
    }
    this.changed({ repository: this.directory.repository, state: 'Stopped', candidates: 0 });
  }

  private async update(): Promise<void> {
    try {
      const space = this.target.space();
      if (!space) return;
      const own = await this.target.describe();
      if (!this.active) return;
      if (own) {
        this.published = own; // Retain a cleanup key even if creation is interrupted.
        await this.directory.publish(own);
      }
      if (!this.active) return;
      const candidates = await this.directory.read(space);
      if (!this.active) return;
      await this.target.discover(candidates);
      this.changed({ repository: this.directory.repository, state: 'Active', candidates: candidates.length });
    } catch {
      if (this.active) this.changed({ repository: this.directory.repository, state: 'Unavailable; peer synchronization continues', candidates: 0 });
    }
  }
}
