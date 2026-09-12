import fs from 'node:fs';
import path from 'node:path';

const source = path.join(process.env.EDITCHAIN_LIVE_TEST_ROOT!, 'sessions/rollout-live.jsonl');
function append(type: string, payload: object) {
  fs.appendFileSync(source, JSON.stringify({ timestamp: new Date().toISOString(), type, payload }) + '\n');
}
function message(turn: string, id: string) {
  append('response_item', { type: 'message', id, role: 'assistant', phase: 'commentary',
    content: [{ type: 'output_text', text: `Streamed activity ${id}` }],
    internal_chat_message_metadata_passthrough: { turn_id: turn } });
}
async function state() {
  return browser.execute(() => {
    const rows = Array.from({ length: 600 }, (_, index) => ({ index, row: window.__editchainRowAt?.(index) as any }))
      .filter(entry => entry.row && !entry.row.is_subop)
      .map(({ index, row }) => ({ index, key: row.continuity_key, lane: row.lane, task: row.task_group }));
    const scroller = document.querySelector('#rows')!;
    const box = scroller.getBoundingClientRect();
    const visible = Array.from(document.querySelectorAll('.row[data-continuity]')).filter(row => {
      const rect = row.getBoundingClientRect();
      return rect.bottom > box.top && rect.top < box.bottom;
    }).map(row => ({ key: row.getAttribute('data-continuity'), top: row.getBoundingClientRect().top,
      expanded: row.querySelector('.task-chevron')?.getAttribute('aria-expanded') }));
    return { rows, visible, scroll: scroller.scrollTop, total: window.__editchainGetTotal?.() };
  });
}
async function idle() {
  await browser.execute(async () => {
    await (window as any).__editchainRendererDebug.whenIdle(10000);
    await Promise.all(document.getAnimations().map(animation => animation.finished.catch(() => undefined)));
  });
  await browser.waitUntil(() => browser.execute(() => {
    const box = document.querySelector('#rows')!.getBoundingClientRect();
    return Array.from(document.querySelectorAll('.row[data-continuity]')).filter(row => {
      const rect = row.getBoundingClientRect();
      return rect.bottom > box.top && rect.top < box.bottom;
    }).every(row => {
      const node = row.querySelector('.graphDot, .graphBundleCapsule');
      return !!node && getComputedStyle(node).opacity === '1' && node.getBoundingClientRect().width > 0;
    });
  }), { timeout: 10000, timeoutMsg: 'visible graph nodes did not finish appearing' });
}
async function scroll(bottom: boolean) {
  await browser.execute(bottom => {
    const scroller = document.querySelector('#rows')!;
    scroller.scrollTop = bottom ? scroller.scrollHeight - scroller.clientHeight : 0;
    scroller.dispatchEvent(new Event('scroll'));
  }, bottom);
  await idle();
}
async function task(turn: string) {
  return (await state()).rows.find(row => row.task?.turn_id === turn);
}
async function toggle(key: string) {
  const before = (await state()).rows.find(row => row.key === key)?.task.expanded;
  await browser.execute(key => {
    const row = Array.from(document.querySelectorAll('.row[data-continuity]')).find(row => row.getAttribute('data-continuity') === key);
    (row?.querySelector('.task-chevron') as HTMLButtonElement)?.click();
  }, key);
  await browser.waitUntil(async () => (await state()).rows.find(row => row.key === key)?.task.expanded !== before, { timeout: 30000 });
  await idle();
}

describe('Offscreen live task disclosure', () => {
  it('opens the latest task, folds it only when wholly offscreen, and preserves explicit choices and scroll anchors', async () => {
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('editchain-history.open');
    });
    const workbench = await browser.getWorkbench();
    let view: Awaited<ReturnType<typeof workbench.getWebviewByTitle>> | undefined;
    await browser.waitUntil(async () => { try { view = await workbench.getWebviewByTitle('EditChain History'); return true; } catch { return false; } }, { timeout: 30000 });
    await view!.open();
    await browser.waitUntil(async () => (await task('offscreen-49'))?.task.expanded === true, { timeout: 60000 });
    await idle();
    const initial = await state();
    expect(initial.rows.filter(row => row.task && row.task.turn_id !== 'offscreen-49').every(row => row.task.expanded === false)).toBe(true);
    const pinned = await task('offscreen-49');
    expect(initial.visible.find(row => row.key === pinned!.key)?.expanded).toBe('true');
    expect(initial.rows.some(row => row.key.endsWith(':seed-49-1'))).toBe(true);
    await browser.saveScreenshot(path.resolve('trace/latest-task-open.png'));
    // Closing then reopening records an explicit choice, independent of the default.
    await toggle(pinned!.key);
    await toggle(pinned!.key);
    await scroll(true);
    const reading = (await state()).visible[0];
    for (let index = 0; index < 10; index++) message('offscreen-49', `pinned-${index}`);
    await browser.waitUntil(async () => (await task('offscreen-49'))?.key.endsWith(':pinned-9') === true, { timeout: 60000 });
    expect((await task('offscreen-49'))!.task.expanded).toBe(true);
    append('event_msg', { type: 'task_started', turn_id: 'fresh-offscreen', model_context_window: null });
    for (let index = 0; index < 100; index++) message('fresh-offscreen', `unseen-${index}`);
    await browser.waitUntil(async () => (await task('fresh-offscreen'))?.key.endsWith(':unseen-99') === true, { timeout: 60000 });
    await idle();
    const unseen = await task('fresh-offscreen');
    expect(unseen!.task.expanded).toBe(false);
    expect(unseen!.task.summarized).toBe(true);
    expect(unseen!.task.member_count).toBeGreaterThan(90);
    const offscreen = await state();
    expect(offscreen.visible[0].key).toBe(reading.key);
    expect(Math.abs(offscreen.visible[0].top - reading.top)).toBeLessThan(1);
    await scroll(false);
    expect((await task('fresh-offscreen'))!.task.summarized).toBe(true);
    await browser.saveScreenshot(path.resolve('trace/offscreen-closed.png'));
    message('fresh-offscreen', 'onscreen-arrival');
    await browser.waitUntil(async () => (await task('fresh-offscreen'))?.key.endsWith(':onscreen-arrival') === true, { timeout: 60000 });
    await idle();
    expect((await task('fresh-offscreen'))!.task.summarized).toBe(false);
    expect((await task('fresh-offscreen'))!.task.expanded).toBe(true);
    const visible = await state();
    expect(visible.visible.find(row => row.key.endsWith(':onscreen-arrival'))?.expanded).toBe('true');
    expect(visible.visible.some(row => row.key?.endsWith(':unseen-98'))).toBe(true);
    await browser.saveScreenshot(path.resolve('trace/latest-task-streaming.png'));
    append('event_msg', { type: 'task_complete', turn_id: 'fresh-offscreen', last_agent_message: null });
    await browser.waitUntil(async () => (await task('fresh-offscreen'))?.task.status === 'completed', { timeout: 60000 });
    expect((await task('fresh-offscreen'))!.task.expanded).toBe(true);
    // Only the ribbon leaves the viewport; members still being read keep it open.
    await browser.execute(() => {
      const scroller = document.querySelector('#rows')!;
      scroller.scrollTop = 300;
      scroller.dispatchEvent(new Event('scroll'));
    });
    await idle();
    const partial = await state();
    expect(partial.visible.some(row => row.key?.endsWith(':onscreen-arrival'))).toBe(false);
    expect(partial.visible.some(row => row.key?.includes(':unseen-'))).toBe(true);
    expect((await task('fresh-offscreen'))!.task.expanded).toBe(true);
    await scroll(true);
    await browser.waitUntil(async () => (await task('fresh-offscreen'))?.task.summarized === true, { timeout: 30000 });
    expect((await task('offscreen-49'))!.task.expanded).toBe(true);
    await scroll(false);
    const final = await state();
    expect((await task('fresh-offscreen'))!.task.expanded).toBe(false);
    expect((await task('fresh-offscreen'))!.lane).toBe(unseen!.lane);
    fs.writeFileSync(path.resolve('trace/offscreen-disclosure.json'), JSON.stringify({ initial, offscreen, visible, partial, final }, null, 2));
    await view!.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('notifications.clearAll'); });
    await view!.open();
    await browser.saveScreenshot(path.resolve('trace/offscreen-settled.png'));
    await view!.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('editchain-history.stopLive'); });
  });
});
