import fs from 'node:fs';
import path from 'node:path';

describe('Incremental graph motion in VS Code', () => {
  it('grows a fork and a merge continuously while existing lanes keep their spacing', async () => {
    const extension = path.resolve(__dirname, '../..');
    const html = fs.readFileSync(path.join(extension, 'test/harness/rust.html'), 'utf8')
      .replace('<title>EditChain History — Rust-only Harness</title>', '<title>EditChain Graph Animation Test</title>');
    // Use the actual shipped CSS/WASM in a VS Code webview. Only the protocol
    // host is replaced, so fork/merge timing is reproducible independently of
    // the provider's choice of ancestry or the viewport's position in history.
    await browser.executeWorkbench(async (vscode, input) => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('notifications.clearAll');
      const root = vscode.Uri.file(input.extension);
      const panel = vscode.window.createWebviewPanel('editchain-growth-test', 'EditChain Graph Animation Test',
        vscode.ViewColumn.One, { enableScripts: true, localResourceRoots: [root] });
      panel.webview.html = input.html.replace(/(src|href)="(\.[^"]+)"/g, (_match, attr, relative) =>
        `${attr}="${panel.webview.asWebviewUri(vscode.Uri.joinPath(root, 'test/harness', relative))}"`);
    }, { extension, html });
    const workbench = await browser.getWorkbench();
    const view = await workbench.getWebviewByTitle('EditChain Graph Animation Test');
    await view.open();
    await browser.waitUntil(() => browser.execute(() => document.body.dataset.rustWasm === 'started'), { timeout: 30000 });
    await browser.execute(() => {
      const app = window as any;
      app.__editchainSetScenario('liveGraph');
      app.__editchainStart();
      document.getElementById('harness-status')!.textContent = 'One operation at a time · fixed lane spacing';
      app.__growthFrames = [];
      const sample = () => {
        for (const part of document.querySelectorAll('path.graph-live-edge')) {
          const offset = parseFloat(getComputedStyle(part).strokeDashoffset);
          if (offset > -0.99 && offset < -0.01) app.__growthFrames.push({
            at: performance.now(), key: part.getAttribute('data-graph-key'), offset,
            row: part.closest('.row')?.getAttribute('data-key'),
          });
        }
        requestAnimationFrame(sample);
      };
      sample();
    });
    await browser.execute(async () => { await (window as any).__editchainRendererDebug.whenIdle(10000); });
    await browser.pause(700);
    for (const phase of ['fork', 'merge']) {
      await browser.execute(phase => {
        const app = window as any;
        const head = app.__editchainFixture.rows[0];
        const status = phase === 'fork' ? 'Fork: grow a new branch from the shared parent' : 'Merge: grow both connections into the new endpoint';
        document.getElementById('harness-status')!.textContent = status;
        app.__editchainLiveDelta([{ ...head, node_key: 'live:' + phase, op_id: 'live:' + phase,
          timestamp_ms: head.timestamp_ms + 1000, summary: status,
          parents: phase === 'fork' ? ['live:row:1'] : ['live:row:0', 'live:fork'] }]);
      }, phase);
      await browser.waitUntil(() => browser.execute(() => (window as any).__editchainLiveResult !== null), { timeout: 10000 });
      await browser.execute(async () => {
        await Promise.all(document.getAnimations().map(animation => animation.finished.catch(() => undefined)));
      });
      const evidence = await browser.execute(() => ({
        total: window.__editchainGetTotal?.(), result: (window as any).__editchainLiveResult,
        frames: (window as any).__growthFrames.splice(0),
        centers: (window as any).__editchainRendererDebug.laneXAll(),
        originalX: document.querySelector('.row[data-key="live:row:0"] .graphDot')?.getAttribute('cx'),
      }));
      expect(evidence.result.error).toBe(null);
      expect(evidence.total).toBe(phase === 'fork' ? 41 : 42);
      expect(evidence.originalX).toBe('14.76');
      expect(evidence.frames.length).toBeGreaterThan(2);
      expect(evidence.centers.slice(1).every((x: number, index: number) =>
        Math.abs(x - evidence.centers[index] - 14.76) < 0.01)).toBe(true);
      fs.writeFileSync(path.resolve(`trace/graph-growth-${phase}.json`), JSON.stringify(evidence, null, 2));
      await browser.saveScreenshot(path.resolve(`trace/graph-growth-${phase}.png`));
      await browser.pause(1100);
    }
    await view.close();
  });
});
