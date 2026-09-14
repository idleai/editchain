import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import type { Tab, TabGroup } from 'vscode';

const output = process.env.EDITCHAIN_WORK_OUTPUT!;
const workspace = path.join(process.env.EDITCHAIN_WORK_FIXTURE!, 'workspace');
const measurements: Record<string, unknown>[] = [];
const inspected = new Set<string>();
const observations: any[] = [];
let webview: Awaited<ReturnType<Awaited<ReturnType<typeof browser.getWorkbench>>['getWebviewByTitle']>>;

// Read each new source once and retain only metadata, never a growing list of
// large before/after buffers. Neither this nor status queries flush capture.
function retained(smallOnly = false): any[] {
  const blobs = path.join(workspace, '.editchain', 'blobs');
  for (const name of fs.readdirSync(blobs).filter(name => /^[a-f0-9]{64}$/.test(name) && !inspected.has(name))) {
    const file = path.join(blobs, name);
    if (smallOnly && fs.statSync(file).size > 8192) continue;
    const fd = fs.openSync(file, 'r');
    const prefix = Buffer.alloc(32);
    try { fs.readSync(fd, prefix, 0, prefix.length, 0); } finally { fs.closeSync(fd); }
    inspected.add(name);
    if (!prefix.toString().startsWith('{"event":')) continue;
    const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
    if (raw.source !== 'vscode.editor') continue;
    const { before, after, text, changes, ...event } = raw.event.event;
    observations.push({ ...raw.event, event, persisted_ms: fs.statSync(file).mtimeMs });
  }
  return observations;
}

async function row(name: string): Promise<any> {
  return await browser.execute(name => Array.from(document.querySelectorAll('#rows .row[data-row]'))
    .filter(element => element.getAttribute('data-file-path') === name)
    .map(element => (window as any).__editchainRowAt?.(Number(element.getAttribute('data-row'))))
    .find(value => value?.file_change?.path === name), name);
}

async function show(name: string): Promise<void> {
  const cursor = await browser.executeWorkbench(async (vscode, name) => {
    const document = await vscode.workspace.openTextDocument(vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, name));
    const editor = await vscode.window.showTextDocument(document, { viewColumn: 1, preview: false });
    const end = document.lineAt(0).range.end;
    editor.selection = new vscode.Selection(end, end);
    // The status bar displays the emoji as one column; API offsets use UTF-16.
    return `Ln 1, Col ${[...document.lineAt(0).text].length + 1}`;
  }, name);
  await browser.waitUntil(async () => browser.execute(cursor => Array.from(document.querySelectorAll('.statusbar-item'))
    .some(element => element.textContent.includes(cursor)), cursor), { timeout: 10000 });
}

describe('large full-snapshot human edits', () => {
  before(async () => {
    await browser.waitUntil(async () => browser.executeWorkbench(vscode =>
      vscode.extensions.getExtension('ambientlight.editchain-history')?.isActive), { timeout: 30000 });
    for (const name of fs.readdirSync(path.join(workspace, '.editchain', 'blobs'))) inspected.add(name);
    fs.writeFileSync(path.join(workspace, 'large-placeholder.ts'), '// keep the code editor group open\n');
    await show('large-placeholder.ts');
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
  });

  after(() => fs.writeFileSync(path.join(output, 'large-files.json'), JSON.stringify(measurements, null, 2)));
  afterEach(async function () {
    if (this.currentTest?.state === 'failed') await browser.saveScreenshot(path.join(output, 'large-file-failure.png'));
    await webview?.close();
  });

  for (const mib of [2, 8]) {
    it(`captures sustained typing in a ${mib} MiB file and opens its exact historical VS Code diff`, async () => {
      const name = `large-${mib}mib.ts`;
      const header = 'export const value = 1; // 😀 \n';
      const padding = '// retained snapshot padding with identical surrounding lines\n';
      const bytes = mib * 1024 * 1024 - 4096;
      const remaining = bytes - Buffer.byteLength(header);
      const before = header + padding.repeat(Math.floor(remaining / padding.length)) + ' '.repeat(remaining % padding.length);
      fs.writeFileSync(path.join(workspace, name), before);
      await browser.executeWorkbench(vscode => vscode.commands.executeCommand('editchain-history.open'));
      await show(name);
      await browser.waitUntil(() => retained().some(value => value.event.type === 'document_snapshot'
        && value.event.document.path === name), { timeout: 10000, interval: 50, timeoutMsg: 'large buffer baseline was skipped' });
      await browser.waitUntil(() => retained().some(value => value.event.type === 'code_read'
        && value.event.document.path === name), { timeout: 5000, interval: 50, timeoutMsg: 'large buffer reading indicator was omitted' });
      await webview.open();
      await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
      await webview.close();

      const started = Date.now();
      await browser.keys('h');
      await webview.open();
      let first: any;
      await browser.waitUntil(async () => { first = await row(name); return !!first; },
        { timeout: 1000, interval: 25, timeoutMsg: 'large unsaved edit did not appear within one second' });
      const firstRowMs = Date.now() - started;
      assert.ok(firstRowMs < 1000);
      assert.equal(first.file_change.source, 'human', 'ordinary typing has direct input or a correlated keyboard receipt');
      await webview.close();
      const suffix = 'uman_snapshot';
      for (const key of suffix) { await browser.keys(key); await browser.pause(75); }
      const savedAt = Date.now();
      await browser.keys(['Control', 's']);
      let saved: any;
      await browser.waitUntil(() => {
        // Do not charge synchronous inspection of every retained full snapshot
        // to save delivery. The save receipt is small; inspect revisions later.
        saved = retained(true).find(value => value.event.type === 'document_saved' && value.event.document.path === name);
        return !!saved;
      }, { timeout: 15000, interval: 25, timeoutMsg: 'large-file save did not reach durable history' });
      const saveObservedMs = Date.now() - savedAt;
      const events = retained();
      const changes = events.filter(value => value.event.type === 'document_changed' && value.event.document.path === name);
      assert.equal(changes.length, suffix.length + 1, 'all intermediate before/after revisions were retained');
      const firstSequence = Math.min(...changes.map(value => value.sequence));
      const input = events.filter(value => value.session === changes[0].session && value.event.type === 'human_edit_batch')
        .flatMap(value => value.event.edits).find(edit => edit.change === firstSequence);
      assert.equal(input?.signal, process.env.EDITCHAIN_CAPTURE_PROPOSED === '1' ? 'editor_input' : 'keyboard_selection');
      assert.ok(!events.some(value => value.event.type === 'tracking_gap'), 'large text did not cause a capture gap');
      const after = before.replace('\n', 'h' + suffix + '\n');
      assert.equal(fs.readFileSync(path.join(workspace, name), 'utf8'), after);

      await webview.open();
      await browser.waitUntil(async () => (await row(name))?.file_change.op_id !== first.file_change.op_id, { timeout: 1000, interval: 25 });
      await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
      await browser.execute(name => Array.from(document.querySelectorAll<HTMLElement>('.row-file'))
        .find(element => element.closest('[data-file-path]')?.getAttribute('data-file-path') === name)?.click(), name);
      await webview.close();
      await browser.waitUntil(async () => browser.executeWorkbench((vscode, name) => vscode.window.tabGroups.all
        .some((group: TabGroup) => group.tabs.some((tab: Tab) => tab.input instanceof vscode.TabInputTextDiff && tab.label.includes(name))), name),
      { timeout: 10000, timeoutMsg: 'large-file row did not open a native historical diff' });
      const exact = await browser.executeWorkbench(async (vscode, name, expectedBefore, expectedAfter) => {
        const tab = vscode.window.tabGroups.all.flatMap((group: TabGroup) => group.tabs)
          .find((tab: Tab) => tab.input instanceof vscode.TabInputTextDiff && tab.label.includes(name));
        const input = tab.input as any;
        const before = await vscode.workspace.openTextDocument(input.original);
        const after = await vscode.workspace.openTextDocument(input.modified);
        return { before: before.getText() === expectedBefore, after: after.getText() === expectedAfter };
      }, name, before, after);
      assert.deepEqual(exact, { before: true, after: true }, 'the native diff contains both complete historical snapshots');
      const timing = { mib, bytes, first_row_ms: firstRowMs, changes: changes.length,
        save_observed_ms: saveObservedMs, save_persist_ms: saved.persisted_ms - saved.time_ms,
        max_capture_ms: Math.max(...changes.map(value => value.persisted_ms - value.time_ms)), exact_diff: exact };
      measurements.push(timing);
      console.log('[large snapshot]', JSON.stringify(timing));
      assert.ok(saveObservedMs < 1000, 'large-file save did not reach durable history within one second');
      await browser.waitUntil(async () => browser.execute(() => Array.from(document.querySelectorAll<HTMLElement>('.monaco-diff-editor .view-line'))
        .some(element => element.offsetParent !== null && element.textContent?.includes('human_snapshot'))),
      { timeout: 10000, timeoutMsg: 'the historical diff opened but did not paint the actual edit' });
      await browser.saveScreenshot(path.join(output, `large-${mib}mib-diff.png`));
    });
  }
});
