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
  private buffer: Buffer = Buffer.alloc(0);
  private nextId = 1;
  private pending = new Map<number, (resp: any) => void>();
  private onMessage: ((msg: any) => void) | null = null;
  private log: ((line: string) => void) | null = null;

  /** Register a log sink for service stderr/exit messages. */
  setLog(sink: (line: string) => void): void {
    this.log = sink;
  }

  /** Start the Rust service binary. */
  start(binaryPath: string): void {
    this.log?.(`spawning ${binaryPath}`);
    this.proc = spawn(binaryPath, [], { stdio: ['pipe', 'pipe', 'pipe'] });
    this.proc.stdout.on('data', (chunk: Buffer) => this.onData(chunk));
    this.proc.stderr.on('data', (chunk: Buffer) => {
      this.log?.(`[service] ${chunk.toString()}`);
    });
    this.proc.on('error', (err) => {
      this.log?.(`[service] spawn error: ${err.message}`);
      this.proc = null;
    });
    this.proc.on('exit', (code) => {
      this.log?.(`[service] exited with code ${code}`);
      this.proc = null;
    });
  }

  /** Register a handler for unsolicited messages (e.g. updates). */
  setMessageHandler(handler: (msg: any) => void): void {
    this.onMessage = handler;
  }

  /** Send a request and await its response. */
  request(body: any): Promise<any> {
    const id = this.nextId++;
    const msg = { id, body };
    return new Promise((resolve, reject) => {
      this.pending.set(id, resolve);
      const payload = Buffer.from(JSON.stringify(msg), 'utf8');
      const header = Buffer.alloc(4);
      header.writeUInt32LE(payload.length, 0);
      this.proc?.stdin.write(Buffer.concat([header, payload]));
      // Timeout to avoid hanging the UI.
      setTimeout(() => {
        if (this.pending.delete(id)) {
          reject(new Error('request timed out'));
        }
      }, 10_000);
    });
  }

  /** Stop the service process. */
  stop(): void {
    this.proc?.kill();
    this.proc = null;
    this.pending.clear();
  }

  private onData(chunk: Buffer): void {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (this.buffer.length >= 4) {
      const len = this.buffer.readUInt32LE(0);
      if (this.buffer.length < 4 + len) {
        break;
      }
      const payload = this.buffer.subarray(4, 4 + len).toString('utf8');
      this.buffer = this.buffer.subarray(4 + len);
      try {
        const msg = JSON.parse(payload);
        if (msg.id !== undefined && this.pending.has(msg.id)) {
          const resolve = this.pending.get(msg.id)!;
          this.pending.delete(msg.id);
          resolve(msg.body);
        } else if (this.onMessage) {
          this.onMessage(msg);
        }
      } catch (e) {
        console.error(`[editchain-service] bad message: ${e}`);
      }
    }
  }
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