#!/usr/bin/env node
'use strict';

// Frontend-only benchmark: real production WASM + DOM, deterministic native
// responses. Native source ingestion, graph projection and disk I/O are excluded.
// Optional argument: a directory containing an earlier renderer JS/WASM/main.css.
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const driver = require('../test/harness/functionalDriver.js');
const { installNativeWindowFixture } = require('../test/harness/nativeWindowFixture.js');

async function main() {
  const assets = process.argv[2] && path.resolve(process.argv[2]);
  const server = await driver.startServer(driver.EXT_ROOT);
  const browser = await driver.launchBrowser();
  try {
    const page = await browser.newPage();
    await page.setViewport({ width: 1440, height: 900 });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    if (assets) {
      await page.setRequestInterception(true);
      page.on('request', request => {
        const name = path.basename(new URL(request.url()).pathname);
        if (['editchain_history_renderer.js', 'editchain_history_renderer_bg.wasm', 'main.css'].includes(name)) {
          void request.respond({ status: 200,
            contentType: name.endsWith('.wasm') ? 'application/wasm' : name.endsWith('.css') ? 'text/css' : 'text/javascript',
            body: fs.readFileSync(path.join(assets, name)) });
        } else void request.continue();
      });
    }
    await page.goto(`http://127.0.0.1:${server.address().port}/test/harness/rust.html?backend=svg`);
    await driver.waitFor(page, () => document.body.dataset.rustWasm === 'started');
    await page.evaluate(() => { window.__editchainSetScenario('large'); window.__editchainStart(); });
    await driver.waitFor(page, () => window.__editchainDataReady === true);
    await page.evaluate(() => window.__editchainRendererDebug.whenIdle(10000));
    await installNativeWindowFixture(page);
    await driver.waitFor(page, () => document.querySelector('.row[data-key="native:0"]'));
    await page.evaluate(() => window.__editchainRendererDebug.whenIdle(10000));
    await page.evaluate(() => {
      const metrics = window.__updateMetrics = { rectangleReads: 0, rowMutations: 0 };
      const rect = Element.prototype.getBoundingClientRect;
      Element.prototype.getBoundingClientRect = function () {
        if (this.classList.contains('row')) metrics.rectangleReads++;
        return rect.call(this);
      };
      new MutationObserver(records => {
        metrics.rowMutations += records.filter(record => {
          const element = record.target.nodeType === Node.ELEMENT_NODE ? record.target : record.target.parentElement;
          return element?.closest('.row[data-row]');
        }).length;
      }).observe(document.getElementById('rows'), { subtree: true, childList: true, attributes: true, characterData: true });
    });
    const cdp = await page.createCDPSession();
    await cdp.send('Performance.enable');
    const results = [];
    for (const scenario of ['burst', 'offscreen', 'prepend', 'disclosure']) {
      await page.evaluate(() => {
        const state = window.__nativeWindow;
        state.requests = []; state.sentRows = 0; state.reusedRows = 0; state.bytes = 0; state.latencies = [];
        window.__updateMetrics.rectangleReads = 0; window.__updateMetrics.rowMutations = 0;
      });
      const before = await cdp.send('Performance.getMetrics');
      const result = await page.evaluate(async scenario => {
        const state = window.__nativeWindow;
        const started = performance.now();
        const initial = state.revision;
        if (scenario === 'burst') {
          await Promise.all(Array.from({ length: 20 }, () => state.update()));
        } else if (scenario === 'offscreen') {
          for (let i = 0; i < 20; i++) await state.update(state.rows.length - 1);
        } else if (scenario === 'prepend') {
          for (let i = 0; i < 20; i++) await state.prepend();
        } else {
          for (let i = 0; i < 10; i++) {
            const done = new Promise(resolve => state.waiters.push({ revision: state.revision + 1, started: performance.now(), resolve }));
            document.querySelector('.row[data-key="native:0"] .task-chevron').click();
            await done;
          }
        }
        await window.__editchainRendererDebug.whenIdle(10000);
        const elapsed = performance.now() - started;
        const sorted = [...state.latencies].sort((a, b) => a - b);
        return { scenario, revisions: state.revision - initial, finalRevision: state.revision, settled: state.settled,
          mountedRows: document.querySelectorAll('.row[data-row]').length,
          contentRows: state.sentRows, reusedRows: state.reusedRows,
          responseBytes: state.bytes, requests: state.requests.length,
          ...window.__updateMetrics, elapsedMs: elapsed,
          ackP50Ms: sorted[Math.floor(sorted.length * 0.5)], ackP95Ms: sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))] };
      }, scenario);
      const after = await cdp.send('Performance.getMetrics');
      for (const name of ['LayoutDuration', 'RecalcStyleDuration', 'ScriptDuration', 'TaskDuration']) {
        result[name + 'Ms'] = 1000 * (after.metrics.find(metric => metric.name === name).value - before.metrics.find(metric => metric.name === name).value);
      }
      assert.equal(result.finalRevision, result.settled);
      assert.equal(result.revisions, scenario === 'disclosure' ? 10 : 20);
      results.push(result);
      // Each scenario includes overlapping updates, but must not charge the
      // preceding scenario's remaining CSS animation to the next measurement.
      await page.evaluate(() => Promise.all(document.getAnimations()
        .filter(animation => animation.effect?.getTiming().iterations !== Infinity)
        .map(animation => animation.finished.catch(() => {}))));
    }
    assert.deepEqual(errors, []);
    console.log(JSON.stringify({ assets: assets || 'working tree', browser: await browser.version(),
      viewport: '1440x900', fixtureRows: 2000,
      scope: 'Synthetic frontend; synchronous native responses; wall times include frame scheduling. No native service or checkpoint timing.',
      results }, null, 2));
  } finally {
    await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
