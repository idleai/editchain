import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import type { Tab, TabGroup, TabInputText } from 'vscode';

const output = process.env.EDITCHAIN_WORK_OUTPUT!;
const workspace = path.join(process.env.EDITCHAIN_WORK_FIXTURE!, 'workspace');
const measurements: Record<string, unknown> = {};
const proposed = process.env.EDITCHAIN_CAPTURE_PROPOSED === '1';
let webview: Awaited<ReturnType<Awaited<ReturnType<typeof browser.getWorkbench>>['getWebviewByTitle']>>;
const inspectedBlobs = new Set<string>();
const capturedEvents: any[] = [];

// Passive inspection only: never invoke the coverage command or a capture flush.
function retained(): any[] {
  const blobs = path.join(workspace, '.editchain', 'blobs');
  for (const name of fs.readdirSync(blobs).filter(name => /^[a-f0-9]{64}$/.test(name) && !inspectedBlobs.has(name))) {
    const file = path.join(blobs, name);
    let source;
    try { source = JSON.parse(fs.readFileSync(file, 'utf8')); } catch { continue; }
    inspectedBlobs.add(name);
    if (source?.source === 'vscode.editor') capturedEvents.push({ ...source.event, persisted_ms: fs.statSync(file).mtimeMs });
  }
  return capturedEvents;
}

async function show(name: string, column = 1): Promise<void> {
  await browser.executeWorkbench(async (vscode, name, column) => {
    const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, name);
    const document = await vscode.workspace.openTextDocument(uri);
    await vscode.window.showTextDocument(document, { viewColumn: column, preview: false });
  }, name, column);
}

async function rows(file?: string, since = 0, continuity?: string): Promise<any[]> {
  // Filter inside the renderer: moving its entire prefetched history over
  // WebDriver on every poll would become the dominant measured latency.
  return await browser.execute((file, since, continuity) => Array.from(document.querySelectorAll('#rows .row[data-row]'))
    .filter(row => !file || row.getAttribute('data-file-path') === file)
    .map(row => (window as any).__editchainRowAt?.(Number(row.getAttribute('data-row'))))
    .filter(row => row && (!file || row.file_change?.path === file) && row.timestamp_ms >= since
      && (!continuity || row.continuity_key === continuity)), file, since, continuity) as unknown as any[];
}

describe('realtime human work without forced flushing', () => {
  before(async () => {
    const openingAt = Date.now();
    await browser.waitUntil(async () => browser.executeWorkbench(vscode =>
      vscode.extensions.getExtension('ambientlight.editchain-history')?.isActive), { timeout: 30000 });
    // A large retained history must not be re-read while measuring new input.
    for (const name of fs.readdirSync(path.join(workspace, '.editchain', 'blobs'))) inspectedBlobs.add(name);
    for (const name of ['realtime-edit.ts', 'realtime-tabs.ts', 'realtime-reload.ts']) {
      fs.writeFileSync(path.join(workspace, name), 'export const value = 1; // \n');
    }
    fs.writeFileSync(path.join(workspace, 'realtime-delete.ts'), 'export const value = 1; // humanwork\n');
    // Keep a file in the left group so moving History cannot remove that group.
    await show('realtime-edit.ts');
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('editchain-history.open');
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
    });
    webview = await (await browser.getWorkbench()).getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.waitUntil(async () => browser.execute(() => !!document.querySelector('#rows .row[data-row]')), { timeout: 30000 });
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
    // Finish the initial page/layout handshake before measuring a new edit.
    // Existing cached rows can paint before the live graph has attached.
    await webview.open();
    await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(30000));
    await webview.close();
    measurements.history_ready_ms = Date.now() - openingAt;
  });

  after(() => fs.writeFileSync(path.join(output, 'human-realtime.json'), JSON.stringify(measurements, null, 2)));
  afterEach(async function () {
    if (this.currentTest?.state === 'failed' && webview) {
      await webview.close();
      await webview.open();
      const name = `failure-${this.currentTest.title.split(' ')[0]}-${Date.now()}`;
      fs.writeFileSync(path.join(output, name + '.json'), JSON.stringify(await rows(), null, 2));
      await browser.saveScreenshot(path.join(output, name + '.png'));
    }
    await webview?.close();
  });

  it('shows the first edit within one second and updates the same row through corrections and save', async () => {
    const started = Date.now();
    await browser.keys('h');
    await webview.open();
    assert.equal(await browser.execute(() => document.visibilityState), 'visible');
    let first: any;
    await browser.waitUntil(async () => {
      first = (await rows('realtime-edit.ts', started))[0];
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
      edited = (await rows('realtime-edit.ts', 0, first.continuity_key))[0];
      return !!edited?.file_change && edited.file_change.op_id !== first.file_change.op_id;
    }, { timeout: 1000, interval: 25 });
    assert.equal((await rows('realtime-edit.ts', started)).length, 1);
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

  it('shows deletion of existing code within one second and coalesces all nine deletions through save', async () => {
    await show('realtime-delete.ts');
    const column = await browser.executeWorkbench(vscode => {
      const editor = vscode.window.activeTextEditor, end = editor.document.lineAt(0).range.end;
      editor.selection = new vscode.Selection(end, end);
      return end.character + 1;
    });
    await browser.waitUntil(async () => browser.execute(expected => Array.from(document.querySelectorAll('.statusbar-item'))
      .some(item => item.textContent.includes(expected)), `Ln 1, Col ${column}`), { timeout: 10000 });
    const started = Date.now();
    await browser.keys('Backspace');
    await webview.open();
    let first: any;
    await browser.waitUntil(async () => {
      first = (await rows('realtime-delete.ts', started))[0];
      return !!first;
    }, { timeout: 1000, interval: 25, timeoutMsg: 'deletion of existing code never became a visible edit' });
    measurements.deletion_row_ms = Date.now() - started;
    assert.equal(first.file_change.source, proposed ? 'human' : 'editor');
    if (!proposed) assert.ok(await browser.execute(() => Array.from(document.querySelectorAll('[data-file-source="editor"]'))
      .some(row => row.textContent?.includes('unattributed'))));
    await webview.close();
    for (let index = 0; index < 8; index++) await browser.keys('Backspace');
    await browser.keys(['Control', 's']);
    await browser.waitUntil(() => retained().some(value => value.event.type === 'document_saved'
      && value.event.document.path === 'realtime-delete.ts'), { timeout: 1000, interval: 25 });
    const events = retained();
    const changes = events.filter(value => value.event.type === 'document_changed' && value.event.document.path === 'realtime-delete.ts');
    const keys = new Set(changes.map(value => `${value.session}:${value.sequence}`));
    const receipts = events.filter(value => value.event.type === (proposed ? 'human_edit_batch' : 'observed_edit_batch')
      && (proposed ? value.event.edits.map((edit: any) => edit.change) : value.event.changes)
        .some((change: number) => keys.has(`${value.session}:${change}`)));
    assert.equal(changes.length, 9);
    assert.equal(receipts.flatMap(value => proposed ? value.event.edits : value.event.changes).length, 9);
    assert.equal(new Set(receipts.map(value => value.event.group)).size, 1);
    const health: any = await browser.executeWorkbench(vscode => vscode.commands.executeCommand('editchain-history.trackingStatus'));
    assert.equal(health[0].mode, proposed ? 'direct' : 'limited');
    measurements.attribution_status = health;
    await webview.open();
    await browser.waitUntil(async () => {
      const row = (await rows('realtime-delete.ts', 0, first.continuity_key))[0];
      return !!row?.file_change && row.file_change.op_id !== first.file_change.op_id;
    }, { timeout: 1000, interval: 25 });
    assert.equal((await rows('realtime-delete.ts', started)).length, 1);
    await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
    await browser.saveScreenshot(path.join(output, 'existing-code-deletions.png'));
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
    console.log('[recorder restart] opening split tabs');
    await show('realtime-reload.ts');
    await show('realtime-reload.ts', 3);
    await browser.waitUntil(() => retained().some(value => value.event.type === 'editor_opened' && value.event.path === 'realtime-reload.ts'),
      { timeout: 1000, interval: 25 });
    const starts = retained().filter(value => value.event.type === 'tracking_started').length;
    // This test host discards its editor layout when the application restarts.
    // Restart the production recorder with the real split tabs still present.
    // The human-work suite separately verifies full host restart and identity.
    console.log('[recorder restart] stopping');
    await browser.executeWorkbench(vscode => vscode.commands.executeCommand('editchain-history.stopTracking'));
    console.log('[recorder restart] starting');
    await browser.executeWorkbench(vscode => vscode.commands.executeCommand('editchain-history.startTracking'));
    console.log('[recorder restart] checking inventory');
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
