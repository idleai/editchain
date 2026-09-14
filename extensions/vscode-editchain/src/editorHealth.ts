import type { EditorEvent } from './editorOutbox';

/** Runtime evidence, read without flushing capture or opening a report. */
export class EditorHealth {
  mode: 'unverified' | 'direct' | 'limited' = 'unverified';
  changes = 0;
  humanChanges = 0;
  observedChanges = 0;

  observe({ event }: EditorEvent): void {
    if (event.type === 'document_changed') {
      this.mode = event.origin ? 'direct' : 'limited';
      this.changes++;
    } else if (event.type === 'human_edit') this.humanChanges++;
    else if (event.type === 'human_edit_batch' && Array.isArray(event.edits)) this.humanChanges += event.edits.length;
    else if (event.type === 'observed_edit_batch' && Array.isArray(event.changes)) this.observedChanges += event.changes.length;
  }
}
