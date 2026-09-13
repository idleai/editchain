import * as vscode from 'vscode';
import * as path from 'node:path';
import { randomUUID } from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { setTimeout, clearTimeout } from 'node:timers';
import { EditorEvent } from './editorOutbox';
import type { HumanIdentity } from './humanIdentity';
import { editorOrigin, isEditorInput } from './editorOrigin';
import { EditorEdits } from './editorEdits';
import { EditorTabs } from './editorTabs';
import type { InputChange } from './editorInput';

type Document = { id: string; uri: string; path: string | null; version: number };
type Range = { start: [number, number]; end: [number, number] };
type BufferState = { document: Document; text: string };
type Exposure = { document: Document; editor: string; ranges: Range[]; started_ms: number; monotonic: number; reported: boolean };

/** VS Code observations, with optional change-origin evidence and local focus guards. */
export class EditorCapture {
  private readonly session = randomUUID();
  private sequence = 0;
  private time = 0;
  private serial = 0;
  private readonly ids = new WeakMap<object, string>();
  private readonly documents = new Map<vscode.TextDocument, BufferState>();
  private readonly subscriptions: vscode.Disposable[] = [];
  private readonly pending = new Map<vscode.TextDocument, {
    sequence: number; beforeVersion: number; version: number; at: number; editor: vscode.TextEditor; carets: number[]; changes: readonly InputChange[];
  }>();
  private readonly skipped = new Set<string>();
  // Keep the last view's receipt when focus/activation/context ends its timer.
  // A hidden tab can return with a new TextEditor object for the same document.
  private readonly views = new WeakMap<vscode.TextDocument, Exposure>();
  private exposure: Exposure | undefined;
  private stopped = false;
  private timer: NodeJS.Timeout | undefined;
  private readonly edits = new EditorEdits(event => { this.record(event); });

  constructor(private readonly folder: vscode.WorkspaceFolder, private readonly dwell: number,
    private readonly maxFileBytes: number, private readonly emit: (event: EditorEvent) => boolean,
    private readonly attribution?: HumanIdentity) {
    const version = vscode.extensions?.getExtension('ambientlight.editchain-history')?.packageJSON.version;
    const tabs = new EditorTabs(uri => this.relative(uri), tab => this.identity(tab), event => {
      this.pending.clear(); this.edits.flush(); this.record(event);
    }, folder.index === 0);
    this.record({ type: 'tracking_started', dwell_ms: dwell, vscode_version: vscode.version,
      ...(typeof version === 'string' ? { extension_version: version } : {}), activity_schema: attribution ? 3 : 2 });
    this.subscriptions.push(
      vscode.workspace.onDidOpenTextDocument(document => { this.baseline(document); }),
      vscode.workspace.onDidCloseTextDocument(document => {
        // VS Code also disposes background/internal documents while typing.
        // Their lifetime must not split the active file's edit or input receipt.
        this.edits.interrupt(document);
        if (this.exposure?.document.id === this.documents.get(document)?.document.id) this.endExposure();
        this.documents.delete(document); this.pending.delete(document);
      }),
      vscode.workspace.onDidChangeTextDocument(event => this.changed(event)),
      vscode.workspace.onDidSaveTextDocument(document => {
        this.edits.interrupt(document);
        this.pending.delete(document);
        const state = this.documents.get(document);
        if (state) this.record({ type: 'document_saved', document: state.document });
      }),
      vscode.workspace.onDidRenameFiles(event => {
        this.pending.clear();
        this.edits.flush();
        for (const file of event.files) {
          const from = this.relative(file.oldUri), to = this.relative(file.newUri);
          if (from !== null && to !== null) this.record({ type: 'document_renamed', from, to });
        }
      }),
      vscode.window.onDidChangeActiveTextEditor(editor => {
        this.edits.activate(editor);
        for (const [document, change] of this.pending) if (change.editor !== editor) this.pending.delete(document);
        if (!editor || (this.exposure && (this.exposure.editor !== this.identity(editor)
          || !this.sameView(this.exposure, editor)))) this.endExposure();
        const state = editor && this.baseline(editor.document);
        this.record({ type: 'editor_activated', document: state?.document ?? null });
        this.viewport();
      }),
      vscode.window.onDidChangeVisibleTextEditors(() => this.viewport()),
      vscode.window.onDidChangeTextEditorVisibleRanges(() => this.viewport()),
      vscode.window.onDidChangeTextEditorSelection(event => this.selection(event)),
      // Focus guards local state only; activity notifications need not lose focus.
      vscode.window.onDidChangeWindowState(() => {
        if (!vscode.window.state.focused) { this.pending.clear(); this.edits.flush(); }
        this.viewport();
      }),
      vscode.window.tabGroups.onDidChangeTabs(event => {
        if (event.closed.length) this.viewport();
        tabs.sync();
      }),
    );
    for (const document of vscode.workspace.textDocuments) this.baseline(document);
    tabs.sync(true);
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
      this.pending.clear();
      this.edits.flush();
      this.record({ type: 'tracking_gap', reason: `Buffer skipped (binary or over ${this.maxFileBytes} bytes): ${uri}` });
    }
    return false;
  }

  private changed(event: vscode.TextDocumentChangeEvent): void {
    if (!this.tracked(event.document) || !event.contentChanges.length) return;
    if (this.pending.has(event.document)) this.edits.interrupt(event.document);
    this.pending.delete(event.document);
    if (this.exposure?.document.id === this.documents.get(event.document)?.document.id) this.endExposure();
    const before = this.documents.get(event.document);
    this.edits.beforeChange(event.document, before?.document.version ?? -1, vscode.window.activeTextEditor);
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
    const origin = editorOrigin(event);
    if (reason || (origin && !isEditorInput(origin))) this.edits.interrupt(event.document);
    const sequence = this.record({ type: 'document_changed', document, before_version: before.document.version,
      before: before.text, after, reason, ...(origin ? { origin } : {}), changes: event.contentChanges.map(change => ({
        offset: change.rangeOffset, length: change.rangeLength, text: change.text,
      })) });
    this.documents.set(event.document, { document, text: after });
    if (sequence && vscode.window.state.focused && vscode.window.activeTextEditor?.document === event.document) {
      // A known non-input origin takes precedence over timing/selection hints.
      // Undo/redo of ordinary editor history reports applyEdits in this API.
      const signal = reason && (!origin || origin.source === 'applyEdits') ? reason
        : origin && isEditorInput(origin) ? 'editor_input'
        : !origin && this.edits.corrects(event.document, before.document.version, event.contentChanges) ? 'typing_correction' : undefined;
      if (signal) this.edits.add(event.document, vscode.window.activeTextEditor, before.document.version, document.version, sequence, signal, event.contentChanges);
      else if (!origin) {
        this.pending.set(event.document, { sequence, beforeVersion: before.document.version, version: document.version, at: performance.now(),
          editor: vscode.window.activeTextEditor, carets: editCarets(event.contentChanges), changes: event.contentChanges });
      }
    }
    this.viewport();
  }

  private selection(event: vscode.TextEditorSelectionChangeEvent): void {
    const change = this.pending.get(event.textEditor.document);
    // A selection update belongs to one revision. An unknown/command update
    // consumes it too, so later navigation cannot claim an automatic edit.
    this.pending.delete(event.textEditor.document);
    const state = this.baseline(event.textEditor.document);
    if (!state || !change) return;
    const keyboard = event.kind === vscode.TextEditorSelectionChangeKind.Keyboard;
    const carets = event.selections.filter(selection => selection.isEmpty)
      .map(selection => event.textEditor.document.offsetAt(selection.active)).sort((a, b) => a - b);
    if (keyboard && vscode.window.state.focused && event.textEditor === vscode.window.activeTextEditor
      && event.textEditor === change.editor && change.version === state.document.version
      && performance.now() - change.at <= 250 && carets.length === event.selections.length
      && JSON.stringify(carets) === JSON.stringify(change.carets)) {
      this.edits.add(event.textEditor.document, event.textEditor, change.beforeVersion, change.version, change.sequence,
        'keyboard_selection', change.changes);
    } else this.edits.interrupt(event.textEditor.document);
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
    const active = vscode.window.activeTextEditor;
    const group = active && this.edits.read(active.document, document.version);
    this.record({ type: 'code_read', document, editor, ranges, started_ms, duration_ms,
      ...(group ? { group } : {}) });
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

  checkpoint(): void { this.edits.flush(); this.publishRead(); }

  context(context: { observed_ms: number; workspace_path?: string; repositories: unknown[] }): void {
    this.pending.clear();
    this.edits.flush();
    this.endExposure();
    this.record({ type: 'workspace_context', ...context });
    this.beginExposure();
  }

  dispose(): void {
    this.edits.flush();
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

/** Endpoints after replaying replacements in their original UTF-16 order. */
function editCarets(changes: readonly vscode.TextDocumentContentChangeEvent[]): number[] {
  const carets: number[] = [];
  for (const change of changes) {
    const end = change.rangeOffset + change.rangeLength;
    for (let index = 0; index < carets.length; index++) {
      if (carets[index] >= end) carets[index] += change.text.length - change.rangeLength;
      else if (carets[index] >= change.rangeOffset) carets[index] = change.rangeOffset + change.text.length;
    }
    carets.push(change.rangeOffset + change.text.length);
  }
  return carets.sort((a, b) => a - b);
}
