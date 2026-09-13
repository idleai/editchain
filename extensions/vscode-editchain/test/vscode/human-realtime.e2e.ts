import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import type { Tab, TabGroup, TabInputText } from 'vscode';

const output = process.env.EDITCHAIN_WORK_OUTPUT!;
const workspace = path.join(process.env.EDITCHAIN_WORK_FIXTURE!, 'workspace');
const measurements: Record<string, unknown> = {};
let webview: Awaited<ReturnType<Awaited<ReturnType<typeof browser.getWorkbench>>['getWebviewByTitle']>>;

// Passive inspection only: never invoke the coverage command or a capture flush.
function retained(): any[] {
  const blobs = path.join(workspace, '.editchain', 'blobs');
  return fs.readdirSync(blobs).filter(name => /^[a-f0-9]{64}$/.test(name)).flatMap(name => {
    const file = path.join(blobs, name);
    let source;
    try { source = JSON.parse(fs.readFileSync(file, 'utf8')); } catch { return []; }
    return source?.source === 'vscode.editor' ? [{ ...source.event, persisted_ms: fs.statSync(file).mtimeMs }] : [];
  });
}

async function show(name: string, column = 1): Promise<void> {
  await browser.executeWorkbench(async (vscode, name, column) => {
    const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, name);
    const document = await vscode.workspace.openTextDocument(uri);
    await vscode.window.showTextDocument(document, { viewColumn: column, preview: false });
  }, name, column);
}

async function rows(): Promise<any[]> {
  return await browser.execute(() => Array.from(document.querySelectorAll('#rows .row[data-row]'))
    .map(row => (window as any).__editchainRowAt?.(Number(row.getAttribute('data-row')))).filter(Boolean)) as unknown as any[];
}

describe('realtime human work without forced flushing', () => {
  before(async () => {
    await browser.waitUntil(async () => browser.executeWorkbench(vscode =>
      vscode.extensions.getExtension('ambientlight.editchain-history')?.isActive), { timeout: 30000 });
    for (const name of ['realtime-edit.ts', 'realtime-tabs.ts', 'realtime-reload.ts']) {
      fs.writeFileSync(path.join(workspace, name), 'export const value = 1; // \n');
    }
    // Keep a file in the left group so moving History cannot remove that group.
    await show('realtime-edit.ts');
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('editchain-history.open');
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
    });
    webview = await (await browser.getWorkbench()).getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.waitUntil(async () => (await rows()).length > 0, { timeout: 30000 });
    await webview.close();
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('vscode.setEditorLayout', { orientation: 0, groups: [{}, {}] });
      await vscode.commands.executeCommand('workbench.action.moveEditorToNextGroup');
    });
    await browser.waitUntil(async () => browser.executeWorkbench(vscode => vscode.window.tabGroups.all
      .some((group: TabGroup) => group.viewColumn === 2 && group.tabs.some((tab: Tab) => tab.label === 'EditChain History'))), { timeout: 10000 });
    await show('realtime-edit.ts');
    assert.ok(await browser.executeWorkbench(vscode => vscode.window.tabGroups.all.length >= 2), 'History stays visible beside the code editor');
    await browser.executeWorkbench(vscode => {
      const editor = vscode.window.activeTextEditor;
      const end = editor.document.lineAt(0).range.end;
      editor.selection = new vscode.Selection(end, end);
    });
    await browser.waitUntil(async () => browser.execute(() => Array.from(document.querySelectorAll('.statusbar-item'))
      .some(item => item.textContent.includes('Ln 1, Col 28'))), { timeout: 10000 });
  });

  after(() => fs.writeFileSync(path.join(output, 'human-realtime.json'), JSON.stringify(measurements, null, 2)));

  it('shows the first edit within one second and updates the same row through corrections and save', async () => {
    const started = Date.now();
    await browser.keys('h');
    await webview.open();
    assert.equal(await browser.execute(() => document.visibilityState), 'visible');
    let first: any;
    await browser.waitUntil(async () => {
      first = (await rows()).find(row => row.file_change?.path === 'realtime-edit.ts');
      return !!first;
    }, { timeout: 1000, interval: 25, timeoutMsg: 'first edit was not visible while still unsaved' });
    measurements.first_row_ms = Date.now() - started;
    assert.ok(Number(measurements.first_row_ms) < 1000);
    assert.equal(first.sub_ops.length, 0);
    await webview.close();
    for (const key of ['u', 'm', 'Backspace', 'm', 'a', 'n']) {
      await browser.keys(key);
      await browser.pause(100);
    }
    await webview.open();
    let edited: any;
    await browser.waitUntil(async () => {
      edited = (await rows()).find(row => row.continuity_key === first.continuity_key);
      return edited?.file_change?.op_id !== first.file_change.op_id;
    }, { timeout: 1000, interval: 25 });
    assert.equal((await rows()).filter(row => row.file_change?.path === 'realtime-edit.ts').length, 1);
    await webview.close();
    const savedAt = Date.now();
    await browser.keys(['Control', 's']);
    let saved: any;
    await browser.waitUntil(() => {
      saved = retained().find(value => value.event.type === 'document_saved' && value.event.document.path === 'realtime-edit.ts');
      return !!saved;
    }, { timeout: 1000, interval: 25, timeoutMsg: 'save did not reach durable capture within one second' });
    measurements.save_observed_ms = Date.now() - savedAt;
    measurements.save_persist_ms = saved.persisted_ms - saved.time_ms;
    const events = retained();
    const changes = events.filter(value => value.event.type === 'document_changed' && value.event.document.path === 'realtime-edit.ts');
    const keys = new Set(changes.map(value => `${value.session}:${value.sequence}`));
    const receipts = events.filter(value => value.event.type === 'human_edit_batch'
      && value.event.edits.some((edit: any) => keys.has(`${value.session}:${edit.change}`)));
    assert.equal(changes.length, 7);
    assert.equal(receipts.flatMap(value => value.event.edits).length, 7, 'correction is captured exactly once');
    assert.equal(new Set(receipts.map(value => value.event.group)).size, 1, 'correction and save preserve the edit group');
    assert.ok(fs.readFileSync(path.join(workspace, 'realtime-edit.ts'), 'utf8').includes('// human'));
    await webview.open();
    await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
    measurements.rows = await rows();
    await browser.saveScreenshot(path.join(output, 'human-realtime.png'));
    await webview.close();
  });

  it('records a split file once and closes it only after the last tab closes', async () => {
    await show('realtime-tabs.ts');
    await show('realtime-tabs.ts', 3);
    await browser.executeWorkbench(async vscode => {
      const tabs = vscode.window.tabGroups.all.flatMap((group: TabGroup) => group.tabs).filter((tab: Tab) =>
        tab.input instanceof vscode.TabInputText && (tab.input as TabInputText).uri.path.endsWith('/realtime-tabs.ts'));
      if (tabs.length !== 2) throw new Error('expected two split tabs');
      await vscode.window.tabGroups.close(tabs[0]);
    });
    await browser.waitUntil(() => retained().some(value => value.event.type === 'editor_opened' && value.event.path === 'realtime-tabs.ts'),
      { timeout: 1000, interval: 25 });
    assert.equal(retained().filter(value => value.event.type === 'editor_closed' && value.event.path === 'realtime-tabs.ts').length, 0);
    await browser.executeWorkbench(async vscode => {
      const tab = vscode.window.tabGroups.all.flatMap((group: TabGroup) => group.tabs).find((tab: Tab) =>
        tab.input instanceof vscode.TabInputText && (tab.input as TabInputText).uri.path.endsWith('/realtime-tabs.ts'));
      await vscode.window.tabGroups.close(tab);
    });
    await browser.waitUntil(() => retained().some(value => value.event.type === 'editor_closed' && value.event.path === 'realtime-tabs.ts'),
      { timeout: 1000, interval: 25 });
    const lifecycle = retained().filter(value => ['editor_opened', 'editor_closed'].includes(value.event.type) && value.event.path === 'realtime-tabs.ts');
    assert.equal(lifecycle.length, 2);
    measurements.split_lifecycle = lifecycle;
  });

  it('inventories existing split tabs in a fresh recorder without another open action', async () => {
    await show('realtime-reload.ts');
    await show('realtime-reload.ts', 3);
    await browser.waitUntil(() => retained().some(value => value.event.type === 'editor_opened' && value.event.path === 'realtime-reload.ts'),
      { timeout: 1000, interval: 25 });
    const starts = retained().filter(value => value.event.type === 'tracking_started').length;
    // This test host discards its editor layout when the application restarts.
    // Restart the production recorder with the real split tabs still present.
    // The human-work suite separately verifies full host restart and identity.
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('editchain-history.stopTracking');
      await vscode.commands.executeCommand('editchain-history.startTracking');
    });
    await browser.waitUntil(() => retained().filter(value => value.event.type === 'tracking_started').length > starts,
      { timeout: 1000, interval: 25 });
    await browser.waitUntil(() => retained().some(value => value.event.type === 'editor_opened'
      && value.event.path === 'realtime-reload.ts' && value.event.restored), { timeout: 1000, interval: 25 });
    const opens = retained().filter(value => value.event.type === 'editor_opened' && value.event.path === 'realtime-reload.ts');
    assert.equal(opens.filter(value => !value.event.restored).length, 1);
    assert.equal(opens.filter(value => value.event.restored).length, 1, 'existing splits produce one inventory entry');
    measurements.restart_opens = opens;
  });
});
