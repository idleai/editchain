import { promises as fs } from 'node:fs';
import * as path from 'node:path';
import { randomUUID } from 'node:crypto';
import type { HumanIdentity } from './humanIdentity';
import { EditorEncoding } from './editorEncoding';
import { MAX_EDITOR_EVENT_BYTES, EDITOR_BATCH_BYTES, EDITOR_QUEUE_BYTES, EDITOR_DISK_BYTES } from './editorLimits';

export type EditorEvent = {
  schema: 1; session: string; sequence: number; time_ms: number;
  identity?: HumanIdentity;
  event: { type: string; [key: string]: unknown };
};
type Batch = { workspace_path: string; chain_dir: string; events: EditorEvent[] };
type Send = (serializedBody: Buffer[]) => Promise<unknown>;
type DeliveryTiming = { events: number; queue_ms: number; request_ms: number; oldest_event_ms: number; journal_ms?: number; read_ms: number; native_work?: unknown };
type QueuedEvent = { session: string; sequence: number; time_ms: number; json: Buffer; bytes: number };
type BatchMetadata = { ack: [string, number][]; oldest: number; journal_ms?: number };

/** Local write-ahead outbox. Never remove a batch before an exact durable ack. */
export class EditorOutbox {
  private queue: QueuedEvent[] = [];
  private bytes = 0;
  private running: Promise<void> | undefined;
  private persisting = Promise.resolve();
  private diskBytes = 0;
  private stopped = false;
  private readonly timer: NodeJS.Timeout;
  private readonly batches = new Map<string, BatchMetadata>();
  private readonly encoding = new EditorEncoding();
  // At most one durable batch waits in memory ahead of the active request.
  private ready: { name: string; raw: Promise<Buffer> } | undefined;
  private wake: NodeJS.Timeout | undefined;
  private error: string | undefined;
  private persistenceFailed = false;
  private retryAt = 0;
  private retryDelay = 1000;

  constructor(private readonly directory: string, private readonly workspace: string,
    private readonly chain: string, private readonly send: Send,
    private readonly status: (message: string) => void,
    private readonly delivered: () => void = () => {},
    private readonly slowDelivery: (timing: DeliveryTiming) => void = () => {}) {
    this.timer = setInterval(() => { void this.flush(false); }, 1000);
    this.timer.unref();
  }

  push(event: EditorEvent): boolean {
    if (this.stopped) return false;
    const json = this.encoding.encode(event);
    const size = json.length;
    if (size > MAX_EDITOR_EVENT_BYTES || this.bytes + size > EDITOR_QUEUE_BYTES
      || this.diskBytes + this.bytes + size > EDITOR_DISK_BYTES) {
      const gap: EditorEvent = { ...event, event: { type: 'tracking_gap', reason: 'Recorder paused at local outbox capacity; subsequent work is unobserved until resumed.' } };
      this.enqueue(gap, Buffer.from(JSON.stringify(gap)));
      this.stopped = true;
      this.status('Tracking paused: pending capture exceeds the local limit. Resume tracking after restoring the service.');
      return false;
    }
    this.enqueue(event, json);
    return true;
  }

  private enqueue(event: EditorEvent, json: Buffer): void {
    const bytes = json.length;
    this.bytes += bytes;
    // Freeze the observation once, without retaining another full object graph
    // or re-serializing both snapshots for sizing and every persistence pass.
    this.queue.push({ session: event.session, sequence: event.sequence, time_ms: event.time_ms, json, bytes });
    // Coalesce one short input frame, without waiting for the recovery interval.
    if (!this.wake) {
      this.wake = setTimeout(() => { this.wake = undefined; void this.flush(false); }, 25);
      this.wake.unref();
    }
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
    if (!force && Date.now() < this.retryAt) {
      if (!this.wake) {
        this.wake = setTimeout(() => { this.wake = undefined; void this.flush(false); }, this.retryAt - Date.now());
        this.wake.unref();
      }
      return false;
    }
    if (force) this.retryAt = 0;
    this.deliver();
    await this.running;
    // Save/selection callbacks can enqueue more evidence while an ack is in flight.
    // Complete that tail too instead of reporting a false capture failure.
    if (!this.error && ((await fs.readdir(this.directory)).some(name => name.endsWith('.json')) || this.queue.length)) return this.flush();
    return !this.error && this.queue.length === 0;
  }

  private deliver(): void {
    if (!this.running && Date.now() >= this.retryAt) {
      this.running = this.drain().catch(error => {
        this.error = String(error);
        const busy = this.error.includes('operation would block');
        this.retryAt = Date.now() + (busy ? 50 : this.retryDelay);
        if (!busy) this.retryDelay = Math.min(30000, this.retryDelay * 2);
        if (!this.wake) {
          this.wake = setTimeout(() => { this.wake = undefined; void this.flush(false); }, Math.max(0, this.retryAt - Date.now()));
          this.wake.unref();
        }
        this.status(`Tracking pending: ${this.error}`);
      }).finally(() => { this.running = undefined; });
    }
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
    // Persist the input already present in this pass. Continuing input must
    // not keep this promise open and starve delivery of earlier durable files.
    let remaining = this.queue.length;
    while (remaining > 0) {
      let size = 0;
      const events: QueuedEvent[] = [];
      for (const event of this.queue) {
        // Keep normal batches small. A large snapshot event gets a batch of
        // its own, so it cannot strand the queue or split its exact revisions.
        if (events.length >= remaining || events.length >= 128
          || (events.length > 0 && size + event.bytes > EDITOR_BATCH_BYTES)) break;
        size += event.bytes; events.push(event);
      }
      const first = events[0];
      const name = `${first.session}-${String(first.sequence).padStart(16, '0')}.json`;
      const prefix = JSON.stringify({ workspace_path: this.workspace, chain_dir: this.chain }).slice(0, -1);
      const raw = Buffer.concat([Buffer.from(`${prefix},"events":[`),
        ...events.flatMap((event, index) => index ? [Buffer.from(','), event.json] : [event.json]), Buffer.from(']}')]);
      await this.publish(path.join(this.directory, name), raw);
      this.batches.set(name, { ack: events.map(event => [event.session, event.sequence]),
        oldest: Math.min(...events.map(event => event.time_ms)), journal_ms: Date.now() - first.time_ms });
      this.queue.splice(0, events.length);
      remaining -= events.length;
      this.bytes -= size;
      this.diskBytes += raw.length;
      if (!this.ready) this.ready = { name, raw: Promise.resolve(raw) };
      // An already durable batch can travel while the next snapshot is written.
      this.deliver();
    }
  }

  private async drain(): Promise<void> {
    this.error = undefined;
    const names = (await fs.readdir(this.directory)).filter(name => name.endsWith('.json')).sort();
    for (const [index, name] of names.entries()) {
      const location = path.join(this.directory, name);
      const reading = Date.now();
      let raw: Buffer;
      try {
        if (this.ready?.name === name) {
          const reading = this.ready.raw; this.ready = undefined; raw = await reading;
        }
        else raw = await fs.readFile(location);
      }
      catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue; throw error; }
      let metadata = this.batches.get(name);
      if (!metadata) {
        // Recovered journals need their acknowledgement metadata decoded once.
        // Fresh batches already have it; snapshots travel in their original JSON.
        const batch = JSON.parse(raw.toString('utf8')) as Batch;
        metadata = { ack: batch.events.map(event => [event.session, event.sequence]),
          oldest: Math.min(...batch.events.map(event => event.time_ms)) };
        this.batches.set(name, metadata);
      }
      const started = Date.now();
      const next = names[index + 1];
      if (next && this.ready?.name !== next) {
        // Keep only the next durable batch ready, overlapping its disk read
        // with the current request. A prefetched error belongs to its own turn.
        const raw = fs.readFile(path.join(this.directory, next));
        void raw.catch(() => {});
        this.ready = { name: next, raw };
      }
      const response = await this.send([Buffer.from('{"RecordEditorEvents":'), raw, Buffer.from('}')]) as { Ok?: { schema: number; ack: [string, number][]; work?: unknown }; Error?: unknown };
      if (response?.Ok?.schema !== 1 || JSON.stringify(response.Ok.ack) !== JSON.stringify(metadata.ack)) {
        throw new Error(`Service did not acknowledge editor capture: ${JSON.stringify(response?.Error ?? response)}`);
      }
      await fs.rm(location, { force: true });
      this.batches.delete(name);
      this.diskBytes = Math.max(0, this.diskBytes - raw.length);
      const finished = Date.now();
      const oldest = metadata.oldest;
      if (finished - started >= 250 || finished - oldest >= 1000) {
        this.slowDelivery({ events: metadata.ack.length, queue_ms: Math.max(0, started - oldest),
          request_ms: finished - started, oldest_event_ms: Math.max(0, finished - oldest),
          journal_ms: metadata.journal_ms, read_ms: started - reading, native_work: response.Ok.work });
      }
      this.delivered();
    }
    this.retryAt = 0;
    this.retryDelay = 1000;
    if (!this.stopped) this.status('Tracking human work');
  }

  private async publish(destination: string, content: Buffer): Promise<void> {
    const temporary = `${destination}.${randomUUID()}.tmp`;
    try {
      const file = await fs.open(temporary, 'wx', 0o600);
      try {
        // Avoid writeFile's repeated 512 KiB continuations on a busy editor
        // event loop. Submit the full buffer and only loop on short writes.
        let offset = 0;
        while (offset < content.length) {
          const { bytesWritten } = await file.write(content, offset, content.length - offset);
          if (!bytesWritten) throw new Error('Capture journal write made no progress');
          offset += bytesWritten;
        }
        await file.sync();
      } finally { await file.close(); }
      await fs.rename(temporary, destination);
      if (process.platform !== 'win32') {
        const directory = await fs.open(this.directory, 'r');
        try { await directory.sync(); } finally { await directory.close(); }
      }
    } catch (error) {
      await fs.rm(temporary, { force: true });
      throw error;
    }
  }

  async stop(): Promise<void> {
    clearInterval(this.timer);
    if (this.wake) clearTimeout(this.wake);
    await this.flush();
    // A drain already in progress may have snapshotted its queue before stop.
    if (this.queue.length) await this.flush();
    this.stopped = true;
    if (this.wake) clearTimeout(this.wake);
    this.wake = undefined;
  }
}
