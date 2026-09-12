import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const workspace = process.env.EDITCHAIN_LIVE_TEST_ROOT!;
const source = path.join(workspace, 'sessions', 'rollout-live.jsonl');

function appendEdit(name: string, precedingLines = 0): void {
  fs.appendFileSync(source, '\n'.repeat(precedingLines) + JSON.stringify({ timestamp: new Date().toISOString(), type: 'event_msg', payload: {
    type: 'item_completed', thread_id: '11111111-1111-7111-8111-111111111111', turn_id: 'turn-live',
    item: { type: 'FileChange', id: `edit-${name}`, status: 'completed', changes: {
      [path.join(workspace, name)]: { type: 'update', unified_diff: '@@ -1 +1 @@\n-before\n+after' },
    } }, started_at_ms: Date.now() - 1, completed_at_ms: Date.now(),
  } }) + '\n');
}

async function hasFile(name: string): Promise<boolean> {
  return browser.execute((name) => {
    const total = window.__editchainGetTotal?.() || 0;
    for (let index = 0; index < Math.min(total + 200, 500); index++) {
      const row = window.__editchainRowAt?.(index);
      if (row?.file_change?.path.endsWith(name)) return true;
    }
    return false;
  }, name);
}

describe('Live Codex history in VS Code', () => {
  it('opens live by default, shows unfinished-turn edits and Git, and resumes without duplicates', async () => {
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
    await browser.waitUntil(() => browser.execute(() =>
      document.getElementById('rows')?.textContent?.includes('Initial live history message') || false), { timeout: 90000 });
    const graph = await browser.execute(() => {
      const rows = Array.from({ length: 300 }, (_, index) => window.__editchainRowAt?.(index));
      const messages = rows.filter(row => !row?.is_subop && row?.summary?.startsWith('Initial live history message'));
      return { straight: messages.some(row => row!.lane > 0 && row!.above.includes(row!.lane)
          && row!.below.includes(row!.lane) && row!.transitions.length === 0),
        gitLeftmost: rows.some(row => row?.git_oid && !row.is_subop && row.lane === 0) };
    });
    expect(graph.straight).toBe(true);
    expect(graph.gitLeftmost).toBe(true);
    await browser.execute(() => {
      const probe = { strokes: 0, curves: 0, partialStrokes: 0, partialCurves: 0 };
      (window as any).__graphGrowthProbe = probe;
      const sample = () => {
        const animations = document.getAnimations().filter(animation =>
          (animation as CSSAnimation).animationName === 'ec-graph-grow' && animation.playState === 'running');
        probe.strokes = Math.max(probe.strokes, animations.length);
        for (const animation of animations) {
          const part = (animation.effect as KeyframeEffect).target as Element;
          const curve = part.tagName === 'path';
          if (curve) probe.curves++;
          const offset = parseFloat(getComputedStyle(part).strokeDashoffset);
          if (offset > -0.99 && offset < -0.01) {
            probe.partialStrokes++;
            if (curve) probe.partialCurves++;
          }
        }
        requestAnimationFrame(sample);
      };
      sample();
    });
    appendEdit('alpha.txt');
    await browser.waitUntil(() => hasFile('alpha.txt'), { timeout: 60000, timeoutMsg: 'first live edit never reached the renderer' });
    // Cross the native batch limit without any later filesystem notification.
    appendEdit('beta.txt', 600);
    await browser.waitUntil(() => hasFile('beta.txt'), { timeout: 60000, timeoutMsg: 'second edit in the active turn never reached the renderer' });
    expect(fs.readFileSync(source, 'utf8')).not.toContain('task_complete');
    expect(await hasFile('alpha.txt')).toBe(true);

    const commitOutput = execFileSync('git', ['-C', workspace, '-c', 'user.name=EditChain Test', '-c', 'user.email=test@example.invalid', 'commit', '--allow-empty', '-m', 'Commit observed by live history'], { encoding: 'utf8' });
    fs.appendFileSync(source, JSON.stringify({ timestamp: new Date().toISOString(), type: 'event_msg', payload: {
      type: 'item_completed', thread_id: '11111111-1111-7111-8111-111111111111', turn_id: 'turn-live',
      item: { type: 'CommandExecution', id: 'commit-command', command: ['git', 'commit', '--allow-empty', '-m', 'Commit observed by live history'],
        cwd: pathToFileURL(workspace).href, parsed_cmd: [], source: 'agent', status: 'completed', stdout: commitOutput, exit_code: 0 },
    } }) + '\n');
    await browser.waitUntil(() => browser.execute(() =>
      document.getElementById('rows')?.textContent?.includes('Commit observed by live history') || false), { timeout: 60000 });
    await browser.waitUntil(() => browser.execute(() => {
      for (let index = 0; index < 50; index++) {
        const row = window.__editchainRowAt?.(index);
        if (row?.git_oid && row.summary.includes('Commit observed by live history')) {
          return row.parents.some(parent => !parent.startsWith('git:')) && row.below.length > 0;
        }
      }
      return false;
    }), { timeout: 60000, timeoutMsg: 'produced-commit edge was not connected to its provider command' });
    await browser.waitUntil(() => browser.execute(() => (window as any).__graphGrowthProbe.partialStrokes > 0),
      { timeout: 10000, timeoutMsg: 'new connections never grew through intermediate lengths' });
    const growth = await browser.execute(() => ({ ...(window as any).__graphGrowthProbe,
      laneCenters: (window as any).__editchainRendererDebug.laneXAll() }));
    expect(growth.partialStrokes).toBeGreaterThan(0);
    expect(growth.laneCenters.slice(1).every((x: number, index: number) =>
      Math.abs(x - growth.laneCenters[index] - 14.76) < 0.01)).toBe(true);
    fs.writeFileSync(path.resolve('trace/live-graph-growth.json'), JSON.stringify(growth, null, 2));
    await view!.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('editchain-history.stopLive'); });
    const chain = path.join(workspace, '.editchain');
    const sizes = () => fs.readdirSync(chain).filter(name => name.endsWith('.eclog')).map(name => [name, fs.statSync(path.join(chain, name)).size]);
    const before = sizes();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('editchain-history.startLive'); });
    view = await workbench.getWebviewByTitle('EditChain History');
    await view.open();
    await browser.waitUntil(() => hasFile('beta.txt'), { timeout: 60000 });
    // A third observed edit proves the resumed collector is running, while
    // existing IDs remain distinct and accepted history grows only for new input.
    const priorKeys = await browser.execute(() => Array.from(document.querySelectorAll('.row[data-key]')).map(row => row.getAttribute('data-key')));
    expect(new Set(priorKeys).size).toBe(priorKeys.length);
    expect(sizes()).toEqual(before);
    appendEdit('gamma.txt');
    await browser.waitUntil(() => hasFile('gamma.txt'), { timeout: 60000 });
    expect(await hasFile('alpha.txt')).toBe(true);
    await browser.execute(async () => {
      const animations = document.getAnimations();
      await Promise.all(animations.map(animation => animation.finished.catch(() => undefined)));
    });
    await browser.saveScreenshot(path.resolve('trace/live-codex-history.png'));
    await view.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('editchain-history.stopLive'); });
  });
});
