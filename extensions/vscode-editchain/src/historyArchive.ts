import { promises as fs, realpathSync } from 'node:fs';
import type { FileHandle } from 'node:fs/promises';
import * as path from 'node:path';
import { homedir } from 'node:os';
import type { EditorEvent } from './editorOutbox';
import { HISTORY_ARCHIVE_EVENT_BYTES, HISTORY_ARCHIVE_QUEUE_BYTES } from './editorLimits';

/**
 * Portable source archive for human-work capture.
 *
 * One line is one recorder event with its original workspace, so the archive
 * can rebuild a chain that no longer exists. It never stores derived chain
 * records or references to chain blobs: every payload the importer needs is
 * embedded in the event itself.
 */
export const HISTORY_ARCHIVE_FORMAT = 'editchain-human-history';
export const HISTORY_ARCHIVE_SCHEMA = 1;
/** Session files sort by date, then by a per-day counter that never reuses holes. */
export const HISTORY_ARCHIVE_NAME = /^(\d{4}-\d{2}-\d{2})-session-(\d{4,})\.jsonl$/;
const ALLOCATION_ATTEMPTS = 1024;

export type HistoryArchiveLine = {
  format: typeof HISTORY_ARCHIVE_FORMAT;
  schema: typeof HISTORY_ARCHIVE_SCHEMA;
  workspace_path: string;
  event: EditorEvent;
};

/** The agreed version-1 record: a full event with its original workspace. */
export function historyArchiveLine(workspacePath: string, event: EditorEvent): Buffer {
  return Buffer.from(`${JSON.stringify({ format: HISTORY_ARCHIVE_FORMAT, schema: HISTORY_ARCHIVE_SCHEMA,
    workspace_path: workspacePath, event })}\n`);
}

/** Local calendar day of the allocation, stable for the life of the file. */
export function archiveDay(now: Date): string {
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

export function archiveFileName(day: string, counter: number): string {
  return `${day}-session-${String(counter).padStart(4, '0')}.jsonl`;
}

/**
 * Resolve the configured archive directory.
 *
 * Empty uses the extension's own global storage. `~` and `~/…` use the home
 * directory. A relative path needs exactly one workspace folder to resolve
 * against; with several folders the intent is ambiguous, so the archive is
 * refused instead of guessing.
 */
export function archiveDirectory(setting: string, workspaces: readonly string[],
  fallback: string): { directory: string } | { error: string } {
  const value = setting.trim();
  if (!value) return { directory: fallback };
  const expanded = value === '~' ? homedir()
    : value.startsWith('~/') || value.startsWith('~\\') ? path.join(homedir(), value.slice(2)) : value;
  if (path.isAbsolute(expanded)) return { directory: path.normalize(expanded) };
  if (workspaces.length === 1) return { directory: path.resolve(workspaces[0], expanded) };
  return { error: workspaces.length
    ? `directory "${value}" is relative but this window has ${workspaces.length} workspace folders; set an absolute directory`
    : `directory "${value}" is relative but no workspace folder is open; set an absolute directory` };
}

export type HistoryArchiveOptions = {
  directory: string;
  log: (line: string) => void;
  report: (message: string) => void;
  now?: () => Date;
};

/**
 * One activation's archive session file.
 *
 * The file is allocated on the first event and then kept for the life of the
 * activation: tracking restarts append to it, and only a reload (or a new
 * destination) allocates another. Writes are ordered, bounded, and drained on
 * stop. A failed write is surfaced once and stops archiving instead of
 * pretending later events were stored.
 */
export class HistoryArchive {
  private readonly queue: Buffer[] = [];
  private bytes = 0;
  private running: Promise<void> | undefined;
  private handle: FileHandle | undefined;
  private file: string | undefined;
  private failure: string | undefined;
  private stopped = false;
  private stopping: Promise<void> | undefined;
  private readonly directory: string;
  private canonical: string | undefined;
  private readonly aliases = new Set<string>();
  private ready: Promise<void> | undefined;

  constructor(private readonly options: HistoryArchiveOptions) {
    this.directory = path.resolve(options.directory);
  }

  /** The session file, once the first event created it. */
  get location(): string | undefined { return this.file; }
  get failed(): boolean { return this.failure !== undefined; }

  /**
   * Cache the destination's physical path before capture baselines a document.
   *
   * VS Code reports a file's physical path, so a configured archive directory
   * that is (or contains) a symlink must be recognized by its real path too.
   * Any other alias of the destination is resolved on demand by `excludes`.
   */
  async setup(): Promise<void> {
    this.ready ??= this.prepare();
    return this.ready;
  }

  private async prepare(): Promise<void> {
    this.canonical = await physicalDirectory(this.directory);
  }

  /** Never re-record our own output, including through a directory alias. */
  excludes(fsPath: string): boolean {
    // The filename guard runs first, so ordinary typing never leaves it.
    if (!HISTORY_ARCHIVE_NAME.test(path.basename(fsPath))) return false;
    const parent = path.resolve(path.dirname(fsPath));
    if (parent === this.directory || parent === this.canonical) return true;
    if (this.aliases.has(parent)) return true;
    // Any other spelling (a symlinked archive, workspace, or unrelated link)
    // only needs one physical resolution of its parent directory. Only hits
    // are remembered so a link created later is still recognized.
    if (!this.canonical || physicalParent(parent) !== this.canonical) return false;
    this.aliases.add(parent);
    return true;
  }

  append(workspacePath: string, event: EditorEvent): void {
    if (this.stopped || this.failure) return;
    const line = historyArchiveLine(workspacePath, event);
    if (line.length > HISTORY_ARCHIVE_EVENT_BYTES) {
      this.fail(`an event of ${line.length} bytes exceeds the archive record limit`);
      return;
    }
    if (this.bytes && this.bytes + line.length > HISTORY_ARCHIVE_QUEUE_BYTES) {
      this.fail('pending archive writes exceed the local limit');
      return;
    }
    this.queue.push(line);
    this.bytes += line.length;
    this.start();
  }

  /** Concurrent callers share one drain, so none can return early. */
  stop(): Promise<void> {
    this.stopped = true;
    this.stopping ??= this.drain();
    return this.stopping;
  }

  private async drain(): Promise<void> {
    // A capacity failure can clear the queue while a write is still
    // allocating or writing its current line, so always settle that write
    // before touching the handle.
    while (this.running || (!this.failure && this.queue.length)) {
      this.start();
      await this.running;
    }
    const handle = this.handle;
    this.handle = undefined;
    if (!handle) return;
    // Close can report a delayed write-back error, so a shutdown failure is
    // surfaced instead of being reported as a clean stop.
    const flush = await handle.sync().then(() => undefined,
      error => `could not flush the archive on shutdown: ${String(error)}`);
    const closed = await handle.close().then(() => undefined,
      error => `could not close the archive: ${String(error)}`);
    const problem = flush ?? closed;
    if (problem) {
      this.options.log(`[capture] Human history archive ${problem}`);
      this.options.report(problem);
    }
  }

  private start(): void {
    if (this.running || this.failure || !this.queue.length) return;
    this.running = this.write().catch(error => this.fail(String(error))).finally(() => {
      this.running = undefined;
      if (!this.failure && this.queue.length) this.start();
    });
  }

  private async write(): Promise<void> {
    while (this.queue.length) {
      const line = this.queue.shift() as Buffer;
      this.bytes -= line.length;
      if (!this.handle) {
        await fs.mkdir(this.directory, { recursive: true });
        await this.setup();
        const allocated = await allocateArchive(this.directory, (this.options.now ?? (() => new Date()))());
        this.handle = allocated.handle;
        this.file = allocated.file;
        this.options.log(`[capture] Human history archive: ${allocated.file}`);
      }
      await writeAll(this.handle, line);
    }
    await this.handle?.sync();
  }

  private fail(reason: string): void {
    if (this.failure) return;
    this.failure = reason;
    this.queue.length = 0;
    this.bytes = 0;
    // A failed writer keeps its destination for this activation, so the only
    // way to resume archiving is a reload that starts a fresh file.
    const message = `archiving stopped: ${reason}. Reload the window to resume.`;
    this.options.log(`[capture] Human history archive ${message}`);
    this.options.report(message);
  }
}

async function writeAll(handle: FileHandle, content: Buffer): Promise<void> {
  let offset = 0;
  while (offset < content.length) {
    const { bytesWritten } = await handle.write(content, offset, content.length - offset);
    if (!bytesWritten) throw new Error('human history archive write made no progress');
    offset += bytesWritten;
  }
}

/** Reserve the next name above the day's existing maximum; holes stay unused. */
async function allocateArchive(directory: string, now: Date): Promise<{ handle: FileHandle; file: string }> {
  const day = archiveDay(now);
  let counter = (await highestArchiveCounter(directory, day)) + 1;
  for (let attempt = 0; attempt < ALLOCATION_ATTEMPTS; attempt++, counter++) {
    const file = path.join(directory, archiveFileName(day, counter));
    try { return { handle: await fs.open(file, 'wx', 0o600), file }; }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error; }
  }
  throw new Error(`cannot allocate a human history archive name in ${directory}`);
}

async function highestArchiveCounter(directory: string, day: string): Promise<number> {
  let names: string[];
  try { names = await fs.readdir(directory); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return 0; throw error; }
  let highest = 0;
  for (const name of names) {
    const match = HISTORY_ARCHIVE_NAME.exec(name);
    if (match && match[1] === day) highest = Math.max(highest, Number(match[2]));
  }
  return highest;
}

/** The real path, or undefined while the directory does not exist yet. */
async function physicalDirectory(value: string): Promise<string | undefined> {
  try { return await fs.realpath(value); }
  catch { return undefined; }
}

/** The physical parent directory, or undefined when it cannot be resolved. */
function physicalParent(value: string): string | undefined {
  try { return realpathSync(value); }
  catch { return undefined; }
}
