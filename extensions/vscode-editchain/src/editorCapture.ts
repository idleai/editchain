import * as vscode from 'vscode';
import * as path from 'node:path';
import { randomUUID } from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { setTimeout, clearTimeout } from 'node:timers';
import { EditorEvent } from './editorOutbox';
import type { HumanIdentity } from './humanIdentity';

type Document = { id: string; uri: string; path: string | null; version: number };
type Range = { start: [number, number]; end: [number, number] };
type BufferState = { document: Document; text: string };
type Exposure = { document: Document; editor: string; ranges: Range[]; started_ms: number; monotonic: number; reported: boolean };

/** Stable VS Code API observer; no proposed APIs and no persisted focus history. */
export class EditorCapture {
  private readonly session = randomUUID();
  private sequence = 0;
  private time = 0;
  private serial = 0;
  private readonly ids = new WeakMap<object, string>();
  private readonly documents = new Map<vscode.TextDocument, BufferState>();
  private readonly subscriptions: vscode.Disposable[] = [];
  private readonly pending = new Map<vscode.TextDocument, { sequence: number; version: number; at: number }[]>();
  private readonly skipped = new Set<string>();
  // Keep the last view's receipt when focus/activation/context ends its timer.
  // A hidden tab can return with a new TextEditor object for the same document.
  private readonly views = new WeakMap<vscode.TextDocument, Exposure>();
  private exposure: Exposure | undefined;
  private stopped = false;
  private timer: NodeJS.Timeout | undefined;

  constructor(private readonly folder: vscode.WorkspaceFolder, private readonly dwell: number,
    private readonly maxFileBytes: number, private readonly emit: (event: EditorEvent) => boolean,
    private readonly attribution?: HumanIdentity) {
    this.record({ type: 'tracking_started', dwell_ms: dwell, vscode_version: vscode.version, activity_schema: attribution ? 3 : 2 });
    this.subscriptions.push(
      vscode.workspace.onDidOpenTextDocument(document => { this.baseline(document); }),
      vscode.workspace.onDidCloseTextDocument(document => {
        if (this.exposure?.document.id === this.documents.get(document)?.document.id) this.endExposure();
        this.documents.delete(document); this.pending.delete(document);
      }),
      vscode.workspace.onDidChangeTextDocument(event => this.changed(event)),
      vscode.workspace.onDidSaveTextDocument(document => {
        const state = this.documents.get(document);
        if (state) this.record({ type: 'document_saved', document: state.document });
      }),
      vscode.workspace.onDidRenameFiles(event => {
        for (const file of event.files) {
          const from = this.relative(file.oldUri), to = this.relative(file.newUri);
          if (from !== null && to !== null) this.record({ type: 'document_renamed', from, to });
        }
      }),
      vscode.window.onDidChangeActiveTextEditor(editor => {
        if (!editor || (this.exposure && (this.exposure.editor !== this.identity(editor)
          || !this.sameView(this.exposure, editor)))) this.endExposure();
        const state = editor && this.baseline(editor.document);
        this.record({ type: 'editor_activated', document: state?.document ?? null });
        this.viewport();
      }),
      vscode.window.onDidChangeVisibleTextEditors(() => this.viewport()),
      vscode.window.onDidChangeTextEditorVisibleRanges(() => this.viewport()),
      vscode.window.onDidChangeTextEditorSelection(event => this.selection(event)),
      // Focus is an in-memory timer guard only; deliberately emit no event.
      vscode.window.onDidChangeWindowState(() => this.viewport()),
      vscode.window.tabGroups.onDidChangeTabs(event => {
        if (event.closed.length) this.viewport();
        for (const tab of event.opened) this.tab(tab, 'editor_opened');
        for (const tab of event.closed) this.tab(tab, 'editor_closed');
      }),
    );
    for (const document of vscode.workspace.textDocuments) this.baseline(document);
    for (const group of vscode.window.tabGroups.all) for (const tab of group.tabs) this.tab(tab, 'editor_opened');
    this.viewport();
  }

  private identity(object: object): string {
    let id = this.ids.get(object);
    if (!id) { id = String(++this.serial); this.ids.set(object, id); }
    return id;
  }

  private relative(uri: vscode.Uri): string | null {
    if (uri.scheme !== 'file') return null;
    const relative = path.relative(this.folder.uri.fsPath, uri.fsPath);
    if (!relative || relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) return null;
    return relative.split(path.sep).join('/');
  }

  private tracked(document: vscode.TextDocument): boolean {
    return this.relative(document.uri) !== null || (document.isUntitled && this.folder.index === 0);
  }

  private baseline(document: vscode.TextDocument): BufferState | undefined {
    const existing = this.documents.get(document);
    if (existing) return existing;
    if (!this.tracked(document)) return undefined;
    const text = document.getText();
    if (!this.withinLimit(document, text)) return undefined;
    const state = { document: { id: String(++this.serial), uri: document.uri.toString(),
      path: this.relative(document.uri), version: document.version }, text };
    this.documents.set(document, state);
    this.record({ type: 'document_snapshot', ...state });
    return state;
  }

  private withinLimit(document: vscode.TextDocument, text: string): boolean {
    if (Buffer.byteLength(text) <= this.maxFileBytes && !text.includes('\0')) return true;
    const uri = document.uri.toString();
    if (!this.skipped.has(uri)) {
      this.skipped.add(uri);
      this.record({ type: 'tracking_gap', reason: `Buffer skipped (binary or over ${this.maxFileBytes} bytes): ${uri}` });
    }
    return false;
  }

  private changed(event: vscode.TextDocumentChangeEvent): void {
    if (!this.tracked(event.document) || !event.contentChanges.length) return;
    if (this.exposure?.document.id === this.documents.get(event.document)?.document.id) this.endExposure();
    const before = this.documents.get(event.document);
    const after = event.document.getText();
    if (!this.withinLimit(event.document, after)) {
      this.documents.delete(event.document); this.pending.delete(event.document); this.viewport(); return;
    }
    if (!before) {
      this.record({ type: 'tracking_gap', reason: `Change preceded buffer baseline: ${event.document.uri.toString()}` });
      this.baseline(event.document); this.viewport(); return;
    }
    const document = { ...before.document, uri: event.document.uri.toString(),
      path: this.relative(event.document.uri), version: event.document.version };
    const reason = event.reason === vscode.TextDocumentChangeReason.Undo ? 'undo'
      : event.reason === vscode.TextDocumentChangeReason.Redo ? 'redo' : null;
    const sequence = this.record({ type: 'document_changed', document, before_version: before.document.version,
      before: before.text, after, reason, changes: event.contentChanges.map(change => ({
        offset: change.rangeOffset, length: change.rangeLength, text: change.text,
      })) });
    this.documents.set(event.document, { document, text: after });
    if (sequence && vscode.window.state.focused && vscode.window.activeTextEditor?.document === event.document) {
      if (reason) this.record({ type: 'human_edit', change: sequence, signal: reason });
      else {
        const pending = this.pending.get(event.document) ?? [];
        pending.push({ sequence, version: document.version, at: performance.now() });
        this.pending.set(event.document, pending.filter(change => performance.now() - change.at <= 250));
      }
    }
    this.viewport();
  }

  private selection(event: vscode.TextEditorSelectionChangeEvent): void {
    const state = this.baseline(event.textEditor.document);
    if (!state) return;
    const keyboard = event.kind === vscode.TextEditorSelectionChangeKind.Keyboard;
    if (keyboard && vscode.window.state.focused) {
      for (const change of this.pending.get(event.textEditor.document) ?? []) {
        if (change.version <= state.document.version && performance.now() - change.at <= 250) {
          this.record({ type: 'human_edit', change: change.sequence, signal: 'keyboard_selection' });
        }
      }
      this.pending.delete(event.textEditor.document);
    }
  }

  private tab(tab: vscode.Tab, type: string): void {
    if (!(tab.input instanceof vscode.TabInputText)) return;
    const uri = tab.input.uri;
    if (this.relative(uri) === null && !(uri.scheme === 'untitled' && this.folder.index === 0)) return;
    this.record({ type, editor: this.identity(tab), uri: uri.toString(), path: this.relative(uri) });
  }

  private viewport(): void {
    const editor = vscode.window.activeTextEditor;
    const current = this.exposure;
    if (current && editor && vscode.window.state.focused && vscode.window.visibleTextEditors.includes(editor)
      && current.editor === this.identity(editor) && this.sameView(current, editor)) return;
    this.endExposure();
    this.beginExposure();
  }

  private sameView(view: Exposure, editor: vscode.TextEditor): boolean {
    return view.document.id === this.documents.get(editor.document)?.document.id
      && view.document.version === editor.document.version
      && JSON.stringify(view.ranges) === JSON.stringify(editor.visibleRanges.map(range));
  }

  private beginExposure(): void {
    if (this.stopped || !vscode.window.state.focused) return;
    const editor = vscode.window.activeTextEditor;
    if (!editor || !vscode.window.visibleTextEditors.includes(editor)) return;
    const state = this.baseline(editor.document);
    if (!state || !editor.visibleRanges.length) return;
    const previous = this.views.get(editor.document);
    const reported = !!previous?.reported && this.sameView(previous, editor);
    this.exposure = { document: { ...state.document }, editor: this.identity(editor), ranges: editor.visibleRanges.map(range),
      started_ms: Date.now(), monotonic: performance.now(), reported };
    this.views.set(editor.document, this.exposure);
    if (!reported) this.scheduleRead(this.dwell);
  }

  private scheduleRead(delay: number): void {
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.publishRead();
      const exposure = this.exposure;
      if (exposure && !exposure.reported && !this.stopped) {
        // Timers can fire just before the measured dwell reaches a whole millisecond.
        this.scheduleRead(Math.max(1, this.dwell - (performance.now() - exposure.monotonic)));
      }
    }, delay);
    this.timer.unref();
  }

  private publishRead(): void {
    const exposure = this.exposure;
    if (!exposure || exposure.reported) return;
    const duration_ms = Math.max(0, Math.min(60000, Math.floor(performance.now() - exposure.monotonic)));
    if (duration_ms < this.dwell) return;
    const { document, editor, ranges, started_ms } = exposure;
    this.record({ type: 'code_read', document, editor, ranges, started_ms, duration_ms });
    exposure.reported = true;
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
  }

  private endExposure(): void {
    this.publishRead();
    this.exposure = undefined;
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
  }

  private record(event: EditorEvent['event']): number | undefined {
    if (this.stopped) return undefined;
    this.time = Math.max(this.time, Date.now());
    const sequence = this.sequence + 1;
    if (!this.emit({ schema: 1, session: this.session, ...(this.attribution ? { identity: this.attribution } : {}), sequence, time_ms: this.time, event })) {
      this.stopped = true; return undefined;
    }
    this.sequence = sequence;
    return sequence;
  }

  checkpoint(): void { this.publishRead(); }

  context(context: { observed_ms: number; workspace_path?: string; repositories: unknown[] }): void {
    this.endExposure();
    this.record({ type: 'workspace_context', ...context });
    this.beginExposure();
  }

  dispose(): void {
    this.endExposure();
    this.record({ type: 'tracking_stopped' });
    this.stopped = true;
    for (const subscription of this.subscriptions) subscription.dispose();
    this.documents.clear(); this.pending.clear();
  }
}

function range(value: vscode.Range): Range {
  return { start: [value.start.line, value.start.character], end: [value.end.line, value.end.character] };
}
