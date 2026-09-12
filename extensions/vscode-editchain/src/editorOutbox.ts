import { promises as fs } from 'node:fs';
import * as path from 'node:path';
import { randomUUID } from 'node:crypto';

export type EditorEvent = {
  schema: 1; session: string; sequence: number; time_ms: number;
  event: { type: string; [key: string]: unknown };
};
type Batch = { workspace_path: string; chain_dir: string; events: EditorEvent[] };
type Send = (body: { RecordEditorEvents: Batch }) => Promise<unknown>;

/** Local write-ahead outbox. Never remove a batch before an exact durable ack. */
export class EditorOutbox {
  private queue: EditorEvent[] = [];
  private bytes = 0;
  private running: Promise<void> | undefined;
  private persisting = Promise.resolve();
  private diskBytes = 0;
  private stopped = false;
  private readonly timer: NodeJS.Timeout;
  private error: string | undefined;
  private persistenceFailed = false;
  private retryAt = 0;
  private retryDelay = 1000;

  constructor(private readonly directory: string, private readonly workspace: string,
    private readonly chain: string, private readonly send: Send,
    private readonly status: (message: string) => void) {
    this.timer = setInterval(() => { void this.flush(false); }, 1000);
    this.timer.unref();
  }

  push(event: EditorEvent): boolean {
    const size = Buffer.byteLength(JSON.stringify(event));
    if (this.stopped) return false;
    if (size > 2 * 1024 * 1024 || this.bytes + size > 32 * 1024 * 1024 || this.diskBytes + this.bytes + size > 64 * 1024 * 1024) {
      const gap: EditorEvent = { ...event, event: { type: 'tracking_gap', reason: 'Recorder paused at local outbox capacity; subsequent work is unobserved until resumed.' } };
      this.queue.push(gap);
      this.bytes += Buffer.byteLength(JSON.stringify(gap));
      this.stopped = true;
      this.status('Tracking paused: pending capture exceeds the local limit. Resume tracking after restoring the service.');
      return false;
    }
    this.bytes += size;
    this.queue.push(event);
    return true;
  }

  async flush(force = true): Promise<boolean> {
    // Persistence continues independently while an earlier network request waits.
    this.persisting = this.persisting.then(() => this.persist()).catch(error => {
      this.persistenceFailed = true;
      this.error = String(error);
      this.status(`Tracking pending: ${this.error}`);
    });
    await this.persisting;
    if (this.persistenceFailed) return false;
    if (!force && Date.now() < this.retryAt) return false;
    if (!this.running) {
      this.running = this.drain().catch(error => {
        this.error = String(error);
        this.retryAt = Date.now() + this.retryDelay;
        this.retryDelay = Math.min(30000, this.retryDelay * 2);
        this.status(`Tracking pending: ${this.error}`);
      }).finally(() => { this.running = undefined; });
    }
    await this.running;
    if (!this.error && (await fs.readdir(this.directory)).some(name => name.endsWith('.json'))) return this.flush();
    return !this.error && this.queue.length === 0;
  }

  private async persist(): Promise<void> {
    this.persistenceFailed = false;
    await fs.mkdir(this.directory, { recursive: true });
    const names = (await fs.readdir(this.directory)).filter(name => name.endsWith('.json'));
    this.diskBytes = 0;
    for (const name of names) {
      try { this.diskBytes += (await fs.stat(path.join(this.directory, name))).size; }
      catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
    }
    // Persist new work even while the service is unavailable.
    while (this.queue.length) {
      let size = 0;
      const events: EditorEvent[] = [];
      for (const event of this.queue) {
        const length = Buffer.byteLength(JSON.stringify(event));
        if (events.length >= 128 || size + length > 4 * 1024 * 1024) break;
        size += length; events.push(event);
      }
      const first = events[0];
      const name = `${first.session}-${String(first.sequence).padStart(16, '0')}.json`;
      const batch: Batch = { workspace_path: this.workspace, chain_dir: this.chain, events };
      await this.publish(path.join(this.directory, name), JSON.stringify(batch));
      this.queue.splice(0, events.length);
      this.bytes -= size;
      this.diskBytes += size;
    }
  }

  private async drain(): Promise<void> {
    this.error = undefined;
    const names = (await fs.readdir(this.directory)).filter(name => name.endsWith('.json')).sort();
    for (const name of names) {
      const location = path.join(this.directory, name);
      let raw: string;
      try { raw = await fs.readFile(location, 'utf8'); }
      catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue; throw error; }
      const batch = JSON.parse(raw) as Batch;
      const response = await this.send({ RecordEditorEvents: batch }) as { Ok?: { schema: number; ack: [string, number][] }; Error?: unknown };
      const expected = batch.events.map(event => [event.session, event.sequence]);
      if (response?.Ok?.schema !== 1 || JSON.stringify(response.Ok.ack) !== JSON.stringify(expected)) {
        throw new Error(`Service did not acknowledge editor capture: ${JSON.stringify(response?.Error ?? response)}`);
      }
      await fs.rm(location, { force: true });
      this.diskBytes = Math.max(0, this.diskBytes - Buffer.byteLength(raw));
    }
    this.retryAt = 0;
    this.retryDelay = 1000;
    if (!this.stopped) this.status('Tracking human work');
  }

  private async publish(destination: string, content: string): Promise<void> {
    const temporary = `${destination}.${randomUUID()}.tmp`;
    const file = await fs.open(temporary, 'wx', 0o600);
    try { await file.writeFile(content); await file.sync(); } finally { await file.close(); }
    await fs.rename(temporary, destination);
    if (process.platform !== 'win32') {
      const directory = await fs.open(this.directory, 'r');
      try { await directory.sync(); } finally { await directory.close(); }
    }
  }

  async stop(): Promise<void> {
    clearInterval(this.timer);
    await this.flush();
    // A drain already in progress may have snapshotted its queue before stop.
    if (this.queue.length) await this.flush();
    this.stopped = true;
  }
}
