import * as vscode from 'vscode';
import type { EditorEvent } from './editorOutbox';

/** A file is open while at least one text tab owns it, including split editors. */
export class EditorTabs {
  private files = new Map<string, { editor: string; uri: string; path: string | null }>();

  constructor(private readonly relative: (uri: vscode.Uri) => string | null,
    private readonly identity: (tab: vscode.Tab) => string,
    private readonly emit: (event: EditorEvent['event']) => void,
    private readonly untitled: boolean) {}

  sync(restored = false): void {
    const next = new Map<string, { editor: string; uri: string; path: string | null }>();
    for (const group of vscode.window.tabGroups.all) for (const tab of group.tabs) {
      if (!(tab.input instanceof vscode.TabInputText)) continue;
      const uri = tab.input.uri;
      const path = this.relative(uri);
      if (path === null && !(uri.scheme === 'untitled' && this.untitled)) continue;
      const key = uri.toString();
      if (!next.has(key)) next.set(key, this.files.get(key) ?? { editor: this.identity(tab), uri: key, path });
    }
    for (const [uri, tab] of this.files) if (!next.has(uri)) this.emit({ type: 'editor_closed', ...tab });
    for (const [uri, tab] of next) if (!this.files.has(uri)) {
      this.emit({ type: 'editor_opened', ...tab, ...(restored ? { restored: true } : {}) });
    }
    this.files = next;
  }
}
