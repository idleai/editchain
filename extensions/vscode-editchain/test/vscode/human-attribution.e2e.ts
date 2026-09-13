import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import type { TextEditorEdit, TextDocument, Position } from 'vscode';

const proposed = process.env.EDITCHAIN_CAPTURE_PROPOSED === '1';
const observations: unknown[] = [];

async function fixture(name: string, text = 'baseline'): Promise<void> {
  await browser.executeWorkbench(async (vscode, name, text) => {
    const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, `attribution-${name}.txt`);
    await vscode.workspace.fs.writeFile(uri, new TextEncoder().encode(text));
    await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uri), { preview: false });
  }, name, text);
  await caret(text.length);
}

async function caret(column: number, anchor = column): Promise<void> {
  await browser.executeWorkbench((vscode, column, anchor) => {
    vscode.window.activeTextEditor.selection = new vscode.Selection(0, anchor, 0, column);
  }, column, anchor);
  await browser.waitUntil(async () => browser.execute(expected => Array.from(document.querySelectorAll('.statusbar-item'))
    .some(item => item.textContent.includes(expected)), `Ln 1, Col ${column + 1}`), { timeout: 10000 });
}

async function evidence(name: string): Promise<{ changes: any[]; human: any[]; saves: any[]; edits: any[] }> {
  const report = await browser.executeWorkbench(async vscode => vscode.commands.executeCommand('editchain-history.humanWork'));
  assert.ok(report, 'production coverage query durably flushed capture');
  const root = path.join(process.env.EDITCHAIN_WORK_FIXTURE!, 'workspace', '.editchain', 'blobs');
  const events = fs.readdirSync(root).filter(name => /^[a-f0-9]{64}$/.test(name)).flatMap(name => {
    let value;
    try { value = JSON.parse(fs.readFileSync(path.join(root, name), 'utf8')); } catch { return []; }
    return value?.source === 'vscode.editor' ? [value.event] : [];
  });
  const changes = events.filter(value => value.event.type === 'document_changed'
    && value.event.document.path === `attribution-${name}.txt`).sort((a, b) => a.sequence - b.sequence);
  const keys = new Set(changes.map(value => `${value.session}:${value.sequence}`));
  const edits = events.filter(value => (value.event.type === 'human_edit' && keys.has(`${value.session}:${value.event.change}`))
    || (value.event.type === 'human_edit_batch' && value.event.edits.some((edit: any) => keys.has(`${value.session}:${edit.change}`))));
  const human = edits.flatMap(value => value.event.type === 'human_edit' ? [value]
    : value.event.edits.map((edit: any) => ({ ...value, event: { type: 'human_edit', ...edit } })));
  const saves = events.filter(value => value.event.type === 'document_saved' && value.event.document.path === `attribution-${name}.txt`);
  const result = { changes, human, saves, edits };
  observations.push({ name, proposed, ...result });
  return result;
}

describe('production edit attribution', () => {
  before(async () => {
    await browser.waitUntil(async () => browser.executeWorkbench(vscode =>
      vscode.extensions.getExtension('ambientlight.editchain-history')?.isActive), { timeout: 30000 });
  });

  after(() => fs.writeFileSync(path.join(process.env.EDITCHAIN_WORK_OUTPUT!, 'edit-attribution.json'), JSON.stringify(observations, null, 2)));

  it('attributes real typing once, before and after saving', async () => {
    await fixture('typing');
    await browser.keys('h');
    const before = await evidence('typing');
    assert.equal(before.changes.length, 1);
    assert.equal(before.human.length, 1);
    assert.equal(before.changes[0].event.origin?.source, proposed ? 'cursor' : undefined);
    await browser.executeWorkbench(async vscode => {
      const document = await vscode.workspace.openTextDocument(vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, 'attribution-typing.txt'));
      await document.save();
    });
    const after = await evidence('typing');
    assert.deepEqual(after.human, before.human);
    assert.equal(after.saves.length, 1);
  });

  it('groups consecutive real keystrokes into one saved edit', async () => {
    await fixture('burst');
    await browser.keys('humanwork');
    await browser.keys(['Control', 's']);
    const result = await evidence('burst');
    assert.equal(result.changes.length, 9, 'real key events retain all nine buffer revisions');
    assert.equal(result.human.length, 9);
    assert.equal(result.edits.length, 1, 'one graph activity instead of nine keystroke edits');
    assert.equal(result.edits[0].event.type, 'human_edit_batch');
    assert.deepEqual(result.edits[0].event.edits.map((edit: any) => edit.change), result.changes.map(value => value.sequence));
    assert.equal(result.changes[0].event.before, 'baseline');
    assert.equal(result.changes.at(-1).event.after, 'baselinehumanwork');
    const again = await evidence('burst');
    assert.deepEqual(again.edits, result.edits, 'save and report do not duplicate the burst');
  });

  it('splits typing bursts around a real agent-style WorkspaceEdit', async () => {
    await fixture('burst-agent');
    await browser.keys('hi');
    await browser.executeWorkbench(async vscode => {
      const editor = vscode.window.activeTextEditor;
      const edit = new vscode.WorkspaceEdit();
      edit.insert(editor.document.uri, new vscode.Position(0, 0), 'AGENT ');
      await vscode.workspace.applyEdit(edit);
    });
    await browser.keys('ok');
    await browser.keys(['Control', 's']);
    const result = await evidence('burst-agent');
    assert.equal(result.changes.length, 5);
    assert.equal(result.human.length, 4);
    assert.equal(result.edits.length, 2);
    assert.deepEqual(result.edits.sort((a, b) => a.sequence - b.sequence).map(value => value.event.edits.map((edit: any) => edit.change)),
      [[result.changes[0].sequence, result.changes[1].sequence], [result.changes[3].sequence, result.changes[4].sequence]]);
    assert.ok(result.changes[3].event.before.startsWith('AGENT '), 'second human burst starts after the agent revision');
  });

  for (const key of ['Backspace', 'Delete', 'Enter', 'Tab']) {
    it(`retains ${key} and uses its explicit editor origin when available`, async () => {
      await fixture(key);
      await caret(4);
      await browser.keys(key);
      const result = await evidence(key);
      assert.equal(result.changes.length, 1, 'key changed the actual editor buffer');
      if (proposed) {
        assert.equal(result.changes[0].event.origin.source, 'cursor');
        assert.equal(result.human.length, 1);
        assert.equal(result.human[0].event.signal, 'editor_input');
      } else if (key === 'Backspace' || key === 'Delete') {
        assert.equal(result.human.length, 0, 'stable API leaves the ambiguous deletion unattributed');
      }
    });
  }

  it('attributes clipboard paste and selected-text deletion to their own revisions', async () => {
    await fixture('clipboard');
    await browser.executeWorkbench(async vscode => vscode.env.clipboard.writeText(' PASTED'));
    await browser.keys(['Control', 'v']);
    await caret(8, 0);
    await browser.keys('Backspace');
    const result = await evidence('clipboard');
    assert.equal(result.changes.length, 2);
    assert.equal(result.human.length, proposed ? 2 : 1);
  });

  it('keeps WorkspaceEdit and editor.edit at the cursor unattributed despite navigation and save', async () => {
    await fixture('programmatic');
    await browser.executeWorkbench(async vscode => {
      const editor = vscode.window.activeTextEditor;
      const edit = new vscode.WorkspaceEdit();
      edit.insert(editor.document.uri, editor.selection.active, ' AGENT');
      await vscode.workspace.applyEdit(edit);
      await editor.edit((builder: TextEditorEdit) => builder.insert(editor.selection.active, ' TOOL'));
    });
    await browser.keys('ArrowLeft');
    await browser.keys(['Control', 's']);
    const result = await evidence('programmatic');
    assert.equal(result.changes.length, 2);
    assert.equal(result.human.length, 0);
    assert.equal(result.saves.length, 1);
    if (proposed) assert.ok(result.changes.every(value => value.event.origin.source === 'unknown'));
  });

  it('separates interleaved editor input and programmatic changes in the same buffer', async () => {
    await fixture('interleaved');
    await browser.keys('h');
    await browser.executeWorkbench(async vscode => {
      const editor = vscode.window.activeTextEditor;
      await editor.edit((builder: TextEditorEdit) => builder.insert(editor.selection.active, ' AGENT'));
    });
    await browser.keys('i');
    const result = await evidence('interleaved');
    assert.equal(result.changes.length, 3);
    assert.deepEqual(result.human.map(value => value.event.change).sort((a, b) => a - b),
      [result.changes[0].sequence, result.changes[2].sequence]);
    assert.equal(result.changes[2].event.before, result.changes[1].event.after, 'human edit retains the intervening agent state');
  });

  it('leaves external disk reloads unattributed', async () => {
    await fixture('disk');
    fs.writeFileSync(path.join(process.env.EDITCHAIN_WORK_FIXTURE!, 'workspace', 'attribution-disk.txt'), 'external');
    await browser.waitUntil(async () => browser.executeWorkbench(vscode => vscode.window.activeTextEditor.document.getText() === 'external'), { timeout: 10000 });
    await browser.keys('ArrowLeft');
    const result = await evidence('disk');
    assert.equal(result.changes.length, 1);
    assert.equal(result.human.length, 0);
    if (proposed) assert.equal(result.changes[0].event.origin.source, 'reloadFromDisk');
  });

  it('keeps formatter changes on save separate from the human input being saved', async () => {
    await fixture('format');
    await browser.keys('h');
    await browser.executeWorkbench(async vscode => {
      const provider = vscode.languages.registerDocumentFormattingEditProvider({ pattern: '**/attribution-format.txt' }, {
        provideDocumentFormattingEdits() { return [vscode.TextEdit.insert(new vscode.Position(0, 0), 'FORMATTED ')]; },
      });
      const settings = vscode.workspace.getConfiguration('editor');
      try {
        await settings.update('formatOnSave', true, vscode.ConfigurationTarget.Global);
        await settings.update('formatOnSaveMode', 'file', vscode.ConfigurationTarget.Global);
        await vscode.window.activeTextEditor.document.save();
      } finally {
        provider.dispose();
        await settings.update('formatOnSave', false, vscode.ConfigurationTarget.Global);
      }
    });
    const result = await evidence('format');
    assert.equal(result.changes.length, 2, 'save executed the real formatting provider');
    assert.equal(result.human.length, 1);
    assert.equal(result.human[0].event.change, result.changes[0].sequence);
    assert.equal(result.saves.length, 1);
    if (proposed) assert.equal(result.changes[1].event.origin.name, 'formatEditsCommand');
  });

  it('does not count accepted provider completions as human-written code', async () => {
    await fixture('completion', 'const answer = ');
    await browser.executeWorkbench(async vscode => {
      (globalThis as any).__attributionCompletion = vscode.languages.registerInlineCompletionItemProvider({ pattern: '**/attribution-completion.txt' }, {
        provideInlineCompletionItems(_document: TextDocument, position: Position) {
          return [new vscode.InlineCompletionItem('42;', new vscode.Range(position, position))];
        },
      });
      await vscode.commands.executeCommand('editor.action.inlineSuggest.trigger');
    });
    try {
      await browser.waitUntil(() => browser.execute(() => document.querySelectorAll('.ghost-text, .ghost-text-decoration').length > 0), { timeout: 10000 });
      await browser.keys('Tab');
      await browser.keys('ArrowLeft');
      const result = await evidence('completion');
      assert.equal(result.changes.length, 1);
      assert.equal(result.changes[0].event.after, 'const answer = 42;');
      assert.equal(result.human.length, 0);
      if (proposed) assert.equal(result.changes[0].event.origin.source, 'inlineCompletionAccept');
    } finally {
      await browser.executeWorkbench(() => { (globalThis as any).__attributionCompletion.dispose(); delete (globalThis as any).__attributionCompletion; });
    }
  });

  it('retains undo and redo as separate human operations', async () => {
    await fixture('undo');
    await browser.keys('h');
    await browser.keys(['Control', 'z']);
    await browser.keys(['Control', 'y']);
    const result = await evidence('undo');
    assert.equal(result.changes.length, 3);
    assert.equal(result.human.length, 3);
    assert.deepEqual(result.human.map(value => value.event.signal).sort(), [proposed ? 'editor_input' : 'keyboard_selection', 'redo', 'undo'].sort());
  });
});
