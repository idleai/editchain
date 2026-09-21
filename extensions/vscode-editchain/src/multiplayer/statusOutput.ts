import type { DiscoveryStatus } from './discovery';
import type { SharingStatus } from './manager';

type Status = SharingStatus & { discovery?: DiscoveryStatus };
type Peer = SharingStatus['peers'][number];
type Observation = {
  peer: Peer;
  since: number;
  savedAt?: number;
  sentAt?: number;
  printedAt?: number;
  printed?: string;
};

/** Follows public status events only; never polls native storage or the network. */
export class MultiplayerStatusOutput {
  private value?: Status;
  private readonly peers = new Map<string, Observation>();
  private timer?: NodeJS.Timeout;
  private printed?: string;
  private started = false;
  private disposed = false;

  constructor(private readonly appendLine: (line: string) => void) {}

  show(value: Status): void {
    if (this.disposed) return;
    if (!this.started) {
      this.write('Live multiplayer status: progress updates appear automatically, at most once per second; waiting peers are reported every 15s.');
      this.write('Received counts are saved on THIS device, per connection. Content objects hold recorded revision data. Total remaining and percentage are unknown; scans and partial downloads are not reported.');
      this.write('Sent counts are confirmed saved by the remote peer, per connection. Sending alone does not count until the peer acknowledges it.');
      this.started = true;
    }
    this.update(value, true);
  }

  update(value: Status, force = false): void {
    if (this.disposed) return;
    if (this.value?.space !== value.space) this.peers.clear();
    this.value = value;
    const present = new Set<string>();
    value.peers.forEach((peer, index) => {
      const key = peer.connection ?? peer.fingerprint ?? `unidentified-${index}`;
      present.add(key);
      let previous = this.peers.get(key);
      // A joining device is listed by fingerprint while connecting, then gains
      // an edge ID. Retain that observation rather than logging a disconnect.
      if (!previous && peer.fingerprint) {
        const prior = [...this.peers].find(([, value]) => value.peer.fingerprint === peer.fingerprint &&
          (!value.peer.connection || !peer.connection));
        if (prior) { previous = prior[1]; this.peers.delete(prior[0]); }
      }
      const before = previous?.peer.progress, after = peer.progress;
      const reset = !!before !== !!after || (before && after &&
        (after.accepted !== before.accepted || after.records < before.records || after.blobs < before.blobs || after.rounds < before.rounds ||
          (after.sent_records ?? 0) < (before.sent_records ?? 0) || (after.sent_blobs ?? 0) < (before.sent_blobs ?? 0)));
      const observation: Observation = !previous || reset ? { peer, since: Date.now() } : previous;
      if (before && after && !reset && (after.records > before.records || after.blobs > before.blobs)) observation.savedAt = Date.now();
      if (before && after && !reset && ((after.sent_records ?? 0) > (before.sent_records ?? 0) || (after.sent_blobs ?? 0) > (before.sent_blobs ?? 0))) observation.sentAt = Date.now();
      observation.peer = { ...peer, progress: after ? { ...after } : undefined };
      this.peers.set(key, observation);
    });
    for (const [key, observation] of this.peers) {
      if (present.has(key)) continue;
      this.write(`${label(observation.peer)}: connection no longer listed.`);
      this.peers.delete(key);
    }
    if (value.enabled) this.timer ??= setInterval(() => this.flush(), 1000);
    else {
      clearInterval(this.timer); this.timer = undefined;
    }
    if (force || !value.enabled) this.flush(force);
  }

  dispose(): void {
    this.disposed = true;
    clearInterval(this.timer); this.timer = undefined;
    this.peers.clear(); this.value = undefined;
  }

  private flush(force = false): void {
    if (this.disposed || !this.value) return;
    const value = this.value;
    const summary = [value.enabled ? `Sharing enabled; hosting: ${value.hosting ? 'yes' : 'no'}; ${value.peers.length} peer(s).` : 'Sharing stopped.',
      value.space ? `Space: ${value.space}.` : '', value.message,
      value.discovery ? `Discovery: ${value.discovery.state}; ${value.discovery.candidates} candidate(s).` : ''].filter(Boolean).join(' ');
    if (force || this.printed !== summary) { this.write(summary); this.printed = summary; }
    for (const observation of this.peers.values()) {
      const { peer } = observation, progress = peer.progress;
      // Routine empty reconciliation rounds must not fill the log while idle.
      const signature = JSON.stringify([peer.fingerprint, peer.state, progress?.accepted, progress?.records, progress?.blobs, progress?.sent_records, progress?.sent_blobs,
        progress?.unavailable, !!progress?.rounds]);
      const waiting = peer.state !== 'Live' && Date.now() - (observation.printedAt ?? 0) >= 15_000;
      if (!force && signature === observation.printed && !waiting) continue;
      this.write(describe(observation));
      observation.printed = signature; observation.printedAt = Date.now();
    }
  }

  private write(line: string): void { this.appendLine(`[${new Date().toISOString()}] ${line}`); }
}

function label(peer: Peer): string { return peer.fingerprint ? `Peer ${peer.fingerprint.slice(0, 12)}` : 'Unidentified peer'; }

function describe(observation: Observation): string {
  const { peer, since, savedAt, sentAt } = observation, progress = peer.progress;
  if (!progress?.accepted) return `${label(peer)}: ${peer.state}.`;
  const phase = progress.synchronizing ? (progress.rounds ? 'checking shared history' : 'checking shared history (first pass)')
    : progress.unavailable ? 'waiting for content' : 'caught up at last check';
  const seconds = Math.max(0, Math.floor((Date.now() - (savedAt ?? since)) / 1000));
  const activity = savedAt === undefined ? `No new saved-data update observed in ${seconds}s.` : `Last saved-data update observed ${seconds}s ago.`;
  const sent = progress.sent_records === undefined || progress.sent_blobs === undefined ? 'Send progress unavailable.'
    : `Sent (confirmed saved by peer): ${progress.sent_records} records, ${progress.sent_blobs} content objects.`;
  const sendActivity = sentAt === undefined ? '' : ` Last send confirmation observed ${Math.max(0, Math.floor((Date.now() - sentAt) / 1000))}s ago.`;
  return `${label(peer)}: Connected; ${phase}. Received here: ${progress.records} records, ${progress.blobs} content objects (this connection). ${sent} Completed passes: ${progress.rounds}; missing-content responses: ${progress.unavailable}. ${activity}${sendActivity}`;
}
