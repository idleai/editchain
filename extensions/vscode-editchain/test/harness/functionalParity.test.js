// Deterministic browser functional-parity suite for the Rust/WASM GPU
// history renderer. This suite is an OFFSCREEN regression oracle: it compares
// the CPU/SVG harness page (test/harness/index.html) against the GPU harness
// page (test/harness/gpu.html), and is NOT the shipped VS Code UI — the
// shipped UI is ONE panel titled "EditChain History" (default command
// `editchain-history.open`) hosting the wgpu canvas; the SVG renderer is the
// deprecated internal fallback and this oracle's CPU reference side.
// Both harness pages run the SAME production renderer
// (media/main.js + main.css + fixture bridge), so this suite drives the SAME
// real interactions through both pages — Activity->Raw->Activity profile
// switching with request-filter gating, virtual scroll/paging on large /
// workUnitsDeep histories, find-in-chain submit + next/prev + off-cache jump +
// clear, legacy flat-list Search, row selection / keyboard roving / raw-JSON
// identity, and work-unit/bundle expansion with sub-op visibility — and
// asserts the GPU DOM meets the exact production semantics on top of the
// debug renderer contract (renderCount > 0, vertexCount > 0, wgpu canvas
// overlaid only over .graph-cell, no lastError).
//
// The GPU page must expose window.__editchainGpuDebug (dataReady, lastError,
// backend, snapshot, metrics, whenIdle); snapshot() reads the production
// rendered .row[data-row] rows and row DTOs, so DOM parity and snapshot
// identity are the same assertion.
//
// Run:  CHROME_PATH=... node --test test/harness/functionalParity.test.js
//
// The suite SKIPS (never fails) when Chrome or the built wasm assets are
// missing, so the generic `node --test test/harness/*.test.js` stays green in
// environments without GPU assets; the GPU CI job installs Chrome first and
// runs this file explicitly. It never requires hardware WebGPU — every run
// pins the deterministic SwiftShader WebGL baseline via ?backend=webgl.

'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const driver = require('./functionalDriver.js');

const PREREQS = driver.suitePrereqs();
const SKIP = PREREQS.ok ? false : PREREQS.reason;

let parityModPromise = null;
function parityMod() {
  if (!parityModPromise) parityModPromise = import('../../scripts/ui-gpu-preview.mjs');
  return parityModPromise;
}

let browser = null;
let server = null;
let baseUrl = '';

before(async () => {
  if (!PREREQS.ok) return;
  server = await driver.startServer(driver.EXT_ROOT);
  baseUrl = 'http://127.0.0.1:' + server.address().port;
  browser = await driver.launchBrowser();
});

after(async () => {
  if (browser) await browser.close();
  if (server) server.close();
});

/** Boot both pages on a scenario and return them (caller closes in finally). */
async function boot(scenario) {
  const pages = await driver.openPages(browser, baseUrl);
  await driver.bootScenario(pages.cpu, 'cpu', scenario);
  await driver.bootScenario(pages.gpu, 'gpu', scenario);
  return pages;
}

async function finish(pages) {
  await pages.cpu.close();
  await pages.gpu.close();
}

/** Settle the two rAF-driven pages sequentially. Background Chromium tabs
 * suspend rAF, so Promise.all would leave one page unable to reach idle. */
async function settleBoth(pages) {
  await driver.settle(pages.cpu, 'cpu');
  await driver.settle(pages.gpu, 'gpu');
}

/** Compare the two pages' rendered row DTOs with the runner's shared parity
 * comparator (absolute index, key, lane, above/below, transitions, total). */
async function assertRowParity(cpu, gpu, label) {
  const { compareParity } = await parityMod();
  const p = compareParity({ rows: cpu.rows, total: cpu.total }, { rows: gpu.rows, total: gpu.total });
  assert.equal(p.pass, true, (label || 'row parity') + ': ' + JSON.stringify({
    totalsEqual: p.totalsEqual,
    commonRows: p.commonRows,
    cpuRows: p.cpuRows,
    gpuRows: p.gpuRows,
    mismatches: p.mismatches.slice(0, 5),
    cpuOnly: p.cpuOnlyRows.slice(0, 3),
    gpuOnly: p.gpuOnlyRows.slice(0, 3),
  }));
  assert.equal(p.totalsEqual, true, (label || '') + ': totals must match');
}

/** The debug renderer contract on the GPU side: real backend, nonempty wgpu
 * geometry (renderCount/vertexCount > 0), ONE transparent canvas inside
 * #gpu-canvas-host (over the .graph-cell column, no canvases anywhere else),
 * and the #gpu-rows mirror holding exactly the snapshot's frame rows. */
function assertGpuHealthy(gpu) {
  assert.equal(gpu.gpu.present, true, 'window.__editchainGpuDebug must exist');
  assert.equal(gpu.gpu.dataReady, true, 'GPU dataReady must be true');
  assert.equal(gpu.gpu.lastError, null, 'GPU must not report a renderer error');
  assert.equal(gpu.gpu.backend, 'webgl', 'deterministic baseline backend');
  assert.equal(gpu.gpu.hasWhenIdle, true, 'whenIdle must be exposed');
  assert.equal(gpu.gpu.canvasCount, 1,
    'exactly one transparent wgpu canvas must exist inside #gpu-canvas-host');
  assert.equal(gpu.gpu.foreignCanvasCount, 0, 'no canvas may exist outside #gpu-canvas-host');
  assert.equal(gpu.gpu.mirrorRows, gpu.gpu.snapshotRows,
    '#gpu-rows mirror must hold exactly the snapshot frame rows');
  assert.ok(gpu.gpu.metrics && gpu.gpu.metrics.renderCount > 0,
    'debug metrics renderCount > 0, got ' + JSON.stringify(gpu.gpu.metrics));
  assert.ok(gpu.gpu.metrics && gpu.gpu.metrics.vertexCount > 0,
    'debug metrics vertexCount > 0, got ' + JSON.stringify(gpu.gpu.metrics));
}

test('functional parity prerequisites (Chrome + built GPU assets)',
  { skip: SKIP }, () => {});

test('GPU page runs the shared production scaffold with a healthy renderer (merge)',
  { skip: SKIP }, async () => {
    const pages = await boot('merge');
    try {
      const [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      // The production scaffold + main.js semantics are identical on both.
      assert.equal(cpu.profile, 'activity');
      assert.equal(gpu.profile, 'activity');
      assert.equal(gpu.header, true, 'GPU page must render the production table header');
      assert.equal(gpu.message, null, 'no message state on a healthy chain');
      assert.equal(gpu.placeholderCount, 0, 'settled view must have no placeholders');
      assert.ok(gpu.rowCount > 0, 'GPU page must render production rows');
      // The GPU debug contract is GPU-only.
      assert.equal(cpu.gpu.present, false, '__editchainGpuDebug is a GPU-page-only API');
      assertGpuHealthy(gpu);
      // snapshot() derives from the production DOM: same rows + total.
      assert.ok(gpu.gpu.snapshotRows > 0, 'snapshot must contain rendered frame rows');
      assert.equal(gpu.gpu.snapshotTotal, gpu.total,
        'snapshot total must equal production total');
      assert.equal(pages.errors.cpu.length, 0, 'CPU page errors: ' + pages.errors.cpu.join('; '));
      assert.equal(pages.errors.gpu.length, 0, 'GPU page errors: ' + pages.errors.gpu.join('; '));
    } finally {
      await finish(pages);
    }
  });

test('merge: identical rendered geometry and functional state on both pages',
  { skip: SKIP }, async () => {
    const pages = await boot('merge');
    try {
      const [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(cpu.total, 5);
      assert.equal(gpu.total, 5);
      await assertRowParity(cpu, gpu, 'merge');
      assert.deepEqual(gpu.rowKeys, cpu.rowKeys, 'rendered row key order must match');
      assert.equal(gpu.rowCount, cpu.rowCount, 'rendered row count must match');
      assertGpuHealthy(gpu);
    } finally {
      await finish(pages);
    }
  });

test('multigroup: virtual paging keeps group boundaries and totals identical',
  { skip: SKIP }, async () => {
    const pages = await boot('multigroup');
    try {
      const top = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(top[0].total, 600);
      assert.equal(top[1].total, 600);
      const groupAt = (page, abs) => page.evaluate((n) => {
        const el = document.querySelector('.row[data-row="' + n + '"]');
        if (!el) return null;
        const label = el.querySelector('.group-label');
        return {
          groupStart: el.classList.contains('row-group-start'),
          label: label ? (label.textContent || '').trim() : null,
        };
      }, abs);
      await driver.scrollToRow(pages.cpu, 100);
      const cpuBoundary = await groupAt(pages.cpu, 100);
      await driver.scrollToRow(pages.gpu, 100);
      const gpuBoundary = await groupAt(pages.gpu, 100);
      assert.deepEqual(cpuBoundary, gpuBoundary, 'group boundary DOM must match');
      assert.equal(gpuBoundary.groupStart, true,
        'GPU group chip must land on absolute row 100');
      assert.ok(gpuBoundary.label, 'group boundary must retain its production label');

      const [cpuMid, gpuMid] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      await assertRowParity(cpuMid, gpuMid, 'multigroup mid-window');
      assert.equal(gpuMid.rows.find((row) => row.index === 100)?.key,
        cpuMid.rows.find((row) => row.index === 100)?.key,
        'mid-window anchor key must match');

      await driver.scrollToBottom(pages.cpu);
      await driver.scrollToBottom(pages.gpu);
      const [cpuBottom, gpuBottom] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpuBottom.total, 600);
      assert.equal(cpuBottom.total, 600);
      assert.equal(gpuBottom.rows[gpuBottom.rows.length - 1].index, 599, 'bottom row reached');
      await assertRowParity(cpuBottom, gpuBottom, 'multigroup bottom window');
      assert.equal(gpuBottom.rows.at(-1)?.key, cpuBottom.rows.at(-1)?.key,
        'bottom anchor key must match');

      await driver.scrollToTop(pages.cpu);
      await driver.scrollToTop(pages.gpu);
      const [cpuTop, gpuTop] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpuTop.rows[0]?.index, 0, 'GPU returned to the top row');
      assert.equal(gpuTop.rows[0]?.key, cpuTop.rows[0]?.key,
        'top anchor key after returning must match');
      await assertRowParity(cpuTop, gpuTop, 'multigroup top return');
      assertGpuHealthy(gpuTop);
    } finally {
      await finish(pages);
    }
  });

test('workUnitsDeep: virtual scroll to bottom pages identically on both sides',
  { skip: SKIP }, async () => {
    const pages = await boot('workUnitsDeep');
    try {
      const [cpuTop, gpuTop] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.ok(cpuTop.total > 500, 'fixture must be large enough to page');
      assert.equal(gpuTop.total, cpuTop.total, 'activity projection total');

      await driver.scrollToBottom(pages.cpu);
      await driver.scrollToBottom(pages.gpu);
      const [cpuBottom, gpuBottom] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpuBottom.rows[gpuBottom.rows.length - 1].index,
        cpuTop.total - 1, 'deep bottom row reached');
      assert.equal(gpuBottom.placeholderCount, 0, 'no placeholders at the settled bottom');
      await assertRowParity(cpuBottom, gpuBottom, 'workUnitsDeep bottom window');
      assert.equal(gpuBottom.rows.at(-1)?.key, cpuBottom.rows.at(-1)?.key,
        'deep bottom anchor key must match');
      // Paging must have engaged on BOTH sides (GetWindow offsets past 0).
      const cpuPaged = Math.max(...cpuBottom.windowOffsets, 0);
      const gpuPaged = Math.max(...gpuBottom.windowOffsets, 0);
      assert.ok(cpuPaged > 0, 'CPU page must have issued deep GetWindow offsets, max=' + cpuPaged);
      assert.ok(gpuPaged > 0, 'GPU page must have issued deep GetWindow offsets, max=' + gpuPaged);

      await driver.scrollToTop(pages.cpu);
      await driver.scrollToTop(pages.gpu);
      const [cpuBack, gpuBack] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpuBack.rows[0]?.index, 0, 'GPU returned to the deep fixture top');
      assert.equal(gpuBack.rows[0]?.key, cpuBack.rows[0]?.key,
        'deep top anchor key after returning must match');
      await assertRowParity(cpuBack, gpuBack, 'workUnitsDeep top return');
      assertGpuHealthy(gpuBack);
    } finally {
      await finish(pages);
    }
  });

test('traced: Activity->Raw->Activity profile parity with request-filter gating',
  { skip: SKIP }, async () => {
    const pages = await boot('traced');
    try {
      const activity = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(activity[0].rowCount, 3, 'activity hides trace rows');
      assert.equal(activity[1].rowCount, 3);
      assert.deepEqual(activity[1].rowKeys, activity[0].rowKeys, 'activity row keys must match');
      assert.equal(activity[0].windowHideTraces.at(-1), true, 'activity requests hide_trace=true');
      assert.equal(activity[1].windowHideTraces.at(-1), true);

      await Promise.all([
        driver.clickProfile(pages.cpu, 'raw'),
        driver.clickProfile(pages.gpu, 'raw'),
      ]);
      await settleBoth(pages);
      const raw = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(raw[0].rowCount, 5, 'raw shows the full stream incl. trace rows');
      assert.equal(raw[1].rowCount, 5);
      assert.equal(raw[0].profile, 'raw');
      assert.equal(raw[1].profile, 'raw');
      assert.deepEqual(raw[1].rowKeys, raw[0].rowKeys, 'raw row keys must match');
      assert.ok(raw[1].rowKeys.some((k) => k === 'node:t:1' || k === 'node:t:3'),
        'raw exposes trace-metadata rows');
      assert.equal(raw[0].windowHideTraces.at(-1), false, 'raw requests hide_trace=false');
      assert.equal(raw[1].windowHideTraces.at(-1), false);
      await assertRowParity(raw[0], raw[1], 'raw profile');

      await Promise.all([
        driver.clickProfile(pages.cpu, 'activity'),
        driver.clickProfile(pages.gpu, 'activity'),
      ]);
      await settleBoth(pages);
      const back = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(back[1].rowCount, 3, 'back to activity regates trace rows');
      assert.deepEqual(back[1].rowKeys, back[0].rowKeys, 'activity row keys after return must match');
      assertGpuHealthy(back[1]);
    } finally {
      await finish(pages);
    }
  });

test('find-in-chain: submit + next/prev + off-cache jump + clear parity',
  { skip: SKIP }, async () => {
    const pages = await boot('workUnitsDeep');
    try {
      await Promise.all([
        driver.installHarnessSpies(pages.cpu),
        driver.installHarnessSpies(pages.gpu),
      ]);
      const before = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.ok(before[0].total > 500, 'fixture must contain off-cache matches');
      assert.equal(before[1].total, before[0].total);

      await Promise.all([
        driver.focusSearch(pages.cpu),
        driver.focusSearch(pages.gpu),
      ]);
      await Promise.all([
        driver.runSearch(pages.cpu, 'ops two without a prefix'),
        driver.runSearch(pages.gpu, 'ops two without a prefix'),
      ]);
      const waitSettled = (page) => driver.waitFor(page, () => {
        const t = (document.getElementById('search-counter')?.textContent || '').trim();
        return /^1 of \d+\+?$/.test(t) || t === '0 of 0';
      }, { timeout: driver.IDLE_TIMEOUT_MS });
      await Promise.all([waitSettled(pages.cpu), waitSettled(pages.gpu)]);

      let [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.ok(/^1 of \d+\+?$/.test(cpu.counter), 'cpu counter settled: ' + cpu.counter);
      assert.equal(gpu.counter, cpu.counter, 'counter text must match');
      assert.equal(gpu.findCurrentRow, cpu.findCurrentRow, 'first match must land on the same row');
      assert.equal(gpu.selectedKey, cpu.selectedKey, 'selected match key must match');
      assert.equal(gpu.total, before[0].total, 'in-place find must not replace the chain');
      assert.equal(gpu.header, true, 'the chain grid stays intact');

      // Next/prev through matches; each step must land on the same row.
      let steps = 0;
      for (; steps < 40; steps++) {
        const prevCpu = cpu.counter;
        const prevGpu = gpu.counter;
        const waitChanged = (page, prev) => driver.waitFor(page, (p) => {
          const t = (document.getElementById('search-counter')?.textContent || '').trim();
          return t !== p && t !== '' && !!document.querySelector('.row-find-current');
        }, { timeout: 60000, args: [prev] });
        await driver.clickNav(pages.cpu, 'next');
        await waitChanged(pages.cpu, prevCpu);
        await driver.clickNav(pages.gpu, 'next');
        await waitChanged(pages.gpu, prevGpu);
        [cpu, gpu] = await Promise.all([
          driver.readState(pages.cpu),
          driver.readState(pages.gpu),
        ]);
        assert.equal(gpu.counter, cpu.counter, 'counter mismatch after next #' + steps);
        assert.equal(gpu.findCurrentRow, cpu.findCurrentRow,
          'match row mismatch after next #' + steps);
        if (gpu.findCurrentRow > 500) break;
      }
      assert.ok(gpu.findCurrentRow > 500,
        'navigation must reach an OFF-CACHE match, stopped at row ' + gpu.findCurrentRow +
        ' after ' + steps + ' next(s)');
      const cpuMax = Math.max(...cpu.windowOffsets, 0);
      const gpuMax = Math.max(...gpu.windowOffsets, 0);
      assert.ok(cpuMax > 0 && gpuMax > 0,
        'off-cache jump must fetch deep windows on both sides (cpu=' + cpuMax + ' gpu=' + gpuMax + ')');

      // Wrap-around navigation still lands on identical rows.
      await driver.clickNav(pages.cpu, 'prev');
      await driver.clickNav(pages.gpu, 'prev');
      await settleBoth(pages);
      [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpu.findCurrentRow, cpu.findCurrentRow, 'prev must land on the same row');

      // Clear: the find session ends without replacing the chain.
      await Promise.all([
        driver.clearSearch(pages.cpu),
        driver.clearSearch(pages.gpu),
      ]);
      await settleBoth(pages);
      [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpu.counter, '', 'cleared find leaves the counter empty');
      assert.equal(gpu.counter, cpu.counter);
      assert.equal(gpu.findCurrentRow, null, 'cleared find removes the current marker');
      assert.equal(gpu.findCurrentRow, cpu.findCurrentRow);
      assert.equal(gpu.total, before[0].total, 'clearing never replaces the chain');
      assertGpuHealthy(gpu);
    } finally {
      await finish(pages);
    }
  });

test('legacy flat-list Search renders identically and clears back to history',
  { skip: SKIP }, async () => {
    const pages = await boot('badges');
    try {
      const before = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      await Promise.all([
        driver.installHarnessSpies(pages.cpu, { legacySearch: true }),
        driver.installHarnessSpies(pages.gpu, { legacySearch: true }),
      ]);
      await Promise.all([
        driver.focusSearch(pages.cpu),
        driver.focusSearch(pages.gpu),
      ]);
      await Promise.all([
        driver.runSearch(pages.cpu, 'the'),
        driver.runSearch(pages.gpu, 'the'),
      ]);
      const waitLegacy = (page) => driver.waitFor(page, () =>
        !!document.querySelector('.search-banner') &&
        document.querySelectorAll('#rows .row:not(.row-placeholder)').length > 0,
      { timeout: 60000 });
      await Promise.all([waitLegacy(pages.cpu), waitLegacy(pages.gpu)]);

      const [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpu.counter, '', 'legacy flat list hides the find counter');
      assert.equal(gpu.counter, cpu.counter);
      assert.ok(gpu.searchBanner && cpu.searchBanner,
        'both pages show the results banner');
      assert.equal(gpu.searchBanner, cpu.searchBanner, 'banner text must match');
      assert.ok(gpu.rowCount > 0, 'flat result list renders');
      assert.equal(gpu.rowCount, cpu.rowCount, 'result count must match');
      assert.deepEqual(gpu.rowKeys, cpu.rowKeys, 'flat result row keys must match');
      assert.equal(gpu.total, cpu.total, 'flat result totals must match');
      await assertRowParity(cpu, gpu, 'legacy Search results');

      // Clearing the input restores the full history from the flat list.
      await Promise.all([
        driver.clearSearch(pages.cpu),
        driver.clearSearch(pages.gpu),
      ]);
      await settleBoth(pages);
      const [cpuBack, gpuBack] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpuBack.counter, '', 'restored history has no find session');
      assert.equal(gpuBack.rowCount, before[1].rowCount, 'history restored on the GPU page');
      assert.deepEqual(gpuBack.rowKeys, cpuBack.rowKeys, 'restored row keys must match');
      assertGpuHealthy(gpuBack);
    } finally {
      await finish(pages);
    }
  });

test('row selection, keyboard roving, and raw-JSON identity parity',
  { skip: SKIP }, async () => {
    const pages = await boot('merge');
    try {
      await Promise.all([
        driver.installHarnessSpies(pages.cpu),
        driver.installHarnessSpies(pages.gpu),
      ]);
      await Promise.all([
        driver.clickRow(pages.cpu, 1),
        driver.clickRow(pages.gpu, 1),
      ]);
      await settleBoth(pages);
      let [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(cpu.selectedRow, 1);
      assert.equal(gpu.selectedRow, 1, 'inline selection row must match');
      assert.equal(gpu.selectedKey, cpu.selectedKey, 'selection key identity must match');
      assert.equal(gpu.selectedAria, 'true', 'aria-selected reflects the selection');

      // Keyboard roving focus moves through the SAME rendered rows.
      await Promise.all([
        driver.pressRowKey(pages.cpu, 'ArrowDown', 1),
        driver.pressRowKey(pages.gpu, 'ArrowDown', 1),
      ]);
      [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(cpu.activeRow, 2, 'roving focus moved on the CPU page');
      assert.equal(gpu.activeRow, cpu.activeRow, 'roving focus rows must match');

      // Enter activates the focused row and posts the SAME raw-JSON identity.
      await Promise.all([
        driver.pressRowKey(pages.cpu, 'Enter', 2),
        driver.pressRowKey(pages.gpu, 'Enter', 2),
      ]);
      [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(gpu.selectedRow, 2, 'Enter selects the activated row');
      assert.ok(gpu.openJsonLog.length > 0, 'raw JSON envelope posted on the GPU page');
      assert.deepEqual(gpu.openJsonLog, cpu.openJsonLog,
        'raw-JSON identity (op_id / git_oid / repository) must match exactly');
    } finally {
      await finish(pages);
    }
  });

test('work-unit/bundle expansion reveals identical sub-ops on both pages',
  { skip: SKIP }, async () => {
    const pages = await boot('workUnits');
    try {
      const before = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(before[0].rowCount, before[1].rowCount, 'activity row count must match');
      const expandableRow = await pages.cpu.evaluate(() => {
        const el = document.querySelector('.row-expandable');
        return el ? Number(el.getAttribute('data-row')) : null;
      });
      assert.ok(expandableRow !== null, 'scenario must contain an expandable bundle row');
      assert.equal(before[0].rows.filter((r) => r.isSubop).length, 0,
        'collapsed bundles hide sub-ops');
      assert.ok(before[0].rowKeys.some((k) => k.includes('wu:req1')),
        'scenario must render work-unit rows on the CPU page');
      assert.ok(before[1].rowKeys.some((k) => k.includes('wu:req1')),
        'scenario must render work-unit rows on the GPU page');

      await Promise.all([
        driver.clickFirstExpandable(pages.cpu),
        driver.clickFirstExpandable(pages.gpu),
      ]);
      await settleBoth(pages);
      const [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      const expandedAria = (page) => page.evaluate((n) =>
        document.querySelector('.row[data-row="' + n + '"]')?.getAttribute('aria-expanded'),
      expandableRow);
      const [cpuAria, gpuAria] = await Promise.all([
        expandedAria(pages.cpu),
        expandedAria(pages.gpu),
      ]);
      assert.equal(gpuAria, 'true', 'bundle row exposes aria-expanded=true');
      assert.equal(gpuAria, cpuAria);
      const cpuSubops = cpu.rows.filter((r) => r.isSubop);
      const gpuSubops = gpu.rows.filter((r) => r.isSubop);
      assert.ok(gpuSubops.length > 0, 'expanding reveals sub-op rows');
      assert.equal(gpuSubops.length, cpuSubops.length, 'sub-op count must match');
      assert.deepEqual(
        gpuSubops.map((r) => r.key),
        cpuSubops.map((r) => r.key),
        'sub-op row identity must match');
      assert.equal(gpu.total, cpu.total, 'expansion never changes the total');
      assertGpuHealthy(gpu);
    } finally {
      await finish(pages);
    }
  });

test('expected empty state renders identically on both pages',
  { skip: SKIP }, async () => {
    const pages = await boot('empty');
    try {
      const [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(cpu.rowCount, 0);
      assert.equal(gpu.rowCount, 0, 'empty state renders no rows');
      assert.equal(gpu.total, 0);
      assert.equal(gpu.total, cpu.total);
      assert.deepEqual(gpu.message, { error: false, text: 'No history found in this workspace' },
        'GPU page must show the production empty-state message');
      assert.deepEqual(gpu.message, cpu.message, 'empty-state message parity');
      assert.equal(gpu.gpu.dataReady, true);
      assert.equal(gpu.gpu.lastError, null);
    } finally {
      await finish(pages);
    }
  });

test('expected open-error state renders identically on both pages',
  { skip: SKIP }, async () => {
    const pages = await boot('error');
    try {
      const [cpu, gpu] = await Promise.all([
        driver.readState(pages.cpu),
        driver.readState(pages.gpu),
      ]);
      assert.equal(cpu.rowCount, 0);
      assert.equal(gpu.rowCount, 0, 'error state renders no rows');
      assert.deepEqual(gpu.message,
        { error: true, text: 'Failed to open history: service unavailable' },
        'GPU page must show the production open-error message');
      assert.deepEqual(gpu.message, cpu.message, 'open-error message parity');
      // The RENDERER did not fail: the chain open failed. lastError stays null.
      assert.equal(gpu.gpu.dataReady, true);
      assert.equal(gpu.gpu.lastError, null);
    } finally {
      await finish(pages);
    }
  });
