import { ChildProcessWithoutNullStreams, spawn } from 'child_process';
import * as vscode from 'vscode';

/**
 * A minimal framed stdio client for the native Rust service.
 *
 * Messages are length-prefixed JSON: a 4-byte little-endian length followed
 * by the UTF-8 JSON payload. This mirrors the Rust service's framing.
 */
export class StdioClient {
  private proc: ChildProcessWithoutNullStreams | null = null;
  private running = false;
  // Process generation counter: bumped on every start, so exit/error/stdout
  // events from a dead child can be told apart from the CURRENT process. A
  // stale event from an old child must never tear down (or reject the requests
  // of) a replacement process.
  private generation = 0;
  // Framing state is owned PER GENERATION: a killed child can leave a partial
  // frame (or emit late bytes) on stdout, and that garbage must never be
  // spliced into the replacement process's stream. `framing` is replaced on
  // every start, and chunks are only parsed when their (gen, child) pair is
  // still current.
  private framing: Framing | null = null;
  private nextId = 1;
  private pending = new Map<number, PendingRequest>();
  // Every child this client has spawned, keyed by generation. The client must
  // never orphan a process it owns: a child that is dropped from `proc` (stop,
  // restart, stdin failure, spawn error) is still killed explicitly here, so a
  // killed-but-not-yet-exited child can never outlive the client.
  private children = new Map<number, ChildProcessWithoutNullStreams>();
  private onMessage: ((msg: any) => void) | null = null;
  private log: ((line: string) => void) | null = null;

  /** Register a log sink for service stderr/exit messages. */
  setLog(sink: (line: string) => void): void {
    this.log = sink;
  }

  /** Whether a service process is currently alive and accepting requests. */
  isRunning(): boolean {
    return this.running && this.proc !== null;
  }

  /**
   * Start the Rust service binary.
   *
   * Any requests still awaiting a previous process generation are rejected
   * first, so a restart can never strand a promise issued against the old
   * (dead) process.
   */
  start(binaryPath: string): void {
    if (this.isRunning()) {
      this.log?.('[service] start called while already running — ignoring');
      return;
    }
    // A restart supersedes requests still awaiting the previous process.
    this.rejectPending('[editchain] service restarted');
    // A previous generation's child can still be alive (SIGTERM delivered but
    // not yet exited, or a child that ignores SIGTERM). Never orphan a process
    // we own: re-kill anything still tracked before installing the
    // replacement.
    for (const [oldGen, oldChild] of this.children) {
      this.log?.(`[service] killing leftover process from generation ${oldGen}`);
      oldChild.kill();
    }
    this.children.clear();
    this.log?.(`spawning ${binaryPath}`);
    const gen = ++this.generation;
    const child = spawn(binaryPath, [], { stdio: ['pipe', 'pipe', 'pipe'] });
    this.children.set(gen, child);
    this.proc = child;
    this.running = true;
    // Fresh framing state (and id space) for this process: stale bytes from a
    // killed child must never be parsed into the replacement's buffer, and a
    // late response can never collide with a new request's id.
    this.framing = { gen, buffer: Buffer.alloc(0) };
    this.nextId = 1;
    child.stdout.on('data', (chunk: Buffer) => this.onData(gen, child, chunk));
    child.stderr.on('data', (chunk: Buffer) => {
      this.log?.(`[service] ${chunk.toString()}`);
    });
    // A write to a dead process surfaces as an 'error' on the stdin stream;
    // without a listener it would be an UNHANDLED extension-host stream error.
    // Bound to (gen, child): only this generation's requests are rejected, and
    // a stale child's stdin error can never tear down a replacement.
    child.stdin.on('error', (err) => {
      this.log?.(`[service] stdin error: ${err.message}`);
      this.teardown(gen, child, `[editchain] service stdin error: ${err.message}`);
    });
    child.on('error', (err) => {
      this.log?.(`[service] spawn error: ${err.message}`);
      // Reject anything that was written before the spawn failed — but only
      // for THIS process generation, and only tear down if this child is still
      // the current one (a replacement may already be running).
      this.teardown(gen, child, `[editchain] service failed to start: ${err.message}`);
    });
    child.on('exit', (code) => {
      this.log?.(`[service] exited with code ${code}`);
      // Fail outstanding requests instead of letting them hang forever: the
      // process is gone, so no response can ever arrive. The webview shows the
      // error and the next `open` restarts the service.
      this.teardown(
        gen,
        child,
        code === null || code === 0
          ? '[editchain] service exited unexpectedly'
          : `[editchain] service exited with code ${code}`
      );
    });
  }

  /**
   * Reject every pending request issued against process generation `gen`, and
   * drop the process's slots — but ONLY if `child` is still the current
   * process. A stale exit/error event from an old child can arrive after a
   * replacement has started (e.g. the old process was killed and `start`
   * re-spawned before its event was delivered); without the identity check it
   * would null out the replacement and strand its requests.
   */
  private teardown(
    gen: number,
    child: ChildProcessWithoutNullStreams,
    reason: string
  ): void {
    this.rejectPending(reason, gen);
    // The child can still be alive on the stdin-error / spawn-error paths;
    // kill it so a dropped process can never outlive the client. Safe no-op
    // when the child already exited (the 'exit' path).
    child.kill();
    this.children.delete(gen);
    if (this.proc !== child) return;
    this.running = false;
    this.proc = null;
  }

  /** Ensure the service is running, starting it if needed. */
  ensureStarted(binaryPath: string): void {
    if (!this.isRunning()) {
      this.start(binaryPath);
    }
  }

  /** Register a handler for unsolicited messages (e.g. updates). */
  setMessageHandler(handler: (msg: any) => void): void {
    this.onMessage = handler;
  }

  /**
   * Send a request and await its response.
   *
   * There is NO artificial deadline by default: the caller opts into a timeout
   * via `opts.timeoutMs`. Requests always settle when the process answers,
   * exits, or is stopped, so a slow operation like `Open` (which builds the
   * chain + git graph and can take minutes on a large workspace) can never be
   * killed by a fixed 10s timeout.
   */
  request(body: any, opts?: { timeoutMs?: number }): Promise<any> {
    const proc = this.proc;
    if (!this.running || !proc) {
      return Promise.reject(new Error('[editchain] service is not running'));
    }
    const id = this.nextId++;
    const msg = { id, body };
    return new Promise((resolve, reject) => {
      const req: PendingRequest = {
        resolve,
        reject,
        timer: null,
        gen: this.generation,
      };
      const timeoutMs = opts?.timeoutMs;
      if (typeof timeoutMs === 'number' && timeoutMs > 0) {
        req.timer = setTimeout(() => {
          if (this.pending.delete(id)) {
            reject(new Error(`[editchain] request timed out after ${timeoutMs}ms`));
          }
        }, timeoutMs);
      }
      this.pending.set(id, req);
      const payload = Buffer.from(JSON.stringify(msg), 'utf8');
      const header = Buffer.alloc(4);
      header.writeUInt32LE(payload.length, 0);
      try {
        proc.stdin.write(Buffer.concat([header, payload]));
      } catch (e) {
        // The write failed synchronously (e.g. writing to a destroyed stdin
        // stream). The process can no longer receive requests, so tear down
        // the WHOLE generation — rejecting every same-gen pending request —
        // and clear the current process slot. Otherwise isRunning() would
        // stay true and ensureStarted() would never restart the service,
        // leaving every future request to fail the same way. Asynchronous
        // failures (EPIPE once the child dies) surface on the stdin 'error'
        // listener installed in start().
        this.teardown(req.gen, proc, `[editchain] service write failed: ${(e as Error).message}`);
      }
    });
  }

  /** Stop the service process, rejecting any in-flight requests. */
  stop(): void {
    this.rejectPending('[editchain] service stopped');
    // Kill every child we have spawned — not just the current one — so a
    // previous generation that ignored SIGTERM (or hasn't exited yet) can
    // never keep running after the client is stopped.
    for (const [, child] of this.children) {
      child.kill();
    }
    this.children.clear();
    this.proc = null;
    this.running = false;
  }

  /**
   * Reject and drop pending requests with `reason`. When `gen` is given, only
   * requests issued against that process generation are rejected, so a stale
   * event from a dead child can never fail requests of a replacement process.
   */
  private rejectPending(reason: string, gen?: number): void {
    if (this.pending.size === 0) return;
    for (const [id, req] of this.pending) {
      if (gen !== undefined && req.gen !== gen) continue;
      this.pending.delete(id);
      if (req.timer) clearTimeout(req.timer);
      req.reject(new Error(reason));
    }
  }

  /**
   * Parse framed responses from one process generation's stdout.
   *
   * `gen`/`child` are bound at start(): if the child that emitted `chunk` is
   * no longer the current process, the bytes are STALE — a killed child's
   * partial frame or late data must never be buffered into (or parsed against)
   * the replacement's framing state.
   */
  private onData(
    gen: number,
    child: ChildProcessWithoutNullStreams,
    chunk: Buffer
  ): void {
    const framing = this.framing;
    if (!framing || framing.gen !== gen || this.proc !== child) return;
    framing.buffer = Buffer.concat([framing.buffer, chunk]);
    while (framing.buffer.length >= 4) {
      const len = framing.buffer.readUInt32LE(0);
      if (framing.buffer.length < 4 + len) {
        break;
      }
      const payload = framing.buffer.subarray(4, 4 + len).toString('utf8');
      framing.buffer = framing.buffer.subarray(4 + len);
      try {
        const msg = JSON.parse(payload);
        if (msg.id !== undefined) {
          const req = this.pending.get(msg.id);
          // Responses with an id that matches no pending request (e.g. a stale
          // response from a previous process generation) are DROPPED, never
          // treated as unsolicited updates. Only id-less messages are
          // unsolicited.
          if (!req) continue;
          this.pending.delete(msg.id);
          if (req.timer) clearTimeout(req.timer);
          req.resolve(msg.body);
        } else if (this.onMessage) {
          this.onMessage(msg);
        }
      } catch (e) {
        console.error(`[editchain-service] bad message: ${e}`);
      }
    }
  }
}

interface Framing {
  /** Process generation this framing state belongs to. */
  gen: number;
  buffer: Buffer;
}

interface PendingRequest {
  resolve: (resp: any) => void;
  reject: (err: Error) => void;
  timer: NodeJS.Timeout | null;
  /** Process generation this request was issued against. */
  gen: number;
}

/** Resolve the path to the Rust service binary. */
export function resolveServicePath(): string {
  const configured = vscode.workspace
    .getConfiguration('editchain-history')
    .get<string>('servicePath', '');
  if (configured) {
    return configured;
  }
  // Fall back to a debug build path relative to the workspace.
  return vscode.Uri.joinPath(
    vscode.workspace.workspaceFolders?.[0]?.uri ?? vscode.Uri.file('.'),
    'target',
    'debug',
    'editchain-vscode-service'
  ).fsPath;
}
