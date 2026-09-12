import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import type { Tab, TabGroup } from 'vscode';

type WorkRow = { author: string; parents: string[]; node_key: string; is_subop: boolean;
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

  it('counts a brief distant viewport as exposure without treating the skipped middle as read', async () => {
    await show(170);
    await browser.pause(100);
    const value = await report();
    assert.ok(value.exposed_lines > value.read_lines, 'brief distant exposure is separate from reading');
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
    const readRows = async (): Promise<WorkRow[]> => await browser.execute(() => Array.from(document.querySelectorAll('#rows .row[data-row]'))
      .map(row => (window as any).__editchainRowAt?.(Number(row.getAttribute('data-row')))).filter(Boolean)) as unknown as WorkRow[];
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
    const toggleEpisode = async (task: string) => {
      await browser.waitUntil(async () => browser.execute(task => {
        // Live arrivals can move the header to a newer fragment; episode identity stays stable.
        const element = Array.from(document.querySelectorAll<HTMLElement>('#rows .row[data-row]')).find(row =>
          (window as any).__editchainRowAt?.(Number(row.dataset.row))?.task_group?.task_id === task);
        const button = element?.querySelector<HTMLElement>('.task-chevron');
        button?.click();
        return !!button;
      }, task), { timeout: 10000, timeoutMsg: `episode disclosure is not rendered: ${task}` });
    };
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
});
