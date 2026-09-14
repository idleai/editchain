import * as vscode from 'vscode';
import { performance } from 'node:perf_hooks';
import { setTimeout, clearTimeout } from 'node:timers';
import { EditorEdits } from './editorEdits';
import { isEditorInput, type EditorOrigin } from './editorOrigin';
import type { EditorEvent } from './editorOutbox';
import type { InputChange } from './editorInput';

type Candidate = { sequence: number; before: number; version: number; at: number;
  editor: vscode.TextEditor; changes: readonly InputChange[]; timer: NodeJS.Timeout };

/** Attribute direct document events; keep unsupported changes visible and unclaimed. */
export class EditorAttribution {
  private readonly human: EditorEdits;
  private readonly observed: EditorEdits;
  private readonly pending = new Map<vscode.TextDocument, Candidate>();

  constructor(emit: (event: EditorEvent['event']) => void) {
    this.human = new EditorEdits(emit);
    this.observed = new EditorEdits(emit, false);
  }

  beforeChange(document: vscode.TextDocument, version: number, editor: vscode.TextEditor | undefined): void {
    this.settle(document);
    this.human.beforeChange(document, version, editor);
    this.observed.beforeChange(document, version, editor?.document === document ? editor : document);
  }

  observe(event: vscode.TextDocumentChangeEvent, before: number, sequence: number,
    reason: string | null, origin: EditorOrigin | undefined): void {
    const document = event.document, editor = vscode.window.activeTextEditor;
    const active = editor?.document === document && vscode.window.state.focused;
    // Direct input reasons require no later selection event or timing correlation.
    const signal = active ? reason && (!origin || origin.source === 'applyEdits') ? reason
      : origin && isEditorInput(origin) ? 'editor_input'
      : !origin && this.human.corrects(document, before, event.contentChanges) ? 'typing_correction' : undefined : undefined;
    if (signal && editor) {
      this.observed.interrupt(document);
      this.human.add(document, editor, before, document.version, sequence, signal, event.contentChanges);
    } else if (active && editor && !origin) {
      const timer = setTimeout(() => this.settle(document), 250);
      timer.unref();
      this.pending.set(document, { sequence, before, version: document.version, at: performance.now(),
        editor, changes: event.contentChanges, timer });
    } else {
      this.unattributed(document, editor?.document === document ? editor : document,
        before, document.version, sequence, event.contentChanges);
    }
  }

  selection(event: vscode.TextEditorSelectionChangeEvent): void {
    const document = event.textEditor.document, change = this.take(document);
    if (!change) return;
    const carets = event.selections.filter(selection => selection.isEmpty)
      .map(selection => document.offsetAt(selection.active)).sort((a, b) => a - b);
    if (event.kind === vscode.TextEditorSelectionChangeKind.Keyboard && vscode.window.state.focused
      && event.textEditor === vscode.window.activeTextEditor && event.textEditor === change.editor
      && change.version === document.version && performance.now() - change.at <= 250
      && carets.length === event.selections.length && JSON.stringify(carets) === JSON.stringify(editCarets(change.changes))) {
      this.observed.interrupt(document);
      this.human.add(document, change.editor, change.before, change.version, change.sequence, 'keyboard_selection', change.changes);
    } else this.unattributed(document, change.editor, change.before, change.version, change.sequence, change.changes);
  }

  activate(editor: vscode.TextEditor | undefined): void {
    for (const [document, change] of this.pending) if (change.editor !== editor) this.settle(document);
    this.human.activate(editor); this.observed.activate(editor);
  }

  interrupt(document: vscode.TextDocument): void {
    this.settle(document);
    this.human.interrupt(document); this.observed.interrupt(document);
  }

  read(document: vscode.TextDocument, version: number): number | undefined {
    return this.human.read(document, version);
  }

  flush(): void {
    for (const document of this.pending.keys()) this.settle(document);
    this.human.flush(); this.observed.flush();
  }

  private take(document: vscode.TextDocument): Candidate | undefined {
    const change = this.pending.get(document);
    if (change) { clearTimeout(change.timer); this.pending.delete(document); }
    return change;
  }

  private settle(document: vscode.TextDocument): void {
    const change = this.take(document);
    if (change) this.unattributed(document, change.editor, change.before, change.version, change.sequence, change.changes);
  }

  private unattributed(document: vscode.TextDocument, editor: object, before: number, version: number,
    sequence: number, changes: readonly InputChange[]): void {
    this.human.interrupt(document);
    this.observed.add(document, editor, before, version, sequence, 'observed', changes);
  }
}

/** Endpoints after replaying replacements in their original UTF-16 order. */
function editCarets(changes: readonly InputChange[]): number[] {
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
