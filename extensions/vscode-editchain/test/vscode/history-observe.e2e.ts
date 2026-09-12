import fs from 'node:fs';
import path from 'node:path';

const root = process.env.EDITCHAIN_OBSERVE_ROOT!;

function watchMarker(marker: string) {
  const source = process.env.EDITCHAIN_OBSERVE_SOURCE!;
  const fd = fs.openSync(source, 'r');
  let offset = fs.fstatSync(fd).size;
  let partial = '';
  const observation = { availableAt: null as number | null, close: () => {} };
  const timer = setInterval(() => {
    const length = Math.min(fs.fstatSync(fd).size - offset, 4 * 1024 * 1024);
    if (length <= 0) return;
    const bytes = Buffer.alloc(length);
    offset += fs.readSync(fd, bytes, 0, length, offset);
    const lines = (partial + bytes.toString('utf8')).split('\n');
    partial = lines.pop() || '';
    for (const line of lines) {
      try {
        const record = JSON.parse(line);
        const item = record.payload;
        const message = item?.type === 'agent_message' ? item.message : item?.role === 'assistant' && item?.type === 'message'
          ? item.content?.map((part: any) => part.text || '').join('') : '';
        if (typeof message === 'string' && message.includes(marker)) observation.availableAt ??= Date.now();
      } catch { /* Incomplete and unrelated provider records are not markers. */ }
    }
  }, 50);
  observation.close = () => { clearInterval(timer); fs.closeSync(fd); };
  return observation;
}

describe('Observe the active Codex session', () => {
  it('receives new real session operations and animates the retained view', async () => {
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
    await browser.execute(() => {
      const probe = { updates: [] as unknown[], animations: 0, edgeAnimations: 0,
        partialStrokes: 0, partialCurves: 0, opens: 0, frames: 0 };
      (window as any).__liveObservation = probe;
      window.addEventListener('message', event => {
        if (event.data?.id === 'open') probe.opens++;
        if (event.data?.id === 'delta') {
          const update = event.data.body?.Ok;
          probe.updates.push({ at: Date.now(), revision: update?.revision, work: update?.work });
        }
      });
      const sample = () => {
        probe.animations = Math.max(probe.animations,
          Array.from(document.querySelectorAll('.row')).flatMap(row => row.getAnimations()).length);
        const growing = document.getAnimations().filter(animation =>
          (animation as CSSAnimation).animationName === 'ec-graph-grow' && animation.playState === 'running');
        probe.edgeAnimations = Math.max(probe.edgeAnimations, growing.length);
        for (const animation of growing) {
          const part = (animation.effect as KeyframeEffect).target as Element;
          const offset = parseFloat(getComputedStyle(part).strokeDashoffset);
          if (offset > -0.99 && offset < -0.01) {
            probe.partialStrokes++;
            if (part.tagName === 'path') probe.partialCurves++;
          }
        }
        probe.frames++;
        requestAnimationFrame(sample);
      };
      sample();
    });
    await browser.waitUntil(() => browser.execute(() =>
      (window as any).__liveObservation.updates.length > 0 &&
      !(window as any).__liveObservation.updates.at(-1)?.work?.provider_pending &&
      (window as any).__editchainRendererDebug?.dataReady &&
      document.querySelector('.row[data-key] .summary') !== null),
    { timeout: 240000, timeoutMsg: 'real rollout did not reach the live renderer' });
    await browser.execute(async () => { await (window as any).__editchainRendererDebug.whenIdle(30000); });
    const marker = `Live observation ${Date.now()}`;
    const baseline = await browser.execute(() => {
      (window as any).__liveObservationNodes = Array.from(document.querySelectorAll('.row[data-key]'));
      (window as any).__liveObservation.animations = 0;
      (window as any).__liveObservation.edgeAnimations = 0;
      (window as any).__liveObservation.partialStrokes = 0;
      (window as any).__liveObservation.partialCurves = 0;
      return { total: window.__editchainGetTotal?.(),
        capturedRows: (window as any).__liveObservationNodes.length,
        ...((window as any).__liveObservation) };
    });
    expect(baseline.capturedRows).toBeGreaterThan(0);
    const available = watchMarker(marker);
    fs.writeFileSync(path.join(root, 'ready.json'), JSON.stringify({ marker, source: process.env.EDITCHAIN_OBSERVE_SOURCE, baseline }, null, 2));
    try {
      await browser.waitUntil(() => browser.execute(marker => {
        for (let index = 0; index < 100; index++) {
          const row = window.__editchainRowAt?.(index);
          if (row?.kind === 'message' && row.summary.includes(marker)) {
            return document.querySelector(`.row[data-key="${CSS.escape(row.node_key)}"]`) !== null;
          }
        }
        return false;
      }, marker), { timeout: 240000, interval: 100, timeoutMsg: 'new assistant progress marker never reached the visible history' });
    } finally { available.close(); }
    await browser.waitUntil(() => browser.execute(() => (window as any).__liveObservation.partialStrokes > 0),
      { timeout: 10000, timeoutMsg: 'connections never grew through intermediate stroke lengths' });
    const evidence = await browser.execute(() => ({
      at: Date.now(),
      total: window.__editchainGetTotal?.(), text: document.getElementById('rows')?.textContent,
      reusedRows: (window as any).__liveObservationNodes.filter((node: Element) => node.isConnected).length,
      graphState: (window as any).__editchainRendererDebug.graphState(),
      taskHeaders: Array.from(document.querySelectorAll('.row-task-group[data-row]')).map(node => ({
        key: node.getAttribute('data-continuity'),
        task: (window.__editchainRowAt?.(Number(node.getAttribute('data-row'))) as any)?.task_group,
        inventedNodes: node.querySelectorAll('.graphDot, .graphBundleCapsule').length,
      })),
      laneCenters: (window as any).__editchainRendererDebug.laneXAll(),
      graph: Array.from(document.querySelectorAll('.row[data-row]')).map(node => {
        const row = window.__editchainRowAt?.(Number(node.getAttribute('data-row')));
        return row && !row.is_subop ? { key: row.node_key, kind: row.kind, lane: row.lane,
          git: Boolean(row.git_oid), above: row.above, below: row.below, transitions: row.transitions } : null;
      }).filter(Boolean),
      ...((window as any).__liveObservation),
    }));
    fs.writeFileSync(path.join(root, 'evidence.json'), JSON.stringify({ marker, source: process.env.EDITCHAIN_OBSERVE_SOURCE, sourceAvailableAt: available.availableAt, baseline, evidence }, null, 2));
    expect(evidence.updates.length).toBeGreaterThan(baseline.updates.length);
    expect(evidence.animations).toBeGreaterThan(0);
    expect(evidence.edgeAnimations).toBeGreaterThan(0);
    expect(evidence.partialStrokes).toBeGreaterThan(0);
    expect(evidence.reusedRows).toBeGreaterThan(0);
    expect(evidence.taskHeaders.length).toBeGreaterThan(0);
    expect(evidence.taskHeaders.every((header: any) => header.task?.thread_id && header.task?.turn_id && header.inventedNodes === 0)).toBe(true);
    expect(evidence.opens).toBe(baseline.opens);
    expect(evidence.graph.some((row: any) => row.kind === 'message' && row.lane > 0)).toBe(true);
    expect(evidence.laneCenters[1] - evidence.laneCenters[0]).toBeGreaterThan(10);
    await browser.execute(async () => {
      await Promise.all(document.getAnimations()
        .map(animation => animation.finished.catch(() => undefined)));
    });
    await browser.saveScreenshot(path.join(root, 'observed.png'));
    await view!.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('editchain-history.stopLive'); });
  });
});
