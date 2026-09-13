import { performance } from 'node:perf_hooks';
import { setTimeout, clearTimeout } from 'node:timers';
import type { EditorEvent } from './editorOutbox';

type Receipt = { change: number; signal: string };
type Burst = { document: object; editor: object; version: number; started: number; last: number; edits: Receipt[] };

/** Group only confirmed, contiguous input. Raw buffer revisions remain separate evidence. */
export class EditorEdits {
  private burst: Burst | undefined;
  private timer: NodeJS.Timeout | undefined;

  constructor(private readonly emit: (event: EditorEvent['event']) => void) {}

  beforeChange(document: object, version: number, editor: object | undefined): void {
    const burst = this.burst;
    if (burst && (burst.document !== document || burst.version !== version || burst.editor !== editor
      || performance.now() - burst.last >= 1000 || performance.now() - burst.started >= 30000)) this.flush();
  }

  activate(editor: object | undefined): void {
    if (this.burst && this.burst.editor !== editor) this.flush();
  }

  add(document: object, editor: object, beforeVersion: number, version: number, change: number, signal: string): void {
    this.beforeChange(document, beforeVersion, editor);
    if (signal === 'undo' || signal === 'redo') {
      this.flush();
      this.emit({ type: 'human_edit', change, signal });
      return;
    }
    const burst = this.burst ?? { document, editor, version, started: performance.now(), last: performance.now(), edits: [] };
    burst.version = version;
    burst.last = performance.now();
    burst.edits.push({ change, signal });
    this.burst = burst;
    if (this.timer) clearTimeout(this.timer);
    // Bound latency and memory during uninterrupted typing, as well as idle typing.
    if (burst.edits.length >= 1024) this.flush();
    else {
      this.timer = setTimeout(() => this.flush(), Math.max(0, Math.min(1000, 30000 - (performance.now() - burst.started))));
      this.timer.unref();
    }
  }

  flush(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    const burst = this.burst;
    this.burst = undefined;
    if (!burst) return;
    this.emit(burst.edits.length === 1
      ? { type: 'human_edit', ...burst.edits[0] }
      : { type: 'human_edit_batch', edits: burst.edits });
  }
}
