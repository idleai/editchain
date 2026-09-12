import fs from 'node:fs';
import path from 'node:path';

const workspace = process.env.EDITCHAIN_LIVE_TEST_ROOT!;
const source = path.join(workspace, 'sessions', 'rollout-live.jsonl');
function append(type: string, payload: object): void {
  fs.appendFileSync(source, JSON.stringify({ timestamp: new Date().toISOString(), type, payload }) + '\n');
}
function message(id: string): void {
  append('response_item', { type: 'message', id, role: 'assistant',
    content: [{ type: 'output_text', text: `Live incremental item ${id}` }], phase: 'commentary',
    internal_chat_message_metadata_passthrough: { turn_id: 'task-live' } });
}
async function state() {
  return browser.execute(() => {
    const cached: any[] = [];
    for (let index = 0; index < 200; index++) {
      const row = window.__editchainRowAt?.(index) as any;
      if (row && !row.is_subop) cached.push({ key: row.continuity_key, node: row.node_key,
        lane: row.lane, parents: row.parents, summary: row.summary, task: row.task_group, kind: row.kind,
        op: row.op_id, git: row.git_oid });
    }
    const visible = Array.from(document.querySelectorAll('.row[data-continuity]')).map(row => ({
      key: row.getAttribute('data-continuity'), text: row.textContent,
      expanded: row.querySelector('.task-chevron')?.getAttribute('aria-expanded'), header: !!row.querySelector('.task-chevron'),
      dots: Array.from(row.querySelectorAll('.graphDot')).map(dot => {
        const bounds = dot.getBoundingClientRect();
        const cell = row.querySelector('.graph-cell')!.getBoundingClientRect();
        return { opacity: getComputedStyle(dot).opacity, transform: getComputedStyle(dot).transform,
          fill: getComputedStyle(dot).fill, bounds: bounds.toJSON(), cell: cell.toJSON(),
          svg: dot.parentElement?.outerHTML,
          contained: bounds.width > 0 && bounds.left >= cell.left && bounds.right <= cell.right
            && bounds.top >= cell.top && bounds.bottom <= cell.bottom };
      }),
    }));
    return { cached, visible, total: window.__editchainGetTotal?.(), centers: (window as any).__editchainRendererDebug.laneXAll() };
  });
}
async function idle() {
  await browser.execute(async () => {
    await (window as any).__editchainRendererDebug.whenIdle(10000);
    await Promise.all(document.getAnimations().map(animation => animation.finished.catch(() => undefined)));
  });
}
async function toggle(key: string) {
  const before = (await state()).cached.find(row => row.key === key)?.task?.expanded;
  await browser.execute(key => {
    const row = Array.from(document.querySelectorAll('.row[data-continuity]')).find(row => row.getAttribute('data-continuity') === key);
    (row?.querySelector('.task-chevron') as HTMLButtonElement)?.click();
  }, key);
  await browser.waitUntil(async () => (await state()).cached.find(row => row.key === key)?.task?.expanded !== before, { timeout: 30000 });
  await idle();
}

describe('Native task disclosure in live Codex history', () => {
  it('folds completed history, keeps new items visible, preserves physical identities and lanes, and finds hidden work', async () => {
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('editchain-history.open');
    });
    const workbench = await browser.getWorkbench();
    let view: Awaited<ReturnType<typeof workbench.getWebviewByTitle>> | undefined;
    await browser.waitUntil(async () => {
      try { view = await workbench.getWebviewByTitle('EditChain History'); return true; } catch { return false; }
    }, { timeout: 30000 });
    await view!.open();
    await browser.waitUntil(async () => (await state()).visible.filter(row => row.header).length === 2,
      { timeout: 90000, timeoutMsg: 'native task path controls did not reach the view' });
    await idle();
    const initial = await state();
    fs.writeFileSync(path.resolve('trace/task-path-initial.json'), JSON.stringify(initial, null, 2));
    expect(initial.cached.every(row => row.kind !== 'task' && !row.key.startsWith('task:'))).toBe(true);
    expect(initial.cached.every(row => row.op || row.git)).toBe(true);
    expect(initial.cached.filter(row => row.task).every(row => row.task.anchor === row.key && row.task.member_count > 1)).toBe(true);
    const history = initial.cached.find(row => row.task?.turn_id === 'task-history')!;
    const active = initial.cached.find(row => row.task?.turn_id === 'task-live')!;
    expect(history.task.status).toBe('completed');
    // The prompt is the real session-to-Git attachment and stays outside the fold.
    expect(history.task.member_count).toBe(40);
    expect(history.task.title).toBe('Implement the history importer');
    expect(active.task.status).toBe('inProgress');
    expect(initial.visible.find(row => row.key === history.key)?.expanded).toBe('false');
    expect(initial.visible.find(row => row.key === active.key)?.expanded).toBe('true');
    expect(initial.visible.some(row => row.key?.endsWith(':history-20'))).toBe(false);
    expect(initial.visible.find(row => row.key === active.key)?.dots.length).toBe(1);
    expect(initial.visible.find(row => row.key === active.key)?.text).toContain('Working on task grouping 2');
    expect(await browser.execute(() => document.querySelectorAll('.graphBundleCapsule').length)).toBeGreaterThan(0);
    await browser.saveScreenshot(path.resolve('trace/task-grouping-collapsed.png'));

    await browser.execute(key => {
      (window as any).__taskHeaderElement = Array.from(document.querySelectorAll('.row[data-continuity]'))
        .find(row => row.getAttribute('data-continuity') === key);
      const probe = { moves: 0, strokes: 0 };
      (window as any).__taskAnimation = probe;
      const sample = () => {
        probe.moves = Math.max(probe.moves, document.querySelectorAll('.row-live-moved').length);
        probe.strokes = Math.max(probe.strokes, document.getAnimations().filter(animation =>
          (animation as CSSAnimation).animationName === 'ec-graph-grow' && animation.playState === 'running').length);
        requestAnimationFrame(sample);
      };
      sample();
    }, active.key);
    message('live-new-1');
    await browser.waitUntil(async () => (await state()).visible.some(row => row.key?.endsWith(':live-new-1')), { timeout: 60000 });
    await idle();
    const appended = await state();
    fs.writeFileSync(path.resolve('trace/task-grouping-progress.json'), JSON.stringify({ initial, appended }, null, 2));
    const appendedTask = appended.cached.find(row => row.task?.turn_id === 'task-live')!;
    expect(appendedTask.task.member_count).toBe(5);
    expect(appendedTask.key.endsWith(':live-new-1')).toBe(true);
    expect(await browser.execute(() => (window as any).__taskHeaderElement?.isConnected)).toBe(true);
    append('event_msg', { type: 'task_complete', turn_id: 'task-live', last_agent_message: null });
    await browser.waitUntil(async () => (await state()).cached.find(row => row.key === appendedTask.key)?.task.status === 'completed', { timeout: 60000 });
    await idle();
    expect((await state()).visible.find(row => row.key === appendedTask.key)?.expanded).toBe('true');

    await toggle(appendedTask.key);
    expect((await state()).visible.find(row => row.key === appendedTask.key)?.expanded).toBe('false');
    message('live-new-2');
    await browser.waitUntil(async () => (await state()).visible.some(row => row.key?.endsWith(':live-new-2')), { timeout: 60000 });
    await idle();
    const late = await state();
    const lateTask = late.cached.find(row => row.task?.turn_id === 'task-live')!;
    expect(lateTask.task.expanded).toBe(false);
    expect(lateTask.task.summarized).toBe(false);
    expect(late.visible.find(row => row.key === lateTask.key)?.text).toContain('Live incremental item live-new-2');
    expect(late.visible.some(row => row.key?.endsWith(':live-1'))).toBe(false);
    expect(late.visible.find(row => row.key === history.key)?.expanded).toBe('false');
    for (const row of initial.cached.filter(row => !row.task)) {
      const visible = late.cached.find(candidate => candidate.key === row.key);
      if (visible) expect(visible.lane).toBe(row.lane);
    }
    expect(late.centers).toEqual(initial.centers);
    fs.writeFileSync(path.resolve('trace/task-grouping-geometry.json'), JSON.stringify(late, null, 2));
    expect(late.visible.filter(row => row.key?.includes('live-new')).every(row => row.dots.length === 1 && row.dots[0].contained)).toBe(true);
    await browser.waitUntil(() => browser.execute(() => {
      const row = Array.from(document.querySelectorAll('.row[data-continuity]'))
        .find(row => row.getAttribute('data-continuity')?.endsWith(':live-new-2'));
      const dot = row?.querySelector('.graphDot');
      return !!dot && getComputedStyle(dot).opacity === '1';
    }), { timeout: 5000, timeoutMsg: 'the new activity node did not finish appearing' });
    await browser.saveScreenshot(path.resolve('trace/task-grouping-live.png'));

    await browser.$('#search').setValue('historicalneedle');
    await browser.keys('Enter');
    await browser.waitUntil(() => browser.execute(() => document.querySelector('.row-find-current')?.textContent?.includes('historicalneedle') || false), { timeout: 30000 });
    const searched = await state();
    expect(searched.visible.find(row => row.key === history.key)?.expanded).toBe('false');
    expect(searched.visible.some(row => row.key?.endsWith(':history-19'))).toBe(false);
    await browser.saveScreenshot(path.resolve('trace/task-grouping-search.png'));
    await browser.$('#search').setValue('');
    await browser.keys('Enter');
    await browser.$('#search').setValue('"Historical activity 39"');
    await browser.keys('Enter');
    await browser.waitUntil(() => browser.execute(() => document.querySelector('.row-find-current')?.textContent?.includes('Historical activity 39') || false), { timeout: 30000 });
    const anchorMatch = (await state()).cached.find(row => row.key === history.key)!;
    expect(anchorMatch.task.expanded).toBe(false);
    expect(anchorMatch.task.summarized).toBe(false);
    await browser.$('#search').setValue('');
    await browser.keys('Enter');
    await toggle(history.key);
    expect((await state()).visible.find(row => row.key === history.key)?.expanded).toBe('true');
    const expanded = await state();
    expect(expanded.visible.some(row => row.key?.endsWith(':history-19'))).toBe(true);
    // Native paging deliberately releases hidden rows. Reopen the section to
    // verify that those rows retained their lanes across folding and appends.
    await toggle(lateTask.key);
    const reopened = await state();
    for (const row of initial.cached.filter(row => !row.task)) {
      expect(reopened.cached.find(candidate => candidate.key === row.key)?.lane).toBe(row.lane);
    }
    const animation = await browser.execute(() => (window as any).__taskAnimation);
    expect(animation.moves).toBeGreaterThan(0);
    expect(animation.strokes).toBeGreaterThan(0);
    fs.writeFileSync(path.resolve('trace/task-grouping.json'), JSON.stringify({ initial, appended, late, searched, expanded, animation }, null, 2));
    await browser.saveScreenshot(path.resolve('trace/task-grouping-expanded.png'));
    await view!.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('editchain-history.stopLive'); });
  });
});
