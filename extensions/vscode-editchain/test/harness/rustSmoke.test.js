// Rust-only adapter smoke test (Browser Slice 3A runtime completion).
//
// This suite drives the REAL Rust/WASM history adapter end-to-end in headless
// Chrome (deterministic SwiftShader WebGL baseline) through the new fixture
// page test/harness/rust.html. Unlike gpu.html (the offscreen parity oracle
// where production media/main.js renders the DOM and bootstrap.js overlays the
// GPU canvas), rust.html loads NEITHER main.js NOR gpu-preview/bootstrap.js:
// media/rust-history/loader.js initializes the generated wasm-bindgen module
// and calls the Rust shell's startHistoryView(), which acquires
// window.acquireVsCodeApi() (capital C — the fixture bridge supplies it),
// installs the host-message listener, renders real .row[data-row][data-key]
// DOM with grid ARIA into #rows, creates the single canvas under
// #gpu-canvas-host, and mirrors row markers into #gpu-rows.
//
// Run:  CHROME_PATH=... node --test test/harness/rustSmoke.test.js
//
// Like functionalParity.test.js, the runtime tests SKIP (never fail) when
// Chrome or the built rust-history assets are missing, so the generic
// `npm run test:harness` suite stays green without GPU build artifacts. The
// static source test always runs.

'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const driver = require('./functionalDriver.js');

const HARNESS_DIR = __dirname;
const RUST_HTML = fs.readFileSync(path.join(HARNESS_DIR, 'rust.html'), 'utf8');

function rustPrereqs() {
  if (!fs.existsSync(driver.CHROME)) {
    return { ok: false, reason: 'Chrome not found at ' + driver.CHROME + ' (set CHROME_PATH)' };
  }
  for (const rel of [
    'media/rust-history/loader.js',
    'media/rust-history/pkg/editchain_gpu_preview.js',
    'media/rust-history/pkg/editchain_gpu_preview_bg.wasm',
  ]) {
    if (!fs.existsSync(path.join(driver.EXT_ROOT, rel))) {
      return { ok: false, reason: 'missing rust-history asset ' + rel + ' (run npm run build:gpu first)' };
    }
  }
  return { ok: true, reason: '' };
}

const PREREQS = rustPrereqs();
const SKIP = PREREQS.ok ? false : PREREQS.reason;

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

// --- static source contract ------------------------------------------------

test('rust.html shares the scaffold but loads neither main.js nor the gpu-preview bootstrap', () => {
  // Shared fixture scripts + theme tokens, exactly like gpu.html.
  assert.match(RUST_HTML, /<script src="\.\/fixtures\.js"><\/script>/,
    'loads fixtures.js with the exact relative src used by the other harness pages');
  assert.match(RUST_HTML, /<script src="\.\/fixtureBridge\.js"><\/script>/,
    'loads fixtureBridge.js with the exact relative src used by the other harness pages');
  assert.match(RUST_HTML, /<link rel="stylesheet" href="\.\.\/\.\.\/media\/main\.css">/,
    'links the shared production media/main.css');
  assert.match(RUST_HTML, /<link rel="stylesheet" href="\.\.\/\.\.\/media\/gpu-preview\/gpu-preview\.css">/,
    'links the shared GPU overlay stylesheet (positioning + status chrome only)');
  assert.match(RUST_HTML, /--vscode-editor-background/, 'ships the VS Code theme tokens');
  // Production scaffold IDs shared with index.html/gpu.html.
  for (const id of [
    'controls', 'profile-control', 'profile-activity', 'profile-raw',
    'search-control', 'search', 'search-counter', 'search-prev', 'search-next',
    'layout', 'rows', 'gpu-canvas-host', 'status-live',
  ]) {
    assert.ok(RUST_HTML.includes('id="' + id + '"'), 'rust.html missing production id #' + id);
  }
  // The Rust-only loader module is the page's bootstrap.
  assert.match(
    RUST_HTML,
    /<script type="module" src="\.\.\/\.\.\/media\/rust-history\/loader\.js"><\/script>/,
    'loads media/rust-history/loader.js as the ES module bootstrap');
  // Neither production renderer nor the overlay bootstrap may load here.
  assert.doesNotMatch(RUST_HTML, /<script[^>]*src="[^"]*main\.js"/,
    'no script tag may reference main.js');
  assert.doesNotMatch(RUST_HTML, /<script[^>]*src="[^"]*gpu-preview\/bootstrap\.js"/,
    'no script tag may reference the gpu-preview bootstrap');
  // Search controls are enabled (the Rust shell owns find-in-chain).
  assert.doesNotMatch(RUST_HTML, /<input id="search"[^>]*disabled/, '#search is enabled');
  assert.doesNotMatch(RUST_HTML, /data-unsupported="3b"/, 'no 3B marker on the search control');
  assert.doesNotMatch(RUST_HTML, /search-3b-note/, 'no 3B note element');
  assert.doesNotMatch(RUST_HTML, /aria-disabled/, 'search buttons are not aria-disabled');
});

// --- runtime helpers -------------------------------------------------------

/** Open rust.html, wait for the wasm-started marker, install a message
 * listener, start the scenario, and settle until dataReady + idle. */
async function openRustPage(scenario, viewport) {
  const page = await browser.newPage();
  await page.setViewport(viewport || { width: 1440, height: 900 });
  const errors = { page: [], console: [] };
  page.on('pageerror', (error) => errors.page.push(error.message));
  page.on('console', (message) => {
    if (message.type() === 'error') errors.console.push(message.text());
  });
  await page.goto(baseUrl + '/test/harness/rust.html?backend=webgl', {
    waitUntil: 'domcontentloaded',
    timeout: driver.BOOT_TIMEOUT_MS,
  });
  await driver.waitFor(page, () =>
    document.body.dataset.rustWasm === 'started' || document.body.dataset.rustWasm === 'error',
  { timeout: driver.BOOT_TIMEOUT_MS });
  const marker = await page.evaluate(() => ({
    state: document.body.dataset.rustWasm,
    error: document.body.dataset.rustWasmError,
    loaderError: (window.__editchainRustLoader || {}).error || null,
  }));
  assert.equal(marker.state, 'started',
    'wasm must start cleanly (marker error: ' + (marker.error || marker.loaderError) + ')');
  // The test's own listener must observe the synchronous Open/Ready window
  // messages and the correlated (numeric-id) host replies.
  await page.evaluate((name) => {
    window.__editchainSeenMessages = [];
    window.addEventListener('message', (event) => {
      const data = event.data;
      if (data && typeof data === 'object' && data.id !== undefined && data.body !== undefined) {
        window.__editchainSeenMessages.push({ id: data.id });
      }
    });
    window.__editchainSetScenario(name);
    window.__editchainStart();
  }, scenario);
  await driver.waitFor(page, () => window.__editchainDataReady === true,
    { timeout: driver.BOOT_TIMEOUT_MS });
  await page.bringToFront();
  await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
  return { page, errors };
}

/** Read the rust-only runtime state snapshot for assertions. */
async function rustState(page) {
  return page.evaluate(() => {
    const gpu = window.__editchainGpuDebug;
    const rows = Array.from(document.querySelectorAll(
      '#rows .row[data-row][data-key]:not(.row-placeholder)'));
    const grid = document.querySelector('#rows .tbl-grid');
    const log = window.__editchainRequestLog || [];
    const windowRequests = log
      .map((request) => request && request.GetWindow)
      .filter(Boolean)
      .map((getWindow) => ({
        offset: getWindow.offset,
        hideTrace: !!(getWindow.filter && getWindow.filter.hide_trace),
      }));
    return {
      wasmReady: window.__editchainWasmReady === true,
      rendererReady: window.__editchainRendererReady === true,
      dataReady: window.__editchainDataReady === true,
      loader: gpu ? gpu.loader : null,
      profile: typeof gpu.profile === 'function' ? gpu.profile() : null,
      total: typeof gpu.total === 'function' ? gpu.total() : -1,
      lastError: gpu ? gpu.lastError : null,
      backend: typeof gpu.backend === 'function' ? gpu.backend() : null,
      metrics: typeof gpu.metrics === 'function' ? gpu.metrics() : null,
      laneXAll: typeof gpu.laneXAll === 'function' ? gpu.laneXAll() : null,
      graphState: typeof gpu.graphState === 'function' ? gpu.graphState() : null,
      snapshot: typeof gpu.snapshot === 'function' ? gpu.snapshot() : null,
      gridRole: grid ? grid.getAttribute('role') : null,
      ariaRowcount: grid ? grid.getAttribute('aria-rowcount') : null,
      rows: rows.map((row) => ({
        index: Number(row.getAttribute('data-row')),
        key: row.getAttribute('data-key'),
        role: row.getAttribute('role'),
        gridcellCount: row.querySelectorAll('[role="gridcell"]').length,
      })),
      placeholderCount: document.querySelectorAll('#rows .row-placeholder').length,
      canvasHostCount: document.querySelectorAll('#gpu-canvas-host canvas').length,
      foreignCanvasCount: document.querySelectorAll('canvas:not(#gpu-canvas-host canvas)').length,
      mirrorRows: document.querySelectorAll('#gpu-rows [data-row][data-key]').length,
      rowsWidth: document.getElementById('rows') ? document.getElementById('rows').clientWidth : -1,
      seenIds: (window.__editchainSeenMessages || []).map((entry) => entry.id),
      windowRequests,
      mainJsScripts: Array.from(document.scripts)
        .filter((script) => (script.src || '').includes('media/main.js')).length,
      bootstrapScripts: Array.from(document.scripts)
        .filter((script) => (script.src || '').includes('gpu-preview/bootstrap.js')).length,
      vscodeShared: typeof window.__editchainVscode !== 'undefined',
    };
  });
}

function assertNoErrors(errors, label) {
  assert.deepEqual(errors.page, [], (label || 'page') + ' errors: ' + JSON.stringify(errors.page));
  assert.deepEqual(errors.console, [],
    (label || 'console') + ' errors: ' + JSON.stringify(errors.console));
}

function assertGpuHealthy(state, label) {
  assert.equal(state.loader, 'rust-history', label + ': __editchainGpuDebug must be the rust loader');
  assert.equal(state.wasmReady, true, label + ': __editchainWasmReady');
  assert.equal(state.rendererReady, true, label + ': renderer ready');
  assert.equal(state.dataReady, true, label + ': dataReady');
  assert.equal(state.lastError, null, label + ': no renderer error');
  assert.equal(state.backend, 'webgl', label + ': deterministic SwiftShader WebGL baseline');
  assert.equal(state.canvasHostCount, 1, label + ': exactly one canvas inside #gpu-canvas-host');
  assert.equal(state.foreignCanvasCount, 0, label + ': no foreign canvases');
  assert.ok(state.metrics && state.metrics.renderCount > 0,
    label + ': renderCount > 0, got ' + JSON.stringify(state.metrics));
  assert.ok(state.metrics && state.metrics.vertexCount > 0,
    label + ': vertexCount > 0, got ' + JSON.stringify(state.metrics));
}

async function settleRust(page) {
  await page.bringToFront();
  await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms),
    driver.IDLE_TIMEOUT_MS);
}

async function pressSearchKey(page, key, shiftKey) {
  await page.evaluate(({ key, shiftKey }) => {
    const input = document.getElementById('search');
    input.focus();
    input.dispatchEvent(new KeyboardEvent('keydown', {
      key,
      shiftKey,
      bubbles: true,
      cancelable: true,
    }));
  }, { key, shiftKey: !!shiftKey });
}

async function saveParityScreenshot(page, name) {
  const traceDir = path.join(driver.EXT_ROOT, 'trace', 'rust-parity');
  fs.mkdirSync(traceDir, { recursive: true });
  const shot = path.join(traceDir, name + '.png');
  await page.screenshot({ path: shot });
  assert.ok(fs.existsSync(shot) && fs.statSync(shot).size > 0,
    name + ' screenshot written');
}

// --- runtime smoke ---------------------------------------------------------

test('rust-only adapter boots in headless Chrome and renders (merge)', { skip: SKIP }, async () => {
  const { page, errors } = await openRustPage('merge');
  try {
    // The test's listener must catch the synchronous Open/Ready messages and
    // the correlated window replies, and the shell must process them.
    let state = await rustState(page);
    assert.ok(state.seenIds.includes('open'), 'listener saw the Open message');
    assert.ok(state.seenIds.includes('ready'), 'listener saw the Ready message');
    assert.ok(state.seenIds.some((id) => typeof id === 'number'),
      'listener saw correlated (numeric-id) window replies: ' + JSON.stringify(state.seenIds));
    assert.equal(state.dataReady, true, 'Open/Ready + window replies drove dataReady');

    // dataReady and total.
    assert.equal(state.total, 5, 'merge fixture total');
    assert.equal(state.snapshot.total, 5, 'snapshot total');

    // Visible semantic rows with grid ARIA.
    assert.equal(state.rows.length, 5, 'five real rows rendered');
    assert.ok(state.rows.length > 0 && state.rows.length <= 30,
      'bounded row count on a small window');
    assert.equal(state.gridRole, 'grid', '.tbl-grid carries role="grid"');
    assert.equal(Number(state.ariaRowcount), 5, 'aria-rowcount matches the total');
    for (const row of state.rows) {
      assert.equal(row.role, 'row', 'row ' + row.index + ' carries role="row"');
      assert.ok(row.key && row.key.startsWith('git:'), 'row ' + row.index + ' has a data-key');
      assert.ok(row.gridcellCount >= 1, 'row ' + row.index + ' has grid cells');
    }
    assert.equal(state.mirrorRows, state.rows.length, '#gpu-rows mirror matches frame rows');

    // Renderer health (one canvas, no foreign canvases, webgl, geometry).
    assertGpuHealthy(state, 'boot');

    // Activity -> Raw -> Activity through real button clicks; the correlated
    // GetWindow filter must flip hide_trace with the profile.
    assert.equal(state.profile, 'activity', 'boots in Activity');
    assert.ok(state.windowRequests.length >= 1, 'boot issued a GetWindow');
    assert.equal(state.windowRequests.at(-1).hideTrace, true, 'Activity sends hide_trace=true');
    await page.bringToFront();
    await page.click('#profile-raw');
    await driver.waitFor(page, () => window.__editchainGpuDebug.profile() === 'raw',
      { timeout: driver.IDLE_TIMEOUT_MS });
    await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
    state = await rustState(page);
    assert.equal(state.profile, 'raw', 'profile switched to Raw');
    assert.equal(state.windowRequests.at(-1).hideTrace, false, 'Raw sends hide_trace=false');
    assert.equal(await page.$eval('#profile-raw', (el) => el.getAttribute('aria-pressed')), 'true',
      'Raw button aria-pressed');
    assert.equal(await page.$eval('#profile-activity', (el) => el.getAttribute('aria-pressed')), 'false',
      'Activity button aria-pressed cleared');
    await page.click('#profile-activity');
    await driver.waitFor(page, () => window.__editchainGpuDebug.profile() === 'activity',
      { timeout: driver.IDLE_TIMEOUT_MS });
    await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
    state = await rustState(page);
    assert.equal(state.profile, 'activity', 'profile switched back to Activity');
    assert.equal(state.windowRequests.at(-1).hideTrace, true, 'Activity restores hide_trace=true');

    // laneXAll is invariant across graph column / available-width changes.
    const laneXBefore = state.laneXAll;
    assert.ok(Array.isArray(laneXBefore) && laneXBefore.length >= 2,
      'laneXAll exposes fixed lane centers, got ' + JSON.stringify(laneXBefore));
    assert.deepEqual(laneXBefore.slice(0, 2), [14.76, 29.52],
      'laneXAll carries the fixed Pulse lane pitch (14.76 CSS px)');
    const rowsWidthBefore = state.rowsWidth;
    await page.setViewport({ width: 900, height: 900 });
    await page.evaluate(() => {
      const rows = document.getElementById('rows');
      rows.scrollTop += 1;
    });
    await driver.waitFor(page, (previousWidth) => {
      const rows = document.getElementById('rows');
      return rows.clientWidth !== previousWidth;
    }, { timeout: driver.IDLE_TIMEOUT_MS, args: [rowsWidthBefore] });
    await page.evaluate(() => {
      const rows = document.getElementById('rows');
      rows.scrollTop = 0;
    });
    await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
    state = await rustState(page);
    assert.notEqual(state.rowsWidth, rowsWidthBefore,
      'the available width actually changed');
    assert.deepEqual(state.laneXAll, laneXBefore,
      'laneXAll is unchanged after changing the graph host/available width');
    assertGpuHealthy(state, 'after resize');

    // Runtime proof that production main.js and the gpu-preview bootstrap are
    // absent (they would have set __editchainVscode / loaded their scripts).
    assert.equal(state.mainJsScripts, 0, 'no media/main.js script at runtime');
    assert.equal(state.bootstrapScripts, 0, 'no gpu-preview/bootstrap.js script at runtime');
    assert.equal(state.vscodeShared, false, '__editchainVscode is never set without main.js/bootstrap');
    assertNoErrors(errors, 'rust smoke (merge)');

    // One deterministic smoke screenshot.
    const traceDir = path.join(driver.EXT_ROOT, 'trace');
    fs.mkdirSync(traceDir, { recursive: true });
    const shot = path.join(traceDir, 'rust-smoke.png');
    await page.screenshot({ path: shot });
    assert.ok(fs.existsSync(shot) && fs.statSync(shot).size > 0, 'screenshot written');
  } finally {
    await page.close();
  }
});

test('find-in-chain, row selection, and disclosure interactions settle in real DOM',
  { skip: SKIP }, async () => {
    const { page, errors } = await openRustPage('workUnitsDeep');
    try {
      await page.evaluate(() => {
        window.__editchainSeenMessages = [];
        window.__editchainOpenJsonLog = [];
      });
      // The parity facade hooks are installed by the Rust shell.
      assert.equal(
        await page.evaluate(() => typeof window.__editchainGetTotal),
        'function',
        '__editchainGetTotal parity hook');
      assert.equal(
        await page.evaluate(() => typeof window.__editchainGetProfile),
        'function',
        '__editchainGetProfile parity hook');
      assert.equal(
        await page.evaluate(() => typeof window.__editchainRowAt),
        'function',
        '__editchainRowAt parity hook');

      // Find-in-chain: submit, settle to "1 of N+", navigate next through an
      // OFF-CACHE match (deep window fetch), then clear without replacing the
      // chain (same surface as the functional parity suite).
      await driver.installHarnessSpies(page);
      await driver.focusSearch(page);
      await driver.runSearch(page, 'ops two without a prefix');
      await driver.waitFor(page, () => {
        const t = (document.getElementById('search-counter')?.textContent || '').trim();
        return /^1 of \d+\+?$/.test(t) || t === '0 of 0';
      }, { timeout: driver.IDLE_TIMEOUT_MS });
      let state = await driver.readState(page);
      assert.ok(/^1 of \d+\+?$/.test(state.counter), 'find counter settled: ' + state.counter);
      assert.ok(state.findCurrentRow !== null, 'a match row is marked');
      assert.equal(state.total, 2592, 'in-place find never replaces the chain');
      assert.equal(state.header, true, 'the chain grid stays intact');

      let guard = 0;
      while (state.findCurrentRow <= 500 && guard < 40) {
        await driver.clickNav(page, 'next');
        await page.bringToFront();
        await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
        state = await driver.readState(page);
        guard += 1;
      }
      assert.ok(state.findCurrentRow > 500,
        'navigation reached an off-cache match at row ' + state.findCurrentRow);
      assert.ok(Math.max(...state.windowOffsets, 0) > 0,
        'off-cache jumps fetch deep GetWindow offsets');

      await driver.clearSearch(page);
      await page.bringToFront();
      await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
      state = await driver.readState(page);
      assert.equal(state.counter, '', 'cleared find empties the counter');
      assert.equal(state.findCurrentRow, null, 'cleared find removes the current marker');
      assert.equal(state.total, 2592, 'clearing never replaces the chain');

      // Row selection + roving keyboard + raw-JSON identity (real controls).
      await driver.clickRow(page, 1);
      state = await driver.readState(page);
      assert.equal(state.selectedRow, 1, 'click selects the row');
      assert.equal(state.selectedAria, 'true', 'aria-selected reflects the selection');
      assert.ok(state.selectedKey, 'selection carries a node key');
      await driver.pressRowKey(page, 'ArrowDown', 1);
      state = await driver.readState(page);
      assert.equal(state.activeRow, 2, 'ArrowDown moves the roving focus');
      // A plain (non-expandable, non-sub-op) row: Enter posts raw JSON.
      const plainRow = await page.evaluate(() => {
        const el = document.querySelector(
          '#rows .row:not(.row-expandable):not(.row-subop)');
        return el ? Number(el.getAttribute('data-row')) : null;
      });
      assert.ok(plainRow !== null, 'scenario contains a plain non-expandable row');
      await driver.pressRowKey(page, 'Enter', plainRow);
      state = await driver.readState(page);
      assert.ok(state.openJsonLog.length > 0, 'Enter posts a raw-JSON envelope');
      const envelope = state.openJsonLog[0];
      assert.equal(envelope.type, 'openJson', 'envelope type');
      assert.ok(envelope.op_id || envelope.git_oid, 'envelope carries an identity');

      // Chevron disclosure: exactly one tab stop and aria-expanded toggle.
      const expandable = await page.evaluate(() => {
        const el = document.querySelector('.row-expandable');
        return el ? Number(el.getAttribute('data-row')) : null;
      });
      assert.ok(expandable !== null, 'scenario contains an expandable row');
      await driver.clickFirstExpandable(page);
      await page.bringToFront();
      await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
      const aria = await page.evaluate((n) =>
        document.querySelector('.row[data-row="' + n + '"]')?.getAttribute('aria-expanded'),
      expandable);
      assert.equal(aria, 'true', 'chevron toggles aria-expanded');
      assert.ok(
        await page.evaluate(() => document.querySelectorAll('#rows .row-subop').length) > 0,
        'expanding reveals sub-op rows');
      assert.equal(
        await page.evaluate(() =>
          Array.from(document.querySelectorAll('#rows .row'))
            .filter((row) => row.getAttribute('tabindex') === '0').length),
        1,
        'exactly one tabbable row');

      // Read-only find facade reflects the session.
      const findState = await page.evaluate(() => window.__editchainGpuDebug.findState());
      assert.equal(findState.active, false, 'find facade reports the cleared session');
      assertNoErrors(errors, 'rust smoke (interactions)');
    } finally {
      await page.close();
    }
  });

test('search keyboard, pending/zero/error ARIA, and legacy Search are functional',
  { skip: SKIP }, async () => {
    const current = await openRustPage('workUnitsDeep');
    try {
      const { page, errors } = current;
      await driver.installHarnessSpies(page);
      await page.evaluate(() => {
        window.__editchainHoldFind = { taken: false };
      });
      await driver.focusSearch(page);
      await driver.runSearch(page, 'ops two without a prefix');
      await driver.waitFor(page, () =>
        document.getElementById('search-counter')?.classList
          .contains('search-counter-pending') === true,
      { timeout: driver.IDLE_TIMEOUT_MS });
      let counter = await page.evaluate(() => {
        const el = document.getElementById('search-counter');
        return {
          busy: el.getAttribute('aria-busy'),
          label: el.getAttribute('aria-label'),
          text: (el.textContent || '').trim(),
          prevHidden: document.getElementById('search-prev').hidden,
          nextDisabled: document.getElementById('search-next').disabled,
        };
      });
      assert.deepEqual(counter, {
        busy: 'true',
        label: 'Searching…',
        text: '',
        prevHidden: true,
        nextDisabled: true,
      }, 'pending search exposes the compact busy state');
      await page.evaluate(() => window.__editchainHoldFind.release());
      await driver.waitFor(page, () =>
        /^1 of \d+\+?$/.test(
          (document.getElementById('search-counter')?.textContent || '').trim()),
      { timeout: driver.IDLE_TIMEOUT_MS });
      await settleRust(page);
      let state = await driver.readState(page);
      const firstCounter = state.counter;
      const firstMatch = state.findCurrentRow;
      assert.equal(state.counterBusy, '', 'settled search clears aria-busy');
      await saveParityScreenshot(page, 'search-current');

      await pressSearchKey(page, 'Enter');
      await driver.waitFor(page, (previous) =>
        (document.getElementById('search-counter')?.textContent || '').trim() !== previous,
      { timeout: driver.IDLE_TIMEOUT_MS, args: [firstCounter] });
      await settleRust(page);
      state = await driver.readState(page);
      assert.notEqual(state.findCurrentRow, firstMatch, 'same-query Enter advances');

      await pressSearchKey(page, 'Enter', true);
      await driver.waitFor(page, (expected) =>
        (document.getElementById('search-counter')?.textContent || '').trim() === expected,
      { timeout: driver.IDLE_TIMEOUT_MS, args: [firstCounter] });
      await settleRust(page);
      state = await driver.readState(page);
      assert.equal(state.findCurrentRow, firstMatch, 'Shift+Enter moves back');

      await pressSearchKey(page, 'ArrowDown');
      await settleRust(page);
      state = await driver.readState(page);
      assert.notEqual(state.findCurrentRow, firstMatch, 'ArrowDown navigates a settled query');

      await page.evaluate(() => {
        const input = document.getElementById('search');
        input.value += ' edited';
        input.dispatchEvent(new Event('input', { bubbles: true }));
      });
      counter = await page.evaluate(() => ({
        prevHidden: document.getElementById('search-prev').hidden,
        nextDisabled: document.getElementById('search-next').disabled,
      }));
      assert.deepEqual(counter, { prevHidden: true, nextDisabled: true },
        'edited text disables navigation until submitted');

      await page.evaluate(() => {
        window.__editchainFindError = 'find service error';
        const input = document.getElementById('search');
        input.value = 'error query';
      });
      await pressSearchKey(page, 'Enter');
      await driver.waitFor(page, () =>
        document.getElementById('search-counter')?.classList
          .contains('search-counter-error') === true,
      { timeout: driver.IDLE_TIMEOUT_MS });
      counter = await page.evaluate(() => {
        const el = document.getElementById('search-counter');
        return {
          text: (el.textContent || '').trim(),
          busy: el.getAttribute('aria-busy'),
          label: el.getAttribute('aria-label'),
          title: el.getAttribute('title'),
          rows: document.querySelectorAll('#rows .row:not(.row-placeholder)').length,
          header: !!document.querySelector('#rows .tbl-header'),
        };
      });
      assert.equal(counter.text, 'error');
      assert.equal(counter.busy, null);
      assert.equal(counter.label, 'Find failed: find service error');
      assert.equal(counter.title, 'find service error');
      assert.ok(counter.rows > 0 && counter.header, 'find error leaves the chain intact');
      await saveParityScreenshot(page, 'search-error');

      await page.evaluate(() => {
        window.__editchainFindError = null;
        const input = document.getElementById('search');
        input.value = 'definitely absent 8b1c730f';
      });
      await pressSearchKey(page, 'Enter');
      await driver.waitFor(page, () =>
        (document.getElementById('search-counter')?.textContent || '').trim() === '0 of 0',
      { timeout: driver.IDLE_TIMEOUT_MS });
      counter = await page.evaluate(() => {
        const el = document.getElementById('search-counter');
        return {
          zero: el.classList.contains('search-counter-zero'),
          busy: el.getAttribute('aria-busy'),
          prevHidden: document.getElementById('search-prev').hidden,
        };
      });
      assert.deepEqual(counter, { zero: true, busy: null, prevHidden: true });
      await pressSearchKey(page, 'Escape');
      await settleRust(page);
      assert.equal((await driver.readState(page)).counter, '', 'Escape clears find');
      assertNoErrors(errors, 'rust search keyboard/ARIA');
    } finally {
      await current.page.close();
    }

    const legacy = await openRustPage('badges');
    try {
      const { page, errors } = legacy;
      const historyTotal = (await driver.readState(page)).total;
      await driver.installHarnessSpies(page, { legacySearch: true });
      await driver.runSearch(page, 'the');
      await driver.waitFor(page, () =>
        !!document.querySelector('.search-banner') &&
        document.querySelectorAll('#rows .row:not(.row-placeholder)').length > 0,
      { timeout: driver.IDLE_TIMEOUT_MS });
      let state = await driver.readState(page);
      assert.match(state.searchBanner, /^\d+ results? for "the"$/);
      assert.equal(state.counter, '', 'legacy results hide the find counter');
      await pressSearchKey(page, 'ArrowDown');
      state = await driver.readState(page);
      assert.equal(state.activeRow, 0, 'legacy ArrowDown focuses the first result');
      await pressSearchKey(page, 'ArrowUp');
      state = await driver.readState(page);
      assert.equal(state.activeRow, state.total - 1, 'legacy ArrowUp focuses the last result');
      await saveParityScreenshot(page, 'legacy-search');
      await driver.clearSearch(page);
      await settleRust(page);
      state = await driver.readState(page);
      assert.equal(state.searchBanner, '', 'clearing legacy search restores history');
      assert.equal(state.total, historyTotal, 'history total is restored');
      assertNoErrors(errors, 'rust legacy search');
    } finally {
      await legacy.page.close();
    }
  });

test('row keyboard disclosure, double-click identity, and divider drag remain coherent',
  { skip: SKIP }, async () => {
    const combined = await openRustPage('workUnits');
    try {
      const { page, errors } = combined;
      const expandable = await page.evaluate(() => {
        const row = document.querySelector('.row-expandable');
        return row ? Number(row.getAttribute('data-row')) : null;
      });
      assert.ok(expandable !== null, 'workUnits contains an expandable row');
      await driver.pressRowKey(page, 'ArrowRight', expandable);
      await settleRust(page);
      let disclosure = await page.evaluate((abs) => ({
        expanded: document.querySelector('.row[data-row="' + abs + '"]')
          ?.getAttribute('aria-expanded'),
        active: Number(document.activeElement?.closest('.row')?.getAttribute('data-row')),
        keys: Array.from(document.querySelectorAll('.row-subop'))
          .map((row) => row.getAttribute('data-key')),
      }), expandable);
      assert.equal(disclosure.expanded, 'true');
      assert.equal(disclosure.active, expandable, 'focus survives expansion rebuild');
      assert.ok(disclosure.keys.length > 0 && disclosure.keys.every(Boolean),
        'expanded sub-ops have stable keys');
      const firstKeys = disclosure.keys;

      await driver.pressRowKey(page, 'ArrowLeft', expandable);
      await settleRust(page);
      assert.equal(await page.$eval('.row[data-row="' + expandable + '"]',
        (row) => row.getAttribute('aria-expanded')), 'false');
      await driver.pressRowKey(page, ' ', expandable);
      await settleRust(page);
      disclosure = await page.evaluate(() => ({
        keys: Array.from(document.querySelectorAll('.row-subop'))
          .map((row) => row.getAttribute('data-key')),
        tabStops: Array.from(document.querySelectorAll('#rows .row'))
          .filter((row) => row.getAttribute('tabindex') === '0').length,
      }));
      assert.deepEqual(disclosure.keys, firstKeys, 're-expansion preserves sub-op identity');
      assert.equal(disclosure.tabStops, 1, 'rebuild preserves one roving tab stop');
      await driver.pressRowKey(page, 'Enter', expandable);
      await settleRust(page);
      assert.equal(await page.$eval('.row[data-row="' + expandable + '"]',
        (row) => row.getAttribute('aria-expanded')), 'false');
      assertNoErrors(errors, 'rust disclosure keyboard');
    } finally {
      await combined.page.close();
    }

    const merge = await openRustPage('merge');
    try {
      const { page, errors } = merge;
      await driver.installHarnessSpies(page);
      await driver.pressRowKey(page, 'End', 1);
      let state = await driver.readState(page);
      assert.equal(state.activeRow, 4, 'End focuses the last rendered row');
      await driver.pressRowKey(page, 'Home', 4);
      state = await driver.readState(page);
      assert.equal(state.activeRow, 0, 'Home focuses the first rendered row');
      await driver.pressRowKey(page, 'ArrowUp', 0);
      state = await driver.readState(page);
      assert.equal(state.activeRow, 0, 'ArrowUp stops at the normal-view boundary');

      const expected = await page.evaluate(() => window.__editchainRowAt(0));
      assert.equal(typeof expected, 'object', '__editchainRowAt returns an object facade');
      await page.evaluate(() => {
        const row = document.querySelector('.row[data-row="0"]');
        row.dispatchEvent(new MouseEvent('dblclick', {
          bubbles: true,
          cancelable: true,
          detail: 2,
        }));
      });
      state = await driver.readState(page);
      assert.deepEqual(state.openJsonLog.at(-1), {
        type: 'openJson',
        op_id: null,
        git_oid: expected.git_oid,
        repository: expected.repository,
      }, 'double-click posts the exact git identity');

      const before = await rustState(page);
      const handle = await page.$('.col-resize-handle[data-col="graph"]');
      assert.ok(handle, 'graph resize handle exists');
      const box = await handle.boundingBox();
      assert.ok(box, 'graph resize handle has a bounding box');
      const widthBefore = await page.$eval('.table-wrap', (el) =>
        Number.parseFloat(getComputedStyle(el).getPropertyValue('--graph-w')));
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await page.mouse.down();
      await page.mouse.move(box.x + box.width / 2 + 80, box.y + box.height / 2, { steps: 4 });
      await page.mouse.up();
      await driver.waitFor(page, (previous) => {
        const wrap = document.querySelector('.table-wrap');
        return Number.parseFloat(getComputedStyle(wrap).getPropertyValue('--graph-w')) > previous;
      }, { timeout: driver.IDLE_TIMEOUT_MS, args: [widthBefore] });
      await settleRust(page);
      const after = await rustState(page);
      assert.deepEqual(after.laneXAll, before.laneXAll,
        'divider drag changes width without moving lanes');
      assert.ok(after.metrics.renderCount > before.metrics.renderCount,
        'divider drag renders a fresh GPU frame');
      await saveParityScreenshot(page, 'selected-graph-wide');
      await page.setViewport({ width: 420, height: 900 });
      await driver.waitFor(page, () => document.getElementById('rows').clientWidth <= 420,
        { timeout: driver.IDLE_TIMEOUT_MS });
      const narrowHandle = await page.$('.col-resize-handle[data-col="graph"]');
      assert.ok(narrowHandle, 'graph resize handle survives viewport resize');
      const narrowBox = await narrowHandle.boundingBox();
      assert.ok(narrowBox, 'narrow graph handle has a bounding box');
      await page.mouse.move(
        narrowBox.x + narrowBox.width / 2,
        narrowBox.y + narrowBox.height / 2);
      await page.mouse.down();
      await page.mouse.move(narrowBox.x - 500, narrowBox.y + narrowBox.height / 2,
        { steps: 4 });
      await page.mouse.up();
      await driver.waitFor(page, () =>
        !!document.querySelector('.tbl-header .th.graph .visually-hidden'),
      { timeout: driver.IDLE_TIMEOUT_MS });
      await settleRust(page);
      assert.deepEqual((await rustState(page)).laneXAll, before.laneXAll,
        'narrow viewport still preserves lane centers');
      await saveParityScreenshot(page, 'graph-narrow');
      assertNoErrors(errors, 'rust row/divider');
    } finally {
      await merge.page.close();
    }
  });

test('warning, empty, and open-error terminal states remain accessible',
  { skip: SKIP }, async () => {
    const scenarios = [
      ['warned', '.open-warning', '6131 blob payload(s) missing from the durable store'],
      ['empty', '.view-message', 'No history found in this workspace'],
      ['error', '.view-message.error', 'Failed to open history: service unavailable'],
    ];
    for (const [scenario, selector, text] of scenarios) {
      const current = await openRustPage(scenario);
      try {
        const actual = await current.page.$eval(selector, (element) =>
          (element.textContent || '').trim());
        assert.equal(actual, text, scenario + ' terminal/status text');
        assertNoErrors(current.errors, 'rust ' + scenario);
        await saveParityScreenshot(current.page, scenario);
      } finally {
        await current.page.close();
      }
    }
  });

test('deep scroll pages the large window with bounded offsets and settles', { skip: SKIP }, async () => {
  const { page, errors } = await openRustPage('large');
  try {
    let state = await rustState(page);
    assert.equal(state.total, 600, 'large fixture total');
    assert.ok(state.rows.length >= 400 && state.rows.length <= 500,
      'first window renders the viewport±BUFFER window, got ' + state.rows.length);
    assert.ok(state.windowRequests.length >= 1, 'boot issued a GetWindow');
    assert.equal(state.windowRequests[0].offset, 0, 'boot fetches from offset 0');
    const renderCountBefore = state.metrics.renderCount;

    // Scroll to the bottom through the real #rows scroll handler; paging must
    // stay bounded (small set of window offsets, rows never exceed the
    // viewport ± BUFFER cap).
    await page.bringToFront();
    for (let i = 0; i < 80; i++) {
      await page.evaluate(() => {
        const rows = document.getElementById('rows');
        rows.scrollTop = rows.scrollHeight;
      });
      await page.waitForFunction(() => {
        const rows = document.getElementById('rows');
        const rendered = Array.from(document.querySelectorAll(
          '#rows .row:not(.row-placeholder)[data-row]'));
        const last = rendered.at(-1);
        return rows.scrollTop >= rows.scrollHeight - rows.clientHeight - 1 &&
          document.querySelectorAll('.row-placeholder').length === 0 &&
          last !== undefined && Number(last.getAttribute('data-row')) === 599;
      }, { timeout: driver.IDLE_TIMEOUT_MS, polling: 50 });
    }
    await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
    state = await rustState(page);

    const offsets = state.windowRequests.map((request) => request.offset);
    assert.ok(offsets.length >= 2, 'deep scroll paged beyond the first window: ' + JSON.stringify(offsets));
    assert.ok(new Set(offsets).size >= 2,
      'deep scroll advanced to a new window offset: ' + JSON.stringify(offsets));
    for (let i = 1; i < offsets.length; i += 1) {
      assert.ok(offsets[i] >= offsets[i - 1], 'window offsets never move backwards');
      assert.ok(offsets[i] <= 599, 'offset stays inside the dataset');
    }
    assert.ok(offsets.at(-1) > 0, 'the final window is past offset 0');
    assert.ok(state.rows.length <= 900, 'rendered rows stay bounded, got ' + state.rows.length);
    assert.ok(state.rows.length >= 400, 'deep window still renders a real window');
    assert.equal(state.rows.at(-1).index, 599, 'last dataset row is rendered');
    assert.equal(state.placeholderCount, 0, 'no placeholders after settle');
    assert.ok(state.metrics.renderCount > renderCountBefore,
      'deep scroll rendered additional frames');
    assertGpuHealthy(state, 'after deep scroll');
    await saveParityScreenshot(page, 'deep-scroll');
    assertNoErrors(errors, 'rust smoke (large)');
  } finally {
    await page.close();
  }
});
