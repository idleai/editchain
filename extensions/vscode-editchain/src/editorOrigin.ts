import type * as vscode from 'vscode';

export type EditorOrigin = {
  source: string; kind?: string; detailed_source?: string; name?: string; extension_id?: string;
};

// Optional proposed textDocumentChangeReason API. Stable VS Code events omit
// the property entirely; enabled hosts include it even when the reason is absent.
type EventWithOrigin = vscode.TextDocumentChangeEvent & { detailedReason?: unknown };

export function editorOrigin(event: vscode.TextDocumentChangeEvent): EditorOrigin | undefined {
  if (!('detailedReason' in event)) return undefined;
  const reason = object((event as EventWithOrigin).detailedReason);
  const metadata = object(reason.metadata);
  return { source: label(reason.source) ?? 'unknown', kind: label(metadata.kind),
    detailed_source: label(metadata.detailedSource), name: label(metadata.name),
    extension_id: label(metadata.$extensionId) };
}

export function isEditorInput(origin: EditorOrigin): boolean {
  // Cooperative attribution of editor input, not proof of physical keystrokes.
  // Unknown, provider, formatting and disk sources never use cursor fallback.
  return origin.source === 'cursor' && ['type', 'paste', 'cut', 'compositionType',
    'compositionEnd', 'executeCommand', 'executeCommands'].includes(origin.kind ?? '');
}

function object(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' ? value as Record<string, unknown> : {};
}

function label(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 && Buffer.byteLength(value) <= 512 ? value : undefined;
}
