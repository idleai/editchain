import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import type { TextEditorEdit } from 'vscode';

const output = process.env.EDITCHAIN_CAPTURE_OUTPUT!;
const workspace = path.join(process.env.EDITCHAIN_CAPTURE_FIXTURE!, 'workspace');
const outcomes: { test: string; state: string }[] = [];
let scenario = '';

async function probe(method: string, ...args: unknown[]): Promise<any> {
  return browser.executeWorkbench(async (vscode, input) => {
    const extension = vscode.extensions.getExtension('ambientlight.editchain-capture-probe');
    if (!extension) throw new Error('Capture probe extension was not loaded');
    const api = await extension.activate();
    return api[input.method](...input.args);
  }, { method, args });
}

async function show(name: string, preview = false): Promise<void> {
  await browser.executeWorkbench(async (vscode, input) => {
    const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, input.name);
    const document = await vscode.workspace.openTextDocument(uri);
    await vscode.window.showTextDocument(document, { preview: input.preview });
  }, { name, preview });
  await browser.waitUntil(async () => (await probe('snapshot')).activeEditor?.uri.endsWith(`/${name}`), {
    timeout: 10000, timeoutMsg: `Active editor did not settle on ${name}`,
  });
}

async function activeText(): Promise<string> {
  return browser.executeWorkbench(vscode => vscode.window.activeTextEditor?.document.getText());
}

async function events(type?: string): Promise<any[]> {
  return probe('getEvents', scenario, type);
}

async function waitForEvent(type: string, predicate = (_event: any) => true): Promise<void> {
  await browser.waitUntil(async () => (await events(type)).some(predicate), {
    timeout: 10000, timeoutMsg: `No matching ${type} event in ${scenario}`,
  });
}

async function wheelEditor(): Promise<void> {
  const editor = await browser.$('.editor-instance .monaco-editor.focused');
  await editor.moveTo();
  await browser.execute(() => {
    (window as any).__captureWheelInput = null;
    document.addEventListener('wheel', event => {
      (window as any).__captureWheelInput = {
        deltaY: event.deltaY, deltaX: event.deltaX, trusted: event.isTrusted,
        deltaMode: event.deltaMode, ctrl: event.ctrlKey, shift: event.shiftKey, alt: event.altKey, meta: event.metaKey,
        wheelDelta: (event as any).wheelDelta, wheelDeltaY: (event as any).wheelDeltaY,
        target: (event.target as HTMLElement).className,
      };
    }, { once: true, capture: true });
  });
  const position = await browser.execute(() => {
    const rect = document.querySelector('.editor-instance .monaco-editor.focused')!.getBoundingClientRect();
    return { x: Math.round(rect.x + rect.width / 2), y: Math.round(rect.y + rect.height / 2) };
  });
  // Chromium 114's direct wheel dispatch has deltaY but zero wheelDeltaY;
  // VS Code 1.85 normalizes using the legacy field and consequently sees zero.
  // A mouse scroll gesture supplies realistic wheel ticks on both versions.
  await browser.sendCommand('Input.synthesizeScrollGesture', {
    ...position, yDistance: -600, gestureSourceType: 'mouse', speed: 3000, preventFling: true,
  });
  await browser.waitUntil(() => browser.execute(() => Boolean((window as any).__captureWheelInput)), {
    timeout: 3000, timeoutMsg: 'WebDriver wheel input was not delivered to the workbench',
  });
  const input = await browser.execute(() => (window as any).__captureWheelInput);
  assert.equal(input.trusted, true);
  fs.appendFileSync(path.join(output, 'wheel-input.jsonl'), JSON.stringify({ scenario, ...input }) + '\n');
}

// Independent oracle: replay the raw VS Code replacements with JavaScript's
// UTF-16 string offsets, without sorting/coalescing them or using after.text.
// Comparing EVERY captured after-state catches incorrect batch order and EOL
// assumptions, including undo and state-only events.
function assertReplay(changes: any[]): void {
  assert.ok(changes.length > 0, 'Expected captured change events');
  for (const event of changes) {
    assert.ok(event.before, `Missing baseline for event ${event.seq}`);
    let text: string = event.before.text;
    for (const change of event.changes) {
      assert.ok(change.offset >= 0 && change.offset + change.length <= text.length);
      text = text.slice(0, change.offset) + change.text + text.slice(change.offset + change.length);
    }
    assert.equal(text, event.after.text, `Replay mismatch at event ${event.seq} (${event.label})`);
    assert.ok(event.after.version >= event.before.version);
    if (event.changes.length) assert.ok(event.after.version > event.before.version);
  }
}

describe('VS Code editor capture feasibility (disposable host)', () => {
  before(async () => {
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('notifications.clearAll');
      const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, 'startup.txt');
      const document = await vscode.workspace.openTextDocument(uri);
      const editor = await vscode.window.showTextDocument(document, { preview: false });
      await editor.edit((builder: TextEditorEdit) => builder.insert(new vscode.Position(0, 0), 'UNSAVED '));
    });
    await probe('start');
  });

  beforeEach(async function () {
    scenario = this.currentTest!.title;
    await probe('mark', scenario);
  });

  afterEach(async function () {
    outcomes.push({ test: this.currentTest!.title, state: this.currentTest!.state || 'unknown' });
    const trace = await probe('getTrace');
    fs.writeFileSync(path.join(output, 'events.json'), JSON.stringify(trace, null, 2));
    fs.writeFileSync(path.join(output, 'results.json'), JSON.stringify({ metadata: trace.metadata, outcomes }, null, 2));
    if (this.currentTest!.state === 'failed') {
      await browser.saveScreenshot(path.join(output, `failure-${outcomes.length}.png`));
    }
  });

  it('bootstraps an already-open dirty buffer without inventing earlier edits', async () => {
    const trace = await probe('getTrace');
    const baseline = trace.events.find((event: any) => event.type === 'baseline');
    const document = baseline.documents.find((value: any) => value.uri.endsWith('/startup.txt'));
    assert.equal(document.dirty, true);
    assert.ok(document.text.startsWith('UNSAVED '));
    assert.ok(baseline.editors.some((editor: any) => editor.documentId === document.id));
    assert.equal(trace.events.filter((event: any) => event.type === 'document_change').length, 0);
    const [major, minor] = trace.metadata.vscode.split('.').map(Number);
    assert.equal(trace.metadata.windowActiveSupported, major > 1 || minor >= 89);
    assert.equal(trace.metadata.vscode, process.env.EDITCHAIN_CAPTURE_VSCODE || '1.137.0');
  });

  it('observes a loaded document with no tab or visible editor', async () => {
    await browser.executeWorkbench(async vscode => {
      await vscode.workspace.openTextDocument(vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, 'hidden.txt'));
    });
    await waitForEvent('document_open', event => event.document.uri.endsWith('/hidden.txt'));
    const state = await probe('snapshot');
    assert.ok(state.documents.some((document: any) => document.uri.endsWith('/hidden.txt')));
    assert.ok(!state.editors.some((editor: any) => editor.uri.endsWith('/hidden.txt')));
    assert.ok(!state.groups.flatMap((group: any) => group.tabs).some((tab: any) => tab.uri?.endsWith('/hidden.txt')));
  });

  it('captures UI typing, deletion and clipboard paste as replayable edits', async () => {
    await show('keyboard.txt');
    await browser.keys('typed');
    await browser.keys('Backspace');
    await browser.executeWorkbench(async vscode => { await vscode.env.clipboard.writeText(' PASTED'); });
    await browser.keys(['Control', 'v']);
    await browser.waitUntil(async () => await activeText() === 'type PASTED', { timeout: 10000 });
    const changes = await events('document_change');
    assertReplay(changes);
    assert.ok(changes.some(event => event.changes.some((change: any) => change.length === 1 && change.text === '')));
    assert.ok(changes.some(event => event.changes.some((change: any) => change.text === ' PASTED')));
    assert.ok(changes.every(event => event.reason === null));
  });

  it('does not expose a stable authorship distinction for WorkspaceEdit and editor.edit', async () => {
    await browser.executeWorkbench(async vscode => {
      const editor = vscode.window.activeTextEditor;
      const edit = new vscode.WorkspaceEdit();
      edit.insert(editor.document.uri, new vscode.Position(0, 0), 'WORKSPACE ');
      if (!await vscode.workspace.applyEdit(edit)) throw new Error('WorkspaceEdit rejected');
      if (!await editor.edit((builder: TextEditorEdit) => builder.insert(new vscode.Position(0, 0), 'EXTENSION '))) {
        throw new Error('editor.edit rejected');
      }
    });
    const changes = (await events('document_change')).filter(event => event.changes.length);
    assert.equal(changes.length, 2);
    assertReplay(changes);
    assert.ok(changes.every(event => event.reason === null));
    const trace = await probe('getTrace');
    if (!trace.metadata.proposedReasonRequested) {
      const ui = trace.events.find((event: any) => event.type === 'document_change'
        && event.changes.some((change: any) => change.text === ' PASTED'));
      for (const change of changes) {
        assert.deepEqual(change.keys, ui.keys);
        assert.equal(change.detailedReason, null);
        assert.ok(!change.keys.includes('detailedReason'));
      }
    } else {
      assert.ok(changes.every(event => event.keys.includes('detailedReason')));
      assert.ok(changes.some(event => event.detailedReason?.source), 'Proposed reason should include a source');
    }
  });

  it('replays UTF-16, CRLF, multi-cursor, multi-edit, same-offset insertion and snippets', async () => {
    await show('unicode.txt');
    await browser.executeWorkbench(vscode => {
      vscode.window.activeTextEditor.selections = [new vscode.Selection(0, 3, 0, 3), new vscode.Selection(1, 1, 1, 1)];
    });
    await waitForEvent('selection', event => event.editor.selections.length === 2
      && event.editor.selections[0].start[1] === 3);
    await browser.keys('Q');
    await browser.waitUntil(async () => (await activeText()).startsWith('A😀QB\r\néQ中'), { timeout: 10000 });
    await browser.executeWorkbench(async vscode => {
      const editor = vscode.window.activeTextEditor;
      const edit = new vscode.WorkspaceEdit();
      edit.replace(editor.document.uri, new vscode.Range(0, 0, 0, 1), 'α');
      edit.replace(editor.document.uri, new vscode.Range(2, 0, 2, 4), '尾😀');
      if (!await vscode.workspace.applyEdit(edit)) throw new Error('Multi-edit rejected');
      if (!await editor.edit((builder: TextEditorEdit) => {
        builder.insert(new vscode.Position(0, 0), 'FIRST');
        builder.insert(new vscode.Position(0, 0), 'SECOND');
      })) throw new Error('Same-offset insertion rejected');
      editor.selection = new vscode.Selection(2, 0, 2, 0);
      await editor.insertSnippet(new vscode.SnippetString('${1:snippet}-$0'));
    });
    const changes = await events('document_change');
    assertReplay(changes);
    const multicursor = changes.find(event => event.changes.filter((change: any) => change.text === 'Q').length === 2);
    assert.ok(multicursor);
    assert.deepEqual(multicursor.changes.map((change: any) => change.offset), [7, 3]);
    // The position after A + emoji is 3 UTF-16 units, but 5 UTF-8 bytes.
    assert.equal(Buffer.byteLength(multicursor.before.text.slice(0, 3), 'utf8'), 5);
    assert.ok((await activeText()).startsWith('FIRSTSECONDα😀QB\r\n'));
  });

  it('captures programmatic type commands without treating keyboard metadata as proof of a human', async () => {
    await show('keyboard.txt');
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('type', { text: 'COMMAND ' });
    });
    await browser.waitUntil(async () => (await activeText()).endsWith('COMMAND '), { timeout: 10000 });
    const changes = (await events('document_change')).filter(event => event.changes.length);
    assertReplay(changes);
    assert.equal(changes.flatMap(event => event.changes.map((change: any) => change.text)).join(''), 'COMMAND ');
    assert.ok(changes.every(event => event.reason === null));
    const trace = await probe('getTrace');
    if (trace.metadata.proposedReasonRequested) {
      const ui = trace.events.find((event: any) => event.type === 'document_change'
        && event.changes.some((change: any) => change.text === 't'));
      assert.deepEqual(changes[0].detailedReason, ui.detailedReason,
        'A programmatic type command can use the same detailed reason as UI input');
    }
    // The next undo/EOL scenario continues the Unicode document.
    await show('unicode.txt');
  });

  it('preserves undo/redo reasons and replays an EOL conversion', async () => {
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('leaveSnippet');
      const editor = vscode.window.activeTextEditor;
      await editor.edit((builder: TextEditorEdit) => builder.insert(new vscode.Position(0, 0), 'UNDO-ME '));
    });
    const inserted = await activeText();
    await browser.keys(['Control', 'z']);
    await browser.waitUntil(async () => await activeText() !== inserted, { timeout: 10000 });
    await browser.keys(['Control', 'y']);
    await browser.waitUntil(async () => await activeText() === inserted, { timeout: 10000 });
    await browser.executeWorkbench(async vscode => {
      await vscode.window.activeTextEditor.edit((builder: TextEditorEdit) => builder.setEndOfLine(vscode.EndOfLine.LF));
    });
    const changes = await events('document_change');
    assert.ok(changes.some(event => event.reason === 1), 'Undo reason');
    assert.ok(changes.some(event => event.reason === 2), 'Redo reason');
    assertReplay(changes);
    assert.ok(!(await activeText()).includes('\r'));
    assert.ok(changes.some(event => event.before.eol === 2 && event.after.eol === 1));
  });

  it('separates save notifications and empty dirty-state events from text changes', async () => {
    await browser.executeWorkbench(async vscode => {
      if (!await vscode.window.activeTextEditor.document.save()) throw new Error('Save rejected');
    });
    await waitForEvent('did_save');
    const changes = await events('document_change');
    assertReplay(changes);
    assert.ok(changes.some(event => event.changes.length === 0 && event.before.dirty && !event.after.dirty));
    assert.ok(changes.every(event => event.before.text === event.after.text));
    assert.ok((await events('will_save')).some(event => event.reason === 1), 'API save reports Manual');
  });

  it('replaces preview tabs without treating every loaded model as visible', async () => {
    await show('preview-a.txt', true);
    const first = await probe('snapshot');
    const firstTab = first.groups.flatMap((group: any) => group.tabs).find((tab: any) => tab.uri?.endsWith('/preview-a.txt'));
    assert.equal(firstTab.preview, true);
    await show('preview-b.txt', true);
    await waitForEvent('tabs', event => event.closed.some((tab: any) => tab.id === firstTab.id));
    const state = await probe('snapshot');
    assert.ok(!state.editors.some((editor: any) => editor.uri.endsWith('/preview-a.txt')));
    assert.ok(state.groups.flatMap((group: any) => group.tabs).some((tab: any) => tab.uri?.endsWith('/preview-b.txt')));
  });

  it('assigns distinct editor/tab identities to two views of one document', async () => {
    await show('long.txt');
    await browser.executeWorkbench(async vscode => {
      await vscode.window.showTextDocument(vscode.window.activeTextEditor.document, {
        viewColumn: vscode.ViewColumn.Beside, preview: false,
      });
    });
    await browser.waitUntil(async () => (await probe('snapshot')).editors.filter((editor: any) => editor.uri.endsWith('/long.txt')).length === 2);
    const state = await probe('snapshot');
    const editors = state.editors.filter((editor: any) => editor.uri.endsWith('/long.txt'));
    assert.equal(new Set(editors.map((editor: any) => editor.id)).size, 2);
    assert.equal(new Set(editors.map((editor: any) => editor.documentId)).size, 1);
    const tabs = state.groups.flatMap((group: any) => group.tabs).filter((tab: any) => tab.uri?.endsWith('/long.txt'));
    assert.equal(tabs.length, 2);
    assert.notEqual(tabs[0].id, tabs[1].id);
  });

  it('observes programmatic reveals and jumps without inventing skipped line exposure', async () => {
    await browser.executeWorkbench(vscode => {
      const editor = vscode.window.activeTextEditor;
      editor.revealRange(new vscode.Range(800, 0, 800, 0), vscode.TextEditorRevealType.AtTop);
    });
    await waitForEvent('viewport', event => event.ranges.some((range: any) => range.start[0] >= 790));
    const viewport = await events('viewport');
    assert.ok(viewport.every(event => !event.keys.includes('kind') && !event.keys.includes('source')));
    const state = await probe('snapshot');
    const views = state.editors.filter((editor: any) => editor.uri.endsWith('/long.txt'));
    assert.ok(views.some((editor: any) => editor.ranges[0].start[0] === 0));
    assert.ok(views.some((editor: any) => editor.ranges[0].start[0] >= 790));
  });

  it('captures wheel scrolling and PageDown through the same viewport event surface', async () => {
    await wheelEditor();
    await waitForEvent('viewport');
    const count = (await events('viewport')).length;
    await browser.keys('PageDown');
    await browser.waitUntil(async () => (await events('viewport')).length > count, { timeout: 10000 });
    const viewport = await events('viewport');
    const trace = await probe('getTrace');
    const reveal = trace.events.find((event: any) => event.type === 'viewport' && event.label.startsWith('observes programmatic'));
    assert.ok(viewport.every(event => JSON.stringify(event.keys) === JSON.stringify(reveal.keys)));
  });

  it('reports viewport changes caused by layout and wrapping', async () => {
    // Electron does not expose Chrome's Browser.getWindowForTarget. Resize the
    // editor viewport through workbench layout instead of a CDP window command.
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.togglePanel');
    });
    await waitForEvent('viewport');
    const count = (await events('viewport')).length;
    await browser.executeWorkbench(async vscode => {
      await vscode.workspace.getConfiguration('editor').update('wordWrap', 'on', vscode.ConfigurationTarget.Global);
    });
    await browser.waitUntil(async () => (await events('viewport')).length > count, { timeout: 10000 });
    await browser.executeWorkbench(async vscode => {
      await vscode.workspace.getConfiguration('editor').update('wordWrap', 'off', vscode.ConfigurationTarget.Global);
      await vscode.commands.executeCommand('workbench.action.closePanel');
    });
  });

  it('preserves disjoint visible ranges across folded code', async () => {
    await show('fold.py');
    await browser.executeWorkbench(async vscode => {
      vscode.window.activeTextEditor.selection = new vscode.Selection(0, 0, 0, 0);
      await vscode.commands.executeCommand('editor.fold');
    });
    await waitForEvent('viewport', event => event.ranges.length > 1);
    const folded = (await events('viewport')).find(event => event.ranges.length > 1);
    assert.ok(folded.ranges[1].start[0] > folded.ranges[0].end[0] + 1);
    await browser.saveScreenshot(path.join(output, 'folded-code.png'));
  });

  it('keeps the active text editor when keyboard focus moves to the terminal', async () => {
    const before = await probe('snapshot');
    await browser.executeWorkbench(async vscode => {
      const terminal = vscode.window.createTerminal({ name: 'capture-focus-fixture' });
      terminal.show(false);
    });
    await browser.waitUntil(() => browser.execute(() => Boolean(document.activeElement?.closest('.terminal'))), { timeout: 10000 });
    const after = await probe('snapshot');
    assert.equal(after.activeEditor?.id, before.activeEditor?.id);
    assert.equal(after.window.focused, true);
    await browser.executeWorkbench(async vscode => {
      for (const terminal of vscode.window.terminals) if (terminal.name === 'capture-focus-fixture') terminal.dispose();
      await vscode.commands.executeCommand('workbench.action.closePanel');
    });
  });

  it('observes rename, disk reload and language-change document lifetimes', async () => {
    await show('rename.txt');
    const original = (await probe('snapshot')).activeEditor;
    await browser.executeWorkbench(async vscode => {
      const oldUri = vscode.window.activeTextEditor.document.uri;
      const newUri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, 'renamed.txt');
      const edit = new vscode.WorkspaceEdit();
      edit.renameFile(oldUri, newUri);
      if (!await vscode.workspace.applyEdit(edit)) throw new Error('Rename rejected');
    });
    await waitForEvent('files_renamed');
    assert.ok((await probe('snapshot')).activeEditor.uri.endsWith('/renamed.txt'));
    fs.writeFileSync(path.join(workspace, 'renamed.txt'), 'External change 😀\n');
    await browser.waitUntil(async () => await activeText() === 'External change 😀\n', { timeout: 15000 });
    const reloaded = (await events('document_change')).filter(event => event.after.uri.endsWith('/renamed.txt'));
    assertReplay(reloaded);
    assert.ok(reloaded.every(event => event.reason === null));
    const beforeLanguage = (await probe('snapshot')).activeEditor;
    await browser.executeWorkbench(async vscode => {
      await vscode.languages.setTextDocumentLanguage(vscode.window.activeTextEditor.document, 'markdown');
    });
    await waitForEvent('document_open', event => event.document.languageId === 'markdown');
    assert.ok((await events('document_close')).some(event => event.document.id === original.documentId));
    const languageClose = (await events('document_close')).find(event => event.document.id === beforeLanguage.documentId);
    const languageOpen = (await events('document_open')).find(event => event.document.languageId === 'markdown');
    assert.ok(languageClose);
    assert.equal(languageClose.document.closed, false, 'Language change does not dispose the API document');
    assert.equal(languageOpen.document.id, beforeLanguage.documentId, 'Language change reuses the API object');
  });

  it('captures untitled save-as as observed document transitions', async () => {
    await browser.executeWorkbench(async vscode => {
      const document = await vscode.workspace.openTextDocument({ content: 'Untitled buffer 😀\n', language: 'plaintext' });
      await vscode.window.showTextDocument(document, { preview: false });
    });
    await browser.waitUntil(async () => (await probe('snapshot')).activeEditor?.uri.startsWith('untitled:'), { timeout: 10000 });
    const untitled = (await probe('snapshot')).activeEditor;
    await browser.keys(['Control', 'Shift', 's']);
    // Older workbenches can keep multiple hidden QuickInput widgets in the DOM.
    const input = await browser.$('input:focus');
    await input.waitForDisplayed({ timeout: 10000 });
    await input.setValue(path.join(workspace, 'saved-as.txt'));
    await browser.keys('Enter');
    await browser.waitUntil(async () => (await probe('snapshot')).activeEditor?.uri.endsWith('/saved-as.txt'), { timeout: 10000 });
    assert.equal(fs.readFileSync(path.join(workspace, 'saved-as.txt'), 'utf8'), 'Untitled buffer 😀\n');
    const saved = (await probe('snapshot')).activeEditor;
    assert.notEqual(saved.documentId, untitled.documentId);
    assert.ok((await events('document_open')).some(event => event.document.uri.endsWith('/saved-as.txt')));
  });

  it('records formatter edits without assigning human authorship', async () => {
    await show('format.txt');
    await browser.executeWorkbench(async vscode => {
      await vscode.workspace.getConfiguration('editor').update('defaultFormatter',
        'ambientlight.editchain-capture-probe', vscode.ConfigurationTarget.Global);
      await vscode.commands.executeCommand('editor.action.formatDocument');
    });
    await browser.waitUntil(async () => await activeText() === 'formatted = true\n', { timeout: 10000 });
    const changes = await events('document_change');
    assertReplay(changes);
    assert.ok(changes.every(event => event.reason === null));
  });

  it('records acceptance of a deterministic inline completion', async () => {
    // This tests the completion mechanism, not Copilot or any real AI model.
    await show('completion.txt');
    await browser.keys('End');
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('editor.action.inlineSuggest.trigger');
    });
    await browser.waitUntil(() => browser.execute(() => document.querySelectorAll('.ghost-text, .ghost-text-decoration').length > 0), {
      timeout: 10000, timeoutMsg: 'Inline completion never became visible',
    });
    await browser.keys('Tab');
    await browser.waitUntil(async () => await activeText() === 'const answer = 42;', { timeout: 10000 });
    const changes = await events('document_change');
    assertReplay(changes);
    assert.ok(changes.every(event => event.reason === null));
    if ((await probe('getTrace')).metadata.proposedReasonRequested) {
      assert.ok(changes.some(event => event.detailedReason?.source === 'inlineCompletionAccept'
        && event.detailedReason.metadata.$extensionId === 'ambientlight.editchain-capture-probe'));
    }
  });

  it('reports delayed autosave separately from the edit that caused it', async () => {
    await show('autosave.txt');
    await browser.executeWorkbench(async vscode => {
      await vscode.workspace.getConfiguration('files').update('autoSaveDelay', 600, vscode.ConfigurationTarget.Global);
      await vscode.workspace.getConfiguration('files').update('autoSave', 'afterDelay', vscode.ConfigurationTarget.Global);
    });
    await browser.keys('x');
    await waitForEvent('did_save', event => event.document.uri.endsWith('/autosave.txt'));
    assert.ok((await events('will_save')).some(event => event.reason === 2 && event.document.uri.endsWith('/autosave.txt')));
    assertReplay(await events('document_change'));
    await browser.executeWorkbench(async vscode => {
      await vscode.workspace.getConfiguration('files').update('autoSave', 'off', vscode.ConfigurationTarget.Global);
    });
  });

  it('tracks tab close/reopen and movement independently of document objects', async () => {
    await show('lifecycle.txt');
    const initial = await probe('snapshot');
    const firstTab = initial.groups.flatMap((group: any) => group.tabs).find((tab: any) => tab.uri?.endsWith('/lifecycle.txt'));
    await browser.executeWorkbench(async vscode => {
      await vscode.window.tabGroups.close(vscode.window.tabGroups.activeTabGroup.activeTab);
    });
    await waitForEvent('tabs', event => event.closed.some((tab: any) => tab.id === firstTab.id));
    await show('lifecycle.txt');
    const reopened = await probe('snapshot');
    const secondTab = reopened.groups.flatMap((group: any) => group.tabs).find((tab: any) => tab.uri?.endsWith('/lifecycle.txt'));
    assert.notEqual(secondTab.id, firstTab.id);
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.moveEditorToPreviousGroup');
    });
    await browser.waitUntil(async () => (await probe('snapshot')).activeEditor.column !== reopened.activeEditor.column, { timeout: 10000 });
    const moved = await probe('snapshot');
    assert.equal(moved.activeEditor.documentId, reopened.activeEditor.documentId);
  });

  it('keeps wheel-only activity distinct from the coarse WindowState.active flag', async () => {
    const metadata = (await probe('getTrace')).metadata;
    if (!metadata.windowActiveSupported) {
      assert.equal((await probe('snapshot')).window.active, null, '1.85 must expose an unavailable capability explicitly');
      return;
    }
    await show('long.txt');
    await browser.executeWorkbench(vscode => {
      vscode.window.activeTextEditor.revealRange(new vscode.Range(100, 0, 100, 0), vscode.TextEditorRevealType.AtTop);
    });
    await browser.keys('ArrowRight');
    await browser.waitUntil(async () => (await probe('snapshot')).window.active === true, { timeout: 10000 });
    // Reading can continue while this timer expires. API polling is not input.
    await browser.waitUntil(async () => (await probe('snapshot')).window.active === false, { timeout: 95000, interval: 1000 });
    const previousCount = (await events('viewport')).length;
    await wheelEditor();
    await browser.waitUntil(async () => (await events('viewport')).length > previousCount, { timeout: 10000 });
    const afterWheel = await probe('snapshot');
    assert.equal(afterWheel.window.active, false, 'Wheel-only scrolling does not reset the DOM activity tracker');
    assert.equal(afterWheel.window.focused, true);
    await browser.keys('ArrowRight');
    await browser.waitUntil(async () => (await probe('snapshot')).window.active === true, { timeout: 10000 });
  });

  it('replays every recorded change and preserves ordered immutable observations', async () => {
    const trace = await probe('getTrace');
    assertReplay(trace.events.filter((event: any) => event.type === 'document_change'));
    for (let index = 1; index < trace.events.length; index++) {
      assert.equal(trace.events[index].seq, trace.events[index - 1].seq + 1);
      assert.ok(trace.events[index].elapsedMs >= trace.events[index - 1].elapsedMs);
    }
    assert.ok(trace.events.some((event: any) => event.type === 'selection' && event.kind === 1));
  });
});
