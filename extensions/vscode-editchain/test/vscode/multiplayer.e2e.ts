import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import type { Tab, TabGroup } from 'vscode';

const fixture = process.env.EDITCHAIN_MULTIPLAYER_UI_FIXTURE!;
const role = process.env.EDITCHAIN_MULTIPLAYER_UI_ROLE!;
const other = role === 'host' ? 'guest' : 'host';
const output = path.join(process.env.EDITCHAIN_MULTIPLAYER_UI_OUTPUT!, role);
const report: Record<string, unknown> = { role };
const mark = (name: string) => fs.writeFileSync(path.join(fixture, name), String(Date.now()));
const waitFor = async (name: string) => {
  await browser.waitUntil(() => fs.existsSync(path.join(fixture, name)) || fs.existsSync(path.join(fixture, other + '.failed')),
    { timeout: 120000, interval: 100, timeoutMsg: `Waiting for ${name}` });
  if (fs.existsSync(path.join(fixture, other + '.failed'))) throw new Error('The other VS Code test failed');
};

async function ready(): Promise<void> {
  await browser.waitUntil(async () => {
    try { return await browser.executeWorkbench(vscode => vscode.extensions.getExtension('ambientlight.editchain-history')?.isActive); }
    catch { return false; }
  }, { timeout: 60000, interval: 500 });
  await browser.executeWorkbench(async vscode => {
    await vscode.extensions.getExtension('ambientlight.editchain-multiplayer-probe')?.activate();
    await vscode.commands.executeCommand('workbench.action.closeSidebar');
    await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
    await vscode.commands.executeCommand('notifications.clearAll');
  });
}

async function probe(name: string, ...args: unknown[]): Promise<any> {
  return browser.executeWorkbench((vscode, name, args) => vscode.commands.executeCommand('editchain-multiplayer-test.' + name, ...args), name, args);
}
async function status(): Promise<any> {
  return browser.executeWorkbench(async vscode => (await vscode.commands.executeCommand('editchain-history.multiplayerStatus') as any).value);
}
async function live(): Promise<void> {
  await browser.waitUntil(async () => (await status()).peers.some((peer: any) => peer.state === 'Live'), { timeout: 120000, interval: 500, timeoutMsg: 'Approved relay peer did not become live' });
}
async function button(label: string): Promise<boolean> {
  return browser.execute(label => {
    const element = Array.from(document.querySelectorAll<HTMLElement>('.monaco-dialog-box .monaco-button'))
      .find(element => element.getBoundingClientRect().width > 0 && element.textContent?.trim() === label);
    element?.click(); return !!element;
  }, label);
}
async function input(title: string): Promise<void> {
  await browser.waitUntil(async () => browser.execute(title => Array.from(document.querySelectorAll<HTMLElement>('.quick-input-widget'))
    .some(widget => widget.getBoundingClientRect().width > 0 && widget.textContent?.includes(title)), title), { timeout: 20000, interval: 100, timeoutMsg: `Missing input: ${title}` });
}

async function connect(): Promise<void> {
  console.log(`[${role}] production Host/Join command and device approval`);
  await probe('run', role === 'host' ? 'multiplayerHost' : 'multiplayerJoin');
  await input(role === 'host' ? 'Host shared history' : 'Join shared history');
  await probe('clipboard', role === 'host' ? 'request' : 'invitation', false);
  await browser.keys(['Control', 'v']); await browser.keys('Enter');
  await input('Share history from'); await browser.keys('Enter');
  const approval = role === 'host' ? 'Approve device' : 'Join space';
  await browser.waitUntil(() => button(approval), { timeout: 20000, interval: 100, timeoutMsg: 'Device approval dialog missing' }).catch(async error => {
    report.commandResult = await probe('result');
    report.buttons = await browser.execute(() => Array.from(document.querySelectorAll<HTMLElement>('[role="button"],button'))
      .filter(element => element.getBoundingClientRect().width > 0).map(element => ({ text: element.textContent?.trim(), classes: element.className })));
    throw error;
  });
  let result: any;
  await browser.waitUntil(async () => {
    // This is VS Code's ordinary extension/account consent dialog.
    await button('Allow');
    result = await probe('result');
    return !!result;
  }, { timeout: 90000, interval: 250, timeoutMsg: 'Host/Join command did not finish' });
  assert.equal(result.ok, true, result.message || 'Host/Join failed');
}

async function type(file: string, text: string): Promise<void> {
  await browser.executeWorkbench(async (vscode, file) => {
    const document = await vscode.workspace.openTextDocument(vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, file));
    const editor = await vscode.window.showTextDocument(document, { preview: false });
    const end = document.lineAt(0).range.end;
    editor.selection = new vscode.Selection(end, end);
    await vscode.commands.executeCommand('notifications.clearAll');
  }, file);
  await browser.waitUntil(async () => browser.execute(() => !!document.activeElement?.closest('.monaco-editor')), { timeout: 10000 });
  await browser.keys(text); await browser.keys(['Control', 's']);
  await browser.waitUntil(() => fs.readFileSync(path.join(fixture, role, file), 'utf8').includes(text), { timeout: 10000 });
}

async function openLiveHistory(): Promise<void> {
  await browser.executeWorkbench(vscode => vscode.commands.executeCommand('editchain-history.open'));
  const view = await (await browser.getWorkbench()).getWebviewByTitle('EditChain History');
  await view.open();
  await browser.waitUntil(() => browser.execute(() => !!(window as any).__editchainRendererDebug?.dataReady), { timeout: 30000 });
  await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
  report.historyInstance = await browser.execute(() => (window as any).__editchainRendererDebug.instanceId());
  await view.close();
  await browser.waitUntil(() => browser.execute(() => Array.from(document.querySelectorAll('.statusbar-item'))
    .some(item => item.textContent?.includes('Codex retry'))), { timeout: 20000, timeoutMsg: 'Missing helper must report Codex retry while history stays live' });
  report.providerRetryVisible = true;
}

async function receivedDiff(file: string, expected: string, artifact: string): Promise<{ before: string; after: string }> {
  const deadline = Date.now() + 60000;
  for (;;) {
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('editchain-history.open');
      await vscode.commands.executeCommand('workbench.action.closePanel');
    });
    const webview = await (await browser.getWorkbench()).getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.waitUntil(async () => browser.execute(file => Array.from(document.querySelectorAll('#rows .row-file'))
      .some(row => row.getAttribute('data-file-path') === file), file), { timeout: 60000, interval: 200, timeoutMsg: 'Remote captured edit did not appear in History' });
    await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
    assert.equal(await browser.execute(() => (window as any).__editchainRendererDebug.instanceId()), report.historyInstance,
      'received rows update the History view opened before this transfer');
    const rows = await browser.execute(file => Array.from(document.querySelectorAll('#rows .row[data-row]'))
      .filter(row => row.getAttribute('data-file-path') === file)
      .map(row => (window as any).__editchainRowAt?.(Number(row.getAttribute('data-row')))), file);
    fs.writeFileSync(path.join(output, artifact + '-rows.json'), JSON.stringify(rows, null, 2));
    assert.ok(rows.some((row: any) => row.session_meta?.session_title === `${other}-user`),
      'received activity carries the original account name');
    assert.ok(await browser.execute(name => Array.from(document.querySelectorAll('#rows .group-label'))
      .some(header => header.textContent === name), `${other}-user`),
      'the visible session header identifies the other user');
    report.namedRemoteHeader = `${other}-user`;
    await browser.saveScreenshot(path.join(output, artifact + '-history.png'));
    await browser.$(`.row-file[data-file-path="${file}"]`).click();
    await webview.close();
    await browser.waitUntil(async () => browser.executeWorkbench(vscode => vscode.window.tabGroups.all.some((group: TabGroup) =>
      group.tabs.some((tab: Tab) => tab.input instanceof vscode.TabInputTextDiff))), { timeout: 20000, timeoutMsg: 'Remote history click did not open native diff' });
    const diff = await browser.executeWorkbench(async vscode => {
      const tab = vscode.window.tabGroups.all.flatMap((group: TabGroup) => group.tabs).find((tab: Tab) => tab.input instanceof vscode.TabInputTextDiff);
      const input = tab!.input as any;
      const before = await vscode.workspace.openTextDocument(input.original);
      const after = await vscode.workspace.openTextDocument(input.modified);
      return { before: before.getText(), after: after.getText() };
    });
    fs.writeFileSync(path.join(output, artifact + '-diff.json'), JSON.stringify(diff, null, 2));
    await browser.saveScreenshot(path.join(output, artifact + '-diff.png'));
    await browser.executeWorkbench(async vscode => {
      for (const group of vscode.window.tabGroups.all) for (const tab of group.tabs) {
        if (tab.input instanceof vscode.TabInputTextDiff) await vscode.window.tabGroups.close(tab);
      }
    });
    if (diff.after.includes(expected)) return diff;
    assert.ok(Date.now() < deadline, 'native diff must converge to the complete remote edit');
    // A row can arrive before the last keystroke in its live edit group.
    // Reopen its current immutable revision after the next publication.
    await browser.pause(200);
  }
}

describe('packaged multiplayer in independent VS Code instances', () => {
  afterEach(async function () {
    if (this.currentTest?.state === 'failed') {
      mark(role + '.failed');
      await browser.saveScreenshot(path.join(output, 'failure.png')).catch(() => {});
      report.failure = this.currentTest.title;
    }
    fs.writeFileSync(path.join(output, 'observations.json'), JSON.stringify(report, null, 2));
  });
  after(async () => {
    const stopped = await browser.executeWorkbench(vscode => vscode.commands.executeCommand('editchain-history.multiplayerStop')).catch(() => undefined);
    if (report.passed) assert.equal((stopped as any)?.ok, true, 'Stop must complete tunnel cleanup');
  });

  it('approves devices, captures typing, renders remote diffs and resumes after a full host restart', async () => {
    await ready();
    report.version = await browser.executeWorkbench(vscode => vscode.version);
    report.installedPackage = await browser.executeWorkbench(vscode => vscode.extensions.getExtension('ambientlight.editchain-history')?.extensionUri.path.includes('-profile/extensions/ambientlight.editchain-history-'));
    assert.equal(report.installedPackage, true, 'the VSIX must load from the isolated profile installation');
    await openLiveHistory();
    const started = Date.now();
    if (role === 'guest') {
      assert.equal((await browser.executeWorkbench(vscode => vscode.commands.executeCommand('editchain-history.multiplayerRequest')) as any).ok, true);
      await probe('clipboard', 'request', true);
      await waitFor('invitation'); await connect(); await live(); mark('guest.joined');
      const originalHost = (await status()).peers[0].fingerprint;
      const first = await receivedDiff('from-host.ts', 'hostOne', 'received-host');
      assert.equal(first.before, "export const owner = 'host'; // \n");
      assert.equal(first.after, "export const owner = 'host'; // hostOne\n");
      assert.equal(fs.readFileSync(path.join(fixture, role, 'from-host.ts'), 'utf8'), "export const owner = 'guest'; // \n");
      report.workingTreeUnchanged = true;
      await type('from-guest.ts', 'guestOne'); mark('guest.edited');
      await waitFor('host.reloaded'); await live();
      assert.equal((await status()).peers[0].fingerprint, originalHost, 'device identity survives host restart');
      await type('from-guest.ts', 'guestTwo'); mark('guest.edited-again');
      await waitFor('host.verified-reload'); mark('guest.done');
    } else {
      await waitFor('request'); await connect(); await probe('clipboard', 'invitation', true);
      await waitFor('guest.joined'); await live();
      await type('from-host.ts', 'hostOne');
      await waitFor('guest.edited');
      const first = await receivedDiff('from-guest.ts', 'guestOne', 'received-guest');
      assert.equal(first.before, "export const owner = 'guest'; // \n");
      assert.equal(fs.readFileSync(path.join(fixture, role, 'from-guest.ts'), 'utf8'), "export const owner = 'host'; // \n");
      report.workingTreeUnchanged = true;
      const progress = (await status()).peers[0].progress;
      assert.ok(progress.sent_records > 0 && progress.sent_blobs > 0, 'outgoing status confirms the guest saved host history');
      assert.ok(progress.records > 0 && progress.blobs > 0, 'incoming status confirms host saved guest history');
      report.transfers = progress;
      const oldPid = await browser.executeWorkbench(() => process.pid);
      console.log(`[host] full VS Code restart, retaining profile and chain; previous extension host ${oldPid}`);
      const restartingAt = Date.now();
      await browser.reloadSession();
      report.reloadSessionMs = Date.now() - restartingAt;
      console.log(`[host] WebDriver session restarted in ${report.reloadSessionMs} ms`);
      const preparingAt = Date.now();
      await ready();
      await openLiveHistory();
      report.extensionReadyMs = Date.now() - preparingAt;
      assert.notEqual(await browser.executeWorkbench(() => process.pid), oldPid);
      const reconnectingAt = Date.now();
      await live(); mark('host.reloaded');
      report.reconnectMs = Date.now() - reconnectingAt;
      report.restartMs = Date.now() - restartingAt;
      await waitFor('guest.edited-again');
      await receivedDiff('from-guest.ts', 'guestTwo', 'received-after-reload');
      report.restartResumed = true; mark('host.verified-reload');
      await waitFor('guest.done');
    }
    report.elapsedMs = Date.now() - started; report.passed = true;
  });
});
