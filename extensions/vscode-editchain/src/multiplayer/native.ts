import { spawn, ChildProcessWithoutNullStreams } from 'node:child_process';
import { Duplex } from 'node:stream';
import { FrameDecoder } from '../frameDecoder';

export const CONTROL_LIMIT = 512 * 1024;
export const INPUT_LIMIT = 64 * 1024;
export class NativePeerError extends Error {}
export type PublicDevice = { certificate: string; fingerprint: string };
export type PeerProgress = { accepted: boolean; synchronizing: boolean; rounds: number; records: number; blobs: number; unavailable: number };
export type PeerTurn = { bytes: string; device: PublicDevice | null; progress: PeerProgress };
export type PeerOptions = { chain_dir: string; device_dir: string; space: string; remote?: string };

/** Deliberately separate from the history service client and its general RPC. */
export class NativeWorker {
  private readonly child: ChildProcessWithoutNullStreams;
  private readonly decoder = new FrameDecoder(CONTROL_LIMIT);
  private pending?: { resolve(value: any): void; reject(error: Error): void; timer: NodeJS.Timeout };
  private closed = false;

  constructor(binary: string, private readonly timeoutMs = 30_000) {
    this.child = spawn(binary, [], { stdio: ['pipe', 'pipe', 'pipe'] });
    this.child.stderr.resume(); // The IPC returns fixed error codes; do not forward diagnostics.
    this.child.on('error', () => this.fail(new NativePeerError('Cannot start the native multiplayer worker. Build or reinstall EditChain.')));
    this.child.on('exit', () => this.fail(new NativePeerError('Native multiplayer worker exited.')));
    this.child.stdin.on('error', () => this.fail(new NativePeerError('Native multiplayer input closed.')));
    this.child.stdout.on('error', () => this.fail(new NativePeerError('Native multiplayer output closed.')));
    this.child.stdout.on('data', (chunk: Buffer) => {
      try {
        for (const bytes of this.decoder.push(chunk)) {
          const pending = this.pending;
          if (!pending) throw new NativePeerError('Unexpected native multiplayer response.');
          const value = JSON.parse(bytes.toString('utf8'));
          if (!value || typeof value.ok !== 'boolean') throw new NativePeerError('Malformed native multiplayer response.');
          this.pending = undefined;
          clearTimeout(pending.timer);
          if (value.ok) pending.resolve(value.result);
          else {
            const codes = ['authentication_failed', 'storage_busy', 'invalid_request_or_peer_data', 'connection_closed', 'storage_or_transport_failure'];
            const code = codes.includes(value.error) ? value.error : 'invalid_native_response';
            pending.reject(new NativePeerError(`Native multiplayer: ${code}`));
          }
        }
      } catch { this.fail(new NativePeerError('Invalid native multiplayer framing or response.')); }
    });
  }

  request<T = any>(body: unknown): Promise<T> {
    if (this.closed) return Promise.reject(new NativePeerError('Native multiplayer worker is closed.'));
    if (this.pending) return Promise.reject(new NativePeerError('Native multiplayer requests must be serialized.'));
    const bytes = Buffer.from(JSON.stringify(body));
    if (bytes.length > CONTROL_LIMIT) return Promise.reject(new NativePeerError('Native multiplayer request exceeds the limit.'));
    const frame = Buffer.allocUnsafe(bytes.length + 4);
    frame.writeUInt32LE(bytes.length);
    bytes.copy(frame, 4);
    return new Promise<T>((resolve, reject) => {
      this.pending = { resolve, reject, timer: setTimeout(() => this.fail(new NativePeerError('Native multiplayer request timed out.')), this.timeoutMs) };
      this.child.stdin.write(frame, error => { if (error) this.fail(new NativePeerError('Native multiplayer input failed.')); });
    });
  }

  stop(): void { this.fail(new NativePeerError('Native multiplayer worker stopped.')); }

  private fail(error: Error): void {
    if (this.closed) return;
    this.closed = true;
    if (this.pending) { clearTimeout(this.pending.timer); this.pending.reject(error); this.pending = undefined; }
    this.child.kill();
  }
}

/** Bounded bridge: one native request and one transport write at a time. */
export class PeerBridge {
  private readonly worker: NativeWorker;
  private timer?: NodeJS.Timeout;
  private tail: Promise<void> = Promise.resolve();
  private queued = 0;
  private closed = false;
  private previous = { records: 0, blobs: 0 };
  private lastProgress?: PeerProgress;
  private started = Date.now();
  private lastInput = Date.now();

  constructor(binary: string, private readonly stream: Duplex, private readonly options: PeerOptions,
    private readonly changed: (progress: PeerProgress, device: PublicDevice | null, durableChange: boolean) => void,
    private readonly ended: (error?: Error) => void,
    private readonly intervalMs = 1500) {
    stream.pause();
    this.worker = new NativeWorker(binary);
    stream.on('error', () => this.stop(new NativePeerError('Multiplayer transport disconnected.')));
    stream.on('close', () => this.stop(new NativePeerError('Multiplayer transport closed.')));
  }

  async start(): Promise<void> {
    try {
      let ready = false;
      // Watch the opening handshake and pending writes too. Backpressure must
      // not disable the deadline that releases an unresponsive connection.
      this.timer = setInterval(() => {
        if (this.closed) return;
        if ((!this.lastProgress?.accepted && Date.now() - this.started > 30_000) || Date.now() - this.lastInput > 90_000) {
          this.stop(new NativePeerError('Multiplayer peer stopped responding.')); return;
        }
        if (!ready || this.queued) return;
        void this.turn(Buffer.alloc(0), true).catch(error => this.stop(error));
      }, this.intervalMs);
      const result = await this.worker.request<PeerTurn>({ type: 'open', ...this.options, remote: this.options.remote ?? null });
      await this.result(result);
      if (this.closed) return;
      ready = true;
      void this.read().catch(() => this.stop(new NativePeerError('Multiplayer transport read failed.')));
    } catch (error) { this.stop(controlledError(error)); throw controlledError(error); }
  }

  stop(error?: Error): void {
    if (this.closed) return;
    this.closed = true;
    clearInterval(this.timer);
    this.worker.stop();
    this.stream.destroy();
    this.ended(error);
  }

  private async read(): Promise<void> {
    for await (const chunk of this.stream) {
      if (this.closed) return;
      this.lastInput = Date.now();
      const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      for (let offset = 0; offset < bytes.length; offset += INPUT_LIMIT) {
        await this.turn(bytes.subarray(offset, offset + INPUT_LIMIT), false);
      }
    }
    this.stop(new NativePeerError('Multiplayer peer disconnected.'));
  }

  private turn(bytes: Buffer, tick: boolean): Promise<void> {
    this.queued++;
    const work = this.tail.then(async () => {
      if (this.closed) return;
      await this.result(await this.worker.request<PeerTurn>({ type: 'turn', bytes: bytes.toString('base64'), tick }));
    });
    this.tail = work.catch(error => this.stop(controlledError(error))).finally(() => { this.queued--; });
    return work;
  }

  private async result(result: PeerTurn): Promise<void> {
    if (this.closed) return;
    if (!result || typeof result.bytes !== 'string' || result.bytes.length > 350_000 || !result.progress ||
      !['rounds', 'records', 'blobs', 'unavailable'].every(key => Number.isSafeInteger(result.progress[key as keyof PeerProgress]))) {
      throw new NativePeerError('Invalid native multiplayer progress.');
    }
    const bytes = Buffer.from(result.bytes, 'base64');
    if (bytes.length) await write(this.stream, bytes);
    const changed = result.progress.records !== this.previous.records || result.progress.blobs !== this.previous.blobs;
    this.previous = result.progress;
    this.lastProgress = result.progress;
    this.changed(result.progress, result.device, changed);
  }
}

function write(stream: Duplex, bytes: Buffer): Promise<void> {
  return new Promise((resolve, reject) => {
    const close = () => { cleanup(); reject(new NativePeerError('Multiplayer transport closed during a write.')); };
    const cleanup = () => { stream.off('close', close); stream.off('error', close); };
    stream.once('close', close); stream.once('error', close);
    stream.write(bytes, error => { cleanup(); if (error) reject(new NativePeerError('Multiplayer transport write failed.')); else resolve(); });
  });
}

function controlledError(error: unknown): Error {
  // This module never receives SDK errors; keep byte buffers and native frames out of diagnostics.
  return error instanceof NativePeerError ? error : new NativePeerError('Multiplayer native connection failed.');
}
