import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import type { Tab, TabGroup, TabInputText } from 'vscode';

type WorkRow = { author: string; parents: string[]; node_key: string; is_subop: boolean;
  group: string;
  activity_kind: string; summary: string; timestamp_ms: number; kind: string; is_system: boolean;
  sub_ops?: { kind: string }[]; file_change?: { source: string };
  task_group?: { task_id: string; expanded: boolean; member_count: number } };

const output = path.resolve('trace', `work-${process.env.EDITCHAIN_CAPTURE_VSCODE || '1.137.0'}`);
async function show(line = 0): Promise<void> {
  await browser.executeWorkbench(async (vscode, line) => {
    const doc = await vscode.workspace.openTextDocument(vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, 'ai.ts'));
    const editor = await vscode.window.showTextDocument(doc, { preview: false });
    editor.selection = new vscode.Selection(line, 0, line, 0);
    editor.revealRange(new vscode.Range(line, 0, line, 0), vscode.TextEditorRevealType.AtTop);
  }, line);
  await browser.waitUntil(async () => browser.executeWorkbench(vscode => vscode.window.activeTextEditor?.document.uri.path.endsWith('/ai.ts')), { timeout: 10000 });
}
async function report(): Promise<any> {
  const value = await browser.executeWorkbench(async vscode => vscode.commands.executeCommand('editchain-history.humanWork'));
  assert.ok(value, 'production coverage command returned a report');
  return value;
}

async function readRows(): Promise<WorkRow[]> {
  return await browser.execute(() => Array.from(document.querySelectorAll('#rows .row[data-row]'))
    .map(row => (window as any).__editchainRowAt?.(Number(row.getAttribute('data-row')))).filter(Boolean)) as unknown as WorkRow[];
}

async function toggleEpisode(task: string): Promise<void> {
  await browser.waitUntil(async () => browser.execute(task => {
    // Live arrivals can move the header; episode identity stays stable.
    const element = Array.from(document.querySelectorAll<HTMLElement>('#rows .row[data-row]')).find(row =>
      (window as any).__editchainRowAt?.(Number(row.dataset.row))?.task_group?.task_id === task);
    const button = element?.querySelector<HTMLElement>('.task-chevron');
    button?.click();
    return !!button;
  }, task), { timeout: 10000, timeoutMsg: `episode disclosure is not rendered: ${task}` });
}

async function lifecycleFile(name: string): Promise<void> {
  await browser.executeWorkbench(async (vscode, name) => {
    const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, name);
    await vscode.workspace.fs.writeFile(uri, new TextEncoder().encode('export const lifecycle = true;\n'));
    await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uri), { preview: false });
    const tab = vscode.window.tabGroups.all.flatMap((group: TabGroup) => group.tabs).find((tab: Tab) =>
      tab.input instanceof vscode.TabInputText && (tab.input as TabInputText).uri.toString() === uri.toString());
    if (!tab || !await vscode.window.tabGroups.close(tab)) throw new Error('test tab did not close');
  }, name);
  await report();
}

async function typeSuffix(text: string): Promise<void> {
  const target = await browser.executeWorkbench(vscode => {
    const editor = vscode.window.activeTextEditor;
    const end = editor.document.lineAt(editor.selection.active.line).range.end;
    editor.selection = new vscode.Selection(end, end);
    return `Ln ${end.line + 1}, Col ${end.character + 1}`;
  });
  // Extension-host selection setters return before the workbench applies them.
  // Wait for the real cursor before sending real keyboard input to Monaco.
  await browser.waitUntil(async () => browser.execute(expected => Array.from(document.querySelectorAll('.statusbar-item'))
    .some(item => item.textContent.includes(expected)), target), { timeout: 10000, timeoutMsg: `workbench cursor did not reach ${target}` });
  await browser.keys(text);
}

describe('production human-work capture', () => {
  it('records startup and viewport reading against canonical AI provenance without opening History', async () => {
    await browser.waitUntil(async () => browser.executeWorkbench(vscode => vscode.extensions.getExtension('ambientlight.editchain-history')?.isActive), { timeout: 30000 });
    await show();
    await browser.pause(2500);
    const value = await report();
    fs.writeFileSync(path.join(output, 'initial-report.json'), JSON.stringify(value, null, 2));
    assert.equal(value.ai_lines, 200);
    assert.ok(value.read_lines > 0 && value.read_lines < 100, 'only the visible part of the file qualified');
    assert.equal(value.edited_lines, 0);
  });

  it('tracks real typing before save and retains human edited AI origins after save', async () => {
    await show();
    await typeSuffix(' // human');
    const text = await browser.executeWorkbench(vscode => vscode.window.activeTextEditor.document.getText());
    assert.ok(text.includes('// human'));
    const unsaved = await report();
    assert.ok(unsaved.human_changes > 0, 'real typing produced human indicators');
    assert.equal(unsaved.historical_ai_lines_edited, 1);
    await show();
    await browser.executeWorkbench(async vscode => vscode.window.activeTextEditor.document.save());
    const saved = await report();
    fs.writeFileSync(path.join(output, 'edited-report.json'), JSON.stringify(saved, null, 2));
    assert.equal(saved.ai_lines, 200);
    assert.equal(saved.edited_lines, 1);
    assert.equal(saved.capture_gaps, 0);
  });

  it('discards a brief distant viewport without adding exposure or treating the skipped middle as read', async () => {
    const before = await report();
    await show(170);
    await browser.pause(100);
    const value = await report();
    assert.equal(value.read_lines, before.read_lines, 'brief visibility produces no read');
    assert.equal(value.exposed_lines, value.read_lines, 'new capture sends no skimming evidence');
    assert.ok(value.read_lines < 100, 'navigation does not fill the skipped line interval');
    fs.writeFileSync(path.join(output, 'final-report.json'), JSON.stringify(value, null, 2));
  });

  it('pauses and resumes independently of History and preserves existing coverage', async () => {
    await browser.executeWorkbench(async vscode => vscode.commands.executeCommand('editchain-history.stopTracking'));
    const paused = await report();
    await show(150);
    await typeSuffix(' // untracked');
    const stillPaused = await report();
    assert.equal(stillPaused.events, paused.events, 'pause adds no observations');
    await browser.executeWorkbench(async vscode => vscode.commands.executeCommand('editchain-history.startTracking'));
    await show(150);
    await typeSuffix(' // tracked');
    const resumed = await report();
    assert.ok(resumed.human_changes > paused.human_changes, 'resume captures a new recorder incarnation');
    assert.equal(resumed.capture_gaps, 0);
  });

  it('renders connected live human work from Git and opens the exact recorded edit', async () => {
    fs.appendFileSync(path.join(output, 'graph-steps.log'), 'open history\n');
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('editchain-history.open');
    });
    const workbench = await browser.getWorkbench();
    const webview = await workbench.getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.waitUntil(async () => (await readRows()).some(row => row.author === 'human'), { timeout: 30000, timeoutMsg: 'live history did not project human work' });
    fs.appendFileSync(path.join(output, 'graph-steps.log'), 'initial rows received\n');
    const initial = await readRows();
    assert.ok(initial.some(row => row.author === 'human' && row.parents.some(parent => parent.startsWith('git:'))), 'human series branches from its recorded Git base');
    assert.ok(initial.some(row => row.author === 'human' && row.parents.some(parent => !parent.startsWith('git:'))), 'successive human fragments form a chain');
    const initialKeys = initial.filter(row => row.author === 'human' && !row.is_subop).map(row => row.node_key);
    await webview.close();

    // Real typing while the same History panel remains alive must arrive as a delta.
    fs.appendFileSync(path.join(output, 'graph-steps.log'), 'show live editor\n');
    await show(1);
    const typedAt = Date.now();
    fs.appendFileSync(path.join(output, 'graph-steps.log'), 'type live edit\n');
    await typeSuffix(' // live human');
    fs.appendFileSync(path.join(output, 'graph-steps.log'), 'request live coverage\n');
    const after = await report();
    fs.appendFileSync(path.join(output, 'graph-steps.log'), 'coverage received\n');
    assert.ok(after.historical_ai_lines_edited >= 2);
    await browser.executeWorkbench(async vscode => vscode.commands.executeCommand('editchain-history.open'));
    await webview.open();
    await browser.waitUntil(async () => {
      const rows = await readRows();
      return rows.some(row => row.author === 'human' && row.timestamp_ms >= typedAt && !initialKeys.includes(row.node_key))
        // The fixture's agent Import owns its generated file and stays visible.
        // Every other Import here would be leaked editor transport evidence.
        && rows.every(row => row.kind !== 'import' || row.node_key === '83:0:1');
    }, { timeout: 30000, timeoutMsg: 'new human work did not reach the open panel without raw transport rows' }).catch(async error => {
      fs.writeFileSync(path.join(output, 'stalled-rows.json'), JSON.stringify({ typedAt, initialKeys, rows: await readRows() }, null, 2));
      throw error;
    });
    fs.appendFileSync(path.join(output, 'graph-steps.log'), 'live delta visible\n');
    const rows = await readRows();
    assert.ok(rows.some(row => row.author === 'agent' || row.file_change?.source === 'agent'));
    assert.ok(rows.some(row => row.author === 'human' && row.activity_kind === 'explore'), 'revision-bound reading indicators are visible');
    fs.writeFileSync(path.join(output, 'human-graph-rows.json'), JSON.stringify(rows, null, 2));
    for (const row of rows.filter(row => row.task_group?.expanded)) {
      const task = row.task_group!.task_id;
      await toggleEpisode(task);
      await browser.waitUntil(async () => (await readRows()).find(item => item.task_group?.task_id === task)?.task_group?.expanded === false, { timeout: 10000 });
    }
    await browser.execute(() => { document.getElementById('rows')!.scrollTop = 0; });
    await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
    await browser.waitUntil(async () => browser.execute(() => {
      const rows = Array.from(document.querySelectorAll('#rows .row[data-row]:not(.row-placeholder)'))
        .filter(row => row.getClientRects().length).map(row => row.getBoundingClientRect().top).sort((a, b) => a - b);
      return rows.length >= 4 && rows.every((top, i) => i === 0 || top - rows[i - 1] < 120);
    }), { timeout: 10000, timeoutMsg: 'folded human episodes left blank space between graph rows' });
    await browser.saveScreenshot(path.join(output, 'human-edits-graph.png'));
    await browser.$('body').saveScreenshot(path.join(output, 'human-edits-graph-webview.png'));
    const overview = await readRows();
    assert.ok(overview.some(row => row.parents.some(parent => parent.startsWith('git:'))), 'folding preserves visible Git connections');
    const episode = overview.find(row => row.author === 'human' && (row.task_group?.member_count ?? 0) > 1);
    assert.ok(episode, 'human work uses native episode disclosure');
    const task = episode.task_group!.task_id;
    await toggleEpisode(task);
    await browser.waitUntil(async () => (await readRows()).find(row => row.task_group?.task_id === task)?.task_group?.expanded === true, { timeout: 10000 });
    const editKey = await browser.waitUntil(async () => browser.execute(() => {
      const row = Array.from(document.querySelectorAll<HTMLElement>('#rows .row[data-row]')).find(element => {
        const wire = (window as any).__editchainRowAt?.(Number(element.dataset.row));
        return element.querySelector('.subop-chevron') && wire?.author === 'human' && wire?.sub_ops?.some((op: any) => op.kind === 'file');
      });
      return row?.dataset.key;
    }), { timeout: 10000, timeoutMsg: 'expanded human episode did not expose an edit disclosure' });
    await browser.execute(key => document.querySelector<HTMLElement>(`.row[data-key="${key}"] .subop-chevron`)?.click(), editKey);
    await browser.$('.row-file[data-file-source="human"]').waitForExist({ timeout: 10000 });
    await browser.saveScreenshot(path.join(output, 'human-edits-detail.png'));
    await browser.$('.row-file[data-file-source="human"]').click();
    await webview.close();
    await browser.waitUntil(async () => browser.executeWorkbench(vscode => vscode.window.tabGroups.all.some((group: TabGroup) => group.tabs.some((tab: Tab) => tab.input instanceof vscode.TabInputTextDiff))), { timeout: 10000, timeoutMsg: 'human file click did not open the native diff' }).catch(async error => {
      await browser.saveScreenshot(path.join(output, 'human-diff-failed.png'));
      fs.writeFileSync(path.join(output, 'human-diff-failed.json'), JSON.stringify(await browser.executeWorkbench(vscode =>
        vscode.window.tabGroups.all.flatMap((group: TabGroup) => group.tabs.map(tab => ({ label: tab.label, input: tab.input })))), null, 2));
      throw error;
    });
    const diff = await browser.executeWorkbench(async vscode => {
      const tab = vscode.window.tabGroups.all.flatMap((group: TabGroup) => group.tabs).find((tab: Tab) => tab.input instanceof vscode.TabInputTextDiff);
      const input = tab.input as any;
      const before = await vscode.workspace.openTextDocument(input.original);
      const after = await vscode.workspace.openTextDocument(input.modified);
      return { label: tab.label, before: before.getText(), after: after.getText() };
    });
    assert.ok(diff.label.includes('human'), 'native diff identifies human evidence');
    assert.notEqual(diff.before, diff.after);
    assert.ok(diff.after.length > diff.before.length, 'the real inserted text appears on the after side');
    fs.writeFileSync(path.join(output, 'human-native-diff.json'), JSON.stringify(diff, null, 2));
    await browser.saveScreenshot(path.join(output, 'human-native-diff.png'));
  });

  it('shows genuine tab open and close events in the connected graph without claiming human review', async () => {
    const before = await report();
    await browser.executeWorkbench(async vscode => {
      const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, 'lifecycle.ts');
      await vscode.workspace.fs.writeFile(uri, new TextEncoder().encode('export const lifecycle = true;\n'));
      await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uri), { preview: false });
    });
    await browser.waitUntil(async () => browser.executeWorkbench(vscode =>
      vscode.window.tabGroups.all.some((group: TabGroup) => group.tabs.some((tab: Tab) =>
        tab.input instanceof vscode.TabInputText && (tab.input as TabInputText).uri.path.endsWith('/lifecycle.ts')))), { timeout: 10000 });
    const closed = await browser.executeWorkbench(async vscode => {
      const tab = vscode.window.tabGroups.all.flatMap((group: TabGroup) => group.tabs).find((tab: Tab) =>
        tab.input instanceof vscode.TabInputText && (tab.input as TabInputText).uri.path.endsWith('/lifecycle.ts'));
      return await vscode.window.tabGroups.close(tab);
    });
    assert.equal(closed, true);
    const after = await report();
    assert.equal(after.human_changes, before.human_changes);
    assert.equal(after.read_lines, before.read_lines);
    assert.equal(after.edited_lines, before.edited_lines);
    await browser.executeWorkbench(async vscode => vscode.commands.executeCommand('editchain-history.open'));
    const webview = await (await browser.getWorkbench()).getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.execute(() => { document.getElementById('rows')!.scrollTop = 0; });
    await browser.waitUntil(async () => (await readRows()).some(row => row.kind === 'editor_closed' && row.summary.includes('lifecycle.ts')),
      { timeout: 30000, timeoutMsg: 'closed editor never appeared in History' });
    const header = (await readRows()).find(row => row.kind === 'editor_closed' && row.summary.includes('lifecycle.ts'));
    if (header?.task_group && !header.task_group.expanded) await toggleEpisode(header.task_group.task_id);
    await browser.waitUntil(async () => (await readRows()).some(row => row.kind === 'editor_opened' && row.summary.includes('lifecycle.ts')),
      { timeout: 10000, timeoutMsg: 'opened editor was missing from its expanded human episode' });
    const rows = await readRows();
    const opened = rows.find(row => row.kind === 'editor_opened' && row.summary.includes('lifecycle.ts'))!;
    const finished = rows.find(row => row.kind === 'editor_closed' && row.summary.includes('lifecycle.ts'))!;
    assert.ok(finished.parents.includes(opened.node_key), 'close follows the matching open in the human series');
    assert.ok(rows.every(row => row.kind !== 'exposure'));
    fs.writeFileSync(path.join(output, 'editor-lifecycle-rows.json'), JSON.stringify(rows, null, 2));
    await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
    await browser.waitUntil(async () => browser.execute(() => {
      const viewport = document.getElementById('rows')!.getBoundingClientRect();
      const tops = Array.from(document.querySelectorAll('#rows .row[data-row]:not(.row-placeholder)'))
        .map(row => row.getBoundingClientRect()).filter(rect => rect.bottom > viewport.top && rect.top < viewport.bottom)
        .map(rect => rect.top).sort((a, b) => a - b);
      return tops.length > 1 && tops.every((top, i) => i === 0 || top - tops[i - 1] >= 30);
    }), { timeout: 10000, timeoutMsg: 'lifecycle rows overlapped after disclosure animation' });
    await browser.saveScreenshot(path.join(output, 'editor-lifecycle-graph.png'));
    await webview.close();
  });

  it('keeps one unsigned human branch across a full VS Code restart with the same profile', async () => {
    await lifecycleFile('identity-before.ts');
    await browser.executeWorkbench(async vscode => vscode.commands.executeCommand('editchain-history.open'));
    let webview = await (await browser.getWorkbench()).getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.execute(() => { document.getElementById('rows')!.scrollTop = 0; });
    await browser.waitUntil(async () => (await readRows()).some(row => row.kind === 'editor_closed' && row.summary.includes('identity-before.ts')), { timeout: 30000 });
    const previous = (await readRows()).find(row => row.kind === 'editor_closed' && row.summary.includes('identity-before.ts'))!;
    await webview.close();
    const identityPath = path.join(process.env.EDITCHAIN_WORK_FIXTURE!, 'profile/settings/User/globalStorage/ambientlight.editchain-history/unsigned-human-identity.json');
    const identity = JSON.parse(fs.readFileSync(identityPath, 'utf8'));
    const oldPid = await browser.executeWorkbench(() => process.pid);
    // The extension-test host exits on workbench.action.reloadWindow. Restart
    // the application through WebDriver, retaining the same profile/chain.
    await browser.reloadSession();
    await browser.waitUntil(async () => {
      try {
        return await browser.executeWorkbench((vscode, oldPid) => process.pid !== oldPid && vscode.extensions.getExtension('ambientlight.editchain-history')?.isActive, oldPid);
      } catch { return false; }
    }, { timeout: 60000, interval: 250, timeoutMsg: 'VS Code extension host did not restart' });
    assert.deepEqual(JSON.parse(fs.readFileSync(identityPath, 'utf8')), identity);
    await lifecycleFile('identity-after.ts');
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('notifications.clearAll');
    });
    await browser.executeWorkbench(async vscode => vscode.commands.executeCommand('editchain-history.open'));
    webview = await (await browser.getWorkbench()).getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.execute(() => { document.getElementById('rows')!.scrollTop = 0; });
    let next: WorkRow | undefined;
    await browser.waitUntil(async () => {
      next = (await readRows()).find(row => row.kind === 'editor_closed' && row.summary.includes('identity-after.ts'));
      return !!next;
    }, { timeout: 30000 });
    assert.ok(next);
    // Work IDs remain recorder-qualified even when a short episode has no
    // task disclosure metadata. The graph group is the persistent identity.
    const incarnation = next.node_key.split(':')[0];
    const expanded = new Set<string>();
    for (let count = 0; count < 8; count++) {
      const header = (await readRows()).find(row => row.node_key.split(':')[0] === incarnation
        && row.task_group && !row.task_group.expanded && !expanded.has(row.task_group.task_id));
      if (!header) break;
      const task = header.task_group!.task_id;
      expanded.add(task);
      await toggleEpisode(task);
      await browser.waitUntil(async () => (await readRows()).some(row => row.task_group?.task_id === task && row.task_group.expanded), { timeout: 10000 });
      await browser.execute(() => { document.getElementById('rows')!.scrollTop = 0; });
    }
    await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
    await browser.waitUntil(async () => (await readRows()).some(row => row.node_key === next!.node_key), { timeout: 10000 });
    const rows = await readRows();
    fs.writeFileSync(path.join(output, 'human-identity-reload.json'), JSON.stringify({ identity, previous, next, rows }, null, 2));
    assert.equal(next.group, previous.group, 'persistent identity owns the same graph group');
    assert.notEqual(incarnation, previous.node_key.split(':')[0], 'restart creates a fresh recorder incarnation');
    const byId = new Map(rows.map(row => [row.node_key, row]));
    let cursor: WorkRow | undefined = next;
    const seen = new Set<string>();
    while (cursor && cursor.node_key !== previous.node_key && !seen.has(cursor.node_key)) {
      seen.add(cursor.node_key);
      cursor = cursor.parents.map(parent => byId.get(parent)).find(parent => parent?.author === 'human');
    }
    assert.equal(cursor?.node_key, previous.node_key, 'new recording continues the prior human work path');
    assert.ok(rows.every(row => row.kind !== 'exposure'));
    await browser.execute(() => (window as any).__editchainRendererDebug.whenIdle(10000));
    await browser.waitUntil(async () => browser.execute(keys => keys.every(key => {
      const row = Array.from(document.querySelectorAll<HTMLElement>('#rows .row[data-row]')).find(element =>
        (window as any).__editchainRowAt?.(Number(element.dataset.row))?.node_key === key);
      const cell = row?.querySelector('.graph-cell');
      const dot = cell?.querySelector('circle[data-graph-key]');
      if (!cell || !dot || getComputedStyle(dot).opacity !== '1') return false;
      const bounds = dot.getBoundingClientRect(), frame = cell.getBoundingClientRect();
      return bounds.width > 0 && bounds.left >= frame.left && bounds.right <= frame.right
        && Array.from(cell.querySelectorAll('[data-graph-key]')).every(part => part.getAnimations().every(animation => animation.playState === 'finished'));
    }), [next!.node_key, previous.node_key]), { timeout: 10000, timeoutMsg: 'human continuation graph did not finish drawing' });
    await browser.saveScreenshot(path.join(output, 'human-identity-reload.png'));
    await webview.close();
  });
});
