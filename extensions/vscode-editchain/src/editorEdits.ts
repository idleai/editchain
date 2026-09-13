import { performance } from 'node:perf_hooks';
import { setTimeout, clearTimeout } from 'node:timers';
import type { EditorEvent } from './editorOutbox';
import { insertedRanges, retractsInserted, type InputChange } from './editorInput';

type Receipt = { change: number; signal: string };
type Burst = { document: object; editor: object; version: number; group: number; last: number; edits: Receipt[]; inserted: [number, number][] };

/** Group only confirmed, contiguous input. Raw buffer revisions remain separate evidence. */
export class EditorEdits {
  private burst: Burst | undefined;
  private timer: NodeJS.Timeout | undefined;

  constructor(private readonly emit: (event: EditorEvent['event']) => void) {}

  beforeChange(document: object, version: number, editor: object | undefined): void {
    const burst = this.burst;
    if (burst && burst.document === document && (burst.version !== version || burst.editor !== editor
      || performance.now() - burst.last >= 30000)) this.flush();
  }

  activate(editor: object | undefined): void {
    if (this.burst && this.burst.editor !== editor) this.flush();
  }

  corrects(document: object, version: number, changes: readonly InputChange[]): boolean {
    return !!this.burst && this.burst.document === document && this.burst.version === version
      && retractsInserted(this.burst.inserted, changes);
  }

  interrupt(document: object): void {
    if (this.burst?.document === document) this.flush();
  }

  read(document: object, version: number): number | undefined {
    this.publish();
    return this.burst?.document === document && this.burst.version === version ? this.burst.group : undefined;
  }

  add(document: object, editor: object, beforeVersion: number, version: number, change: number, signal: string,
    changes: readonly InputChange[]): void {
    if (this.burst && (this.burst.document !== document || this.burst.editor !== editor)) this.flush();
    this.beforeChange(document, beforeVersion, editor);
    if (signal === 'undo' || signal === 'redo') {
      this.flush();
      this.emit({ type: 'human_edit', change, signal });
      return;
    }
    const first = !this.burst;
    const burst = this.burst ?? { document, editor, version, group: change, last: performance.now(), edits: [], inserted: [] };
    burst.version = version;
    burst.last = performance.now();
    burst.edits.push({ change, signal });
    burst.inserted = insertedRanges(burst.inserted, changes);
    this.burst = burst;
    // Publish the first receipt immediately. Later frames update its logical
    // row; publishing must never end the editing episode or wait for a save.
    if (first || burst.edits.length >= 128) this.publish();
    else if (!this.timer) {
      this.timer = setTimeout(() => this.publish(), 100);
      this.timer.unref();
    }
  }

  flush(): void {
    this.publish();
    this.burst = undefined;
  }

  private publish(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    const burst = this.burst;
    if (!burst?.edits.length) return;
    this.emit({ type: 'human_edit_batch', group: burst.group, edits: burst.edits });
    burst.edits = [];
  }
}
