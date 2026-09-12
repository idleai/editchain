/** Single owner of import + publication. Events during a pass schedule one
 * follow-up; checkpoints are advanced only after successful durable imports. */
export interface LiveCapture {
  sessions: Map<string, string>;
  titles: string;
  history: string;
}

export interface LiveActions {
  capture(): Promise<LiveCapture>;
  importFiles(files: string[], signal: AbortSignal): Promise<void>;
  publish(): Promise<void>;
  status(text: string): void;
  /** The retained service checks its frontier itself; no chain inventory. */
  pollNative?: boolean;
}

export class LiveSync {
  private accepted = new Map<string, string>();
  private titles: string | undefined;
  private history: string | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private running = false;
  private dirty = false;
  private failures = 0;
  private started = false;
  private abort = new AbortController();

  constructor(private actions: LiveActions, private intervalMs = 1500) {}

  wake(): void {
    if (this.abort.signal.aborted) return;
    this.dirty = true;
    if (this.timer) clearTimeout(this.timer);
    if (!this.running) void this.run();
  }

  dispose(): void {
    this.abort.abort();
    if (this.timer) clearTimeout(this.timer);
  }

  private async run(): Promise<void> {
    this.running = true;
    this.dirty = false;
    try {
      if (!this.started) {
        this.started = true;
        this.actions.status('Scanning Codex sessions…');
      }
      const captured = await this.actions.capture();
      if (this.abort.signal.aborted) return;
      const pending = [...captured.sessions].filter(([file, stamp]) =>
        this.accepted.get(file) !== stamp || this.titles !== captured.titles
      ).map(([file]) => file);
      // Publish each durable batch before walking more of the initial archive.
      // captureSources orders recently modified sessions first, so ongoing work
      // does not wait behind every historical rollout on startup.
      const files = pending.slice(0, 32);
      if (files.length) {
        this.actions.status(`Importing Codex changes (${files.length}/${pending.length} queued)…`);
        await this.actions.importFiles(files, this.abort.signal);
      }
      if (this.abort.signal.aborted) return;
      // Keep the stamps captured BEFORE import. Growth during capture/import
      // is picked up on the next pass, even if the exporter accepted only a prefix.
      this.accepted = this.titles !== captured.titles ? new Map() :
        new Map([...this.accepted].filter(([file]) => captured.sessions.has(file)));
      for (const file of files) this.accepted.set(file, captured.sessions.get(file)!);
      this.titles = captured.titles;
      this.dirty ||= pending.length > files.length;
      const afterImport = files.length && !this.actions.pollNative ? await this.actions.capture() : captured;
      if (this.abort.signal.aborted) return;
      if (this.actions.pollNative ? !files.length : this.history !== afterImport.history) {
        if (!this.actions.pollNative) this.actions.status('Updating history…');
        await this.actions.publish();
        if (this.abort.signal.aborted) return;
        this.history = afterImport.history;
      }
      this.failures = 0;
      this.actions.status(pending.length > files.length
        ? `Catching up Codex (${pending.length - files.length} queued)…`
        : captured.sessions.size ? 'Live · Codex + Git' : 'Live · waiting for Codex sessions');
    } catch (error) {
      if (!this.abort.signal.aborted) {
        this.failures++;
        this.actions.status(`Live retry: ${String(error)}`);
      }
    } finally {
      this.running = false;
      if (!this.abort.signal.aborted) {
        const delay = this.failures ? Math.min(30_000, this.intervalMs * 2 ** this.failures) :
          this.dirty ? 0 : this.intervalMs;
        this.timer = setTimeout(() => this.wake(), delay);
      }
    }
  }
}
