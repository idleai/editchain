// Rust-only adapter smoke test (Browser Slice 3A runtime completion).
//
// This suite drives the REAL Rust/WASM history adapter end-to-end in headless
// Chrome through the fixture page test/harness/rust.html. rust.html loads
// NEITHER media/main.js NOR media/gpu-preview/bootstrap.js (both retired
// oracle files): media/rust-history/loader.js initializes the generated
// wasm-bindgen module and calls the Rust shell's startHistoryView(), which
// acquires window.acquireVsCodeApi() (capital C — the fixture bridge supplies
// it), installs the host-message listener, renders real .row[data-row][data-key]
// DOM with grid ARIA into #rows, paints every hydrated row's own aria-hidden
// svg.graph-row-fragment inside its .graph-cell. No canvas surface is created.
//
// Run:  CHROME_PATH=... node --test test/harness/rustSmoke.test.js
//
// The runtime tests SKIP (never fail) when Chrome or the built rust-history
// assets are missing, so the generic `npm run test:harness` suite stays green
// without renderer build artifacts. The static source test always runs.

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
    'media/rust-history/pkg/editchain_history_renderer.js',
    'media/rust-history/pkg/editchain_history_renderer_bg.wasm',
  ]) {
    if (!fs.existsSync(path.join(driver.EXT_ROOT, rel))) {
      return { ok: false, reason: 'missing rust-history asset ' + rel + ' (run npm run build:renderer first)' };
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
  // Shared fixture scripts + theme tokens, exactly like the production
  // webview scaffold.
  assert.match(RUST_HTML, /<script src="\.\/fixtures\.js"><\/script>/,
    'loads fixtures.js with the exact relative src used by the other harness pages');
  assert.match(RUST_HTML, /<script src="\.\/fixtureBridge\.js"><\/script>/,
    'loads fixtureBridge.js with the exact relative src used by the other harness pages');
  assert.match(RUST_HTML, /<link rel="stylesheet" href="\.\.\/\.\.\/media\/main\.css">/,
    'links the shared production media/main.css');
  assert.doesNotMatch(RUST_HTML, /gpu-preview\.css/,
    'the removed GPU scaffold stylesheet is not loaded');
  assert.match(RUST_HTML, /--vscode-editor-background/, 'ships the VS Code theme tokens');
  // Production scaffold IDs shared with the shipped webview scaffold.
  for (const id of [
    'controls', 'search-control', 'search', 'search-counter', 'search-prev', 'search-next',
    'layout', 'rows', 'status-live',
  ]) {
    assert.ok(RUST_HTML.includes('id="' + id + '"'), 'rust.html missing production id #' + id);
  }
  assert.doesNotMatch(RUST_HTML, /id="gpu-toolbar"|id="gpu-backend"|id="gpu-status"/,
    'the harness has no visible renderer-status toolbar');
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
  // Chrome may leave a newly-created page backgrounded after the preceding
  // fixture page closes. Keep module/WASM startup on the foreground page so
  // renderer scheduling cannot be throttled before the boot marker appears.
  await page.bringToFront();
  await page.setViewport(viewport || { width: 1440, height: 900 });
  const errors = { page: [], console: [] };
  page.on('pageerror', (error) => errors.page.push(error.message));
  page.on('console', (message) => {
    if (message.type() === 'error') errors.console.push(message.text());
  });
  await page.goto(baseUrl + '/test/harness/rust.html?backend=svg', {
    waitUntil: 'domcontentloaded',
    timeout: driver.BOOT_TIMEOUT_MS,
  });
  try {
    await driver.waitFor(page, () =>
      document.body.dataset.rustWasm === 'started' || document.body.dataset.rustWasm === 'error',
    { timeout: driver.BOOT_TIMEOUT_MS });
  } catch (error) {
    const startup = await page.evaluate(() => ({
      documentState: document.readyState,
      visibility: document.visibilityState,
      marker: document.body.dataset.rustWasm || null,
      loader: window.__editchainRustLoader || null,
      resources: performance.getEntriesByType('resource').map((entry) => entry.name),
    }));
    throw new Error('Rust WASM startup stalled for fixture ' + scenario + ': ' +
      JSON.stringify(startup), { cause: error });
  }
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
  await page.evaluate((ms) => window.__editchainRendererDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
  return { page, errors };
}

/** Read the rust-only runtime state snapshot for assertions. */
async function rustState(page) {
  return page.evaluate(() => {
    const gpu = window.__editchainRendererDebug;
    const hydrated = Array.from(document.querySelectorAll(
      '#rows .row[data-row][data-key]:not(.row-placeholder)'));
    const grid = document.querySelector('#rows .tbl-grid');
    const log = window.__editchainRequestLog || [];
    const windowRequests = log
      .map((request) => request && request.GetWindow)
      .filter(Boolean)
      .map((getWindow) => ({
        offset: getWindow.offset,
        keys: Object.keys(getWindow).sort(),
      }));
    // Per-row SVG graph fragments: the production contract is exactly one
    // aria-hidden svg.graph-row-fragment per hydrated row, whose geometry
    // centre sits on the row's vertical centre. Marker shapes (dots/bundle
    // capsules) define the visual centre; rows without a marker (sub-ops)
    // fall back to the row-aligned SVG box itself.
    let fragmentCount = 0;
    const fragmentIssues = [];
    let maxAlignDelta = 0;
    const alignExamples = [];
    const rows = hydrated.map((row) => {
      const fragments = row.querySelectorAll('svg.graph-row-fragment');
      const fragment = fragments[0] || null;
      const fragmentOk = fragments.length === 1 && !!fragment &&
        fragment.getAttribute('aria-hidden') === 'true';
      if (fragmentOk) {
        fragmentCount++;
      } else if (fragmentIssues.length < 5) {
        fragmentIssues.push({
          row: Number(row.getAttribute('data-row')),
          count: fragments.length,
          ariaHidden: fragment ? fragment.getAttribute('aria-hidden') : null,
        });
      }
      let alignDelta = null;
      if (fragmentOk) {
        const rowBox = row.getBoundingClientRect();
        const svgBox = fragment.getBoundingClientRect();
        const shapes = Array.from(fragment.querySelectorAll(
          '.graphDot, .graphBundleCapsule'));
        let minY = Infinity;
        let maxY = -Infinity;
        let any = false;
        for (const shape of shapes) {
          const b = shape.getBoundingClientRect();
          if (b.width <= 0 && b.height <= 0) continue;
          any = true;
          if (b.top < minY) minY = b.top;
          if (b.bottom > maxY) maxY = b.bottom;
        }
        const center = any ? (minY + maxY) / 2 : svgBox.top + svgBox.height / 2;
        alignDelta = Math.abs(center - (rowBox.top + rowBox.height / 2));
        if (alignDelta > maxAlignDelta) maxAlignDelta = alignDelta;
        if (alignDelta > 1 && alignExamples.length < 5) {
          alignExamples.push({
            row: Number(row.getAttribute('data-row')),
            delta: Math.round(alignDelta * 100) / 100,
          });
        }
      }
      return {
        index: Number(row.getAttribute('data-row')),
        key: row.getAttribute('data-key'),
        role: row.getAttribute('role'),
        gridcellCount: row.querySelectorAll('[role="gridcell"]').length,
        fragmentCount: fragments.length,
        fragmentAriaHidden: fragment ? fragment.getAttribute('aria-hidden') : null,
        alignDelta: alignDelta === null ? null : Math.round(alignDelta * 100) / 100,
      };
    });
    return {
      wasmReady: window.__editchainWasmReady === true,
      rendererReady: window.__editchainRendererReady === true,
      dataReady: window.__editchainDataReady === true,
      loader: gpu ? gpu.loader : null,
      total: typeof gpu.total === 'function' ? gpu.total() : -1,
      lastError: gpu ? gpu.lastError : null,
      backend: typeof gpu.backend === 'function' ? gpu.backend() : null,
      metrics: typeof gpu.metrics === 'function' ? gpu.metrics() : null,
      laneXAll: typeof gpu.laneXAll === 'function' ? gpu.laneXAll() : null,
      graphState: typeof gpu.graphState === 'function' ? gpu.graphState() : null,
      snapshot: typeof gpu.snapshot === 'function' ? gpu.snapshot() : null,
      gridRole: grid ? grid.getAttribute('role') : null,
      ariaRowcount: grid ? grid.getAttribute('aria-rowcount') : null,
      rows,
      fragmentCount,
      fragmentMissing: hydrated.length - fragmentCount,
      fragmentIssues,
      maxAlignDelta: Math.round(maxAlignDelta * 100) / 100,
      alignExamples,
      placeholderCount: document.querySelectorAll('#rows .row-placeholder').length,
      canvasCount: document.querySelectorAll('canvas').length,
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

test('live insertion and revision preserve selection and the scroll anchor without background flashes', { skip: SKIP }, async () => {
  const { page, errors } = await openRustPage('large');
  try {
    await page.evaluate(() => {
      const rows = document.getElementById('rows');
      rows.scrollTop = 1707;
      rows.dispatchEvent(new Event('scroll'));
    });
    await settleRust(page);
    const before = await page.evaluate(() => {
      const anchor = document.querySelector('.row[data-row="50"]');
      const selected = document.querySelector('.row[data-row="52"]');
      window.__liveAnchorNode = anchor;
      window.__liveAnchorCell = anchor.querySelector('.text-cell');
      window.__liveSelectedNode = selected;
      selected.click();
      selected.focus({ preventScroll: true });
      return { anchor: anchor.dataset.continuity, y: anchor.getBoundingClientRect().y,
        selected: selected.dataset.continuity, scroll: document.getElementById('rows').scrollTop };
    });
    await page.evaluate(({ selected }) => {
      const fixture = window.__editchainFixture;
      const existing = fixture.rows.map(row => ({ ...row, continuity_key: row.node_key,
        ...(row.node_key === selected ? { node_key: row.node_key + ':revision', summary: 'Updated during the same Codex turn' } : {}) }));
      fixture.rows = Array.from({ length: 3 }, (_, i) => ({ ...existing[0],
        node_key: 'live-new-' + i, continuity_key: 'live-new-' + i, summary: 'New live edit ' + i,
      })).concat(existing);
      window.__editchainLiveUpdate();
    }, before);
    await driver.waitFor(page, () => window.__editchainLiveResult !== null);
    const after = await page.evaluate(({ anchor }) => {
      const anchorRow = [...document.querySelectorAll('.row[data-continuity]')].find(row => row.dataset.continuity === anchor);
      const selectedRow = document.querySelector('.row[aria-selected="true"]');
      return { result: window.__editchainLiveResult, selected: selectedRow?.dataset.continuity,
        focused: document.activeElement?.closest('.row')?.dataset.continuity,
        key: selectedRow?.dataset.key, y: anchorRow.getBoundingClientRect().y,
        reusedAnchor: anchorRow === window.__liveAnchorNode,
        reusedContent: anchorRow.querySelector('.text-cell') === window.__liveAnchorCell,
        reusedSelection: selectedRow === window.__liveSelectedNode,
        scroll: document.getElementById('rows').scrollTop,
        animations: [...document.querySelectorAll('.row-live-changed')].map(row => getComputedStyle(row).animationName),
        selectedAnimation: getComputedStyle(selectedRow).animationName };
    }, before);
    assert.equal(after.result.error, null);
    assert.equal(after.reusedAnchor, true);
    assert.equal(after.reusedContent, true);
    assert.equal(after.reusedSelection, true);
    assert.equal(after.selected, before.selected);
    assert.equal(after.focused, before.selected);
    assert.equal(after.key, before.selected + ':revision');
    assert.equal(after.scroll, before.scroll + 102);
    assert.ok(Math.abs(after.y - before.y) < 1, 'the same logical anchor stays at the same screen position');
    assert.ok(after.animations.every(name => name === 'none' || name === 'ec-live-move'));
    assert.equal(after.selectedAnimation, 'none');
    await page.emulateMediaFeatures([{ name: 'prefers-reduced-motion', value: 'reduce' }]);
    const reduced = await page.evaluate(() => [...document.querySelectorAll('.row-live-moved')].every(row => getComputedStyle(row).animationName === 'none'));
    assert.equal(reduced, true);
    assert.deepEqual(errors.page, []);
    assert.deepEqual(errors.console, []);
  } finally { await page.close(); }
});

test('one-operation forks and merges grow connections and retain SVG animation clocks across another delta', { skip: SKIP }, async () => {
  const { page, errors } = await openRustPage('liveGraph');
  try {
    const before = await page.evaluate(() => {
      window.__graphOriginal = document.querySelector('.row[data-key="live:row:0"] .graphDot');
      if (!window.__graphOriginal) throw new Error(JSON.stringify({
        row: window.__editchainRowAt(0), html: document.getElementById('rows').innerHTML.slice(0, 3000),
      }));
      const head = window.__editchainFixture.rows[0];
      window.__editchainLiveDelta([{ ...head, node_key: 'live:fork', op_id: 'live:fork',
        timestamp_ms: head.timestamp_ms + 1000, summary: 'Fork from the shared parent',
        parents: ['live:row:1'] }]);
      return { x: window.__graphOriginal.getAttribute('cx') };
    });
    await driver.waitFor(page, () => window.__editchainLiveResult !== null);
    const fork = await page.evaluate(() => {
      window.__graphSegments = Array.from(document.querySelectorAll('.graph-live-edge'));
      window.__graphAnimations = window.__graphSegments.map(part => part.getAnimations()[0]);
      for (const animation of window.__graphAnimations) {
        const timing = animation.effect.getTiming();
        animation.pause();
        animation.currentTime = timing.delay + timing.duration / 2;
      }
      return { result: window.__editchainLiveResult, total: window.__editchainGetTotal(),
        paths: window.__graphSegments.filter(part => part.tagName === 'path').length,
        offsets: window.__graphSegments.map(part => parseFloat(getComputedStyle(part).strokeDashoffset)),
        originalConnected: window.__graphOriginal.isConnected,
        originalX: window.__graphOriginal.getAttribute('cx'),
        laneCenters: window.__editchainRendererDebug.laneXAll() };
    });
    assert.equal(fork.result.error, null);
    assert.equal(fork.total, 41, '+1 operation, no snapshot replacement');
    assert.ok(fork.paths >= 2, 'the fork has curved connection halves');
    assert.ok(fork.offsets.length >= 4);
    assert.ok(fork.offsets.every(offset => Math.abs(offset + 0.5) < 0.01), 'each SVG stroke is partially drawn halfway through its animation');
    assert.equal(fork.originalConnected, true);
    assert.equal(fork.originalX, before.x);
    assert.deepEqual(fork.laneCenters.slice(0, 2), [14.76, 29.52]);
    await page.evaluate(() => {
      const fork = window.__editchainFixture.rows.find(row => row.node_key === 'live:fork');
      window.__editchainLiveDelta([{ ...fork, summary: 'Fork content revised while its connection grows' }]);
    });
    await driver.waitFor(page, () => window.__editchainLiveResult !== null);
    const retained = await page.evaluate(() => window.__graphSegments.map((part, index) => ({
      connected: part.isConnected,
      clock: part.getAnimations()[0] === window.__graphAnimations[index],
      offset: parseFloat(getComputedStyle(part).strokeDashoffset),
    })));
    assert.ok(retained.every(part => part.connected && part.clock && Math.abs(part.offset + 0.5) < 0.01),
      'revising row content preserves every connection element and its in-flight clock');
    await page.evaluate(() => {
      window.__editchainLiveResult = null;
      window.dispatchEvent(new MessageEvent('message', { data: window.__editchainLastDelta }));
    });
    await driver.waitFor(page, () => window.__editchainLiveResult !== null);
    assert.equal(await page.evaluate(() => window.__graphSegments.every((part, index) =>
      part.getAnimations()[0] === window.__graphAnimations[index])), true, 'replay does not restart growth');
    await page.evaluate(() => {
      for (const animation of window.__graphAnimations) animation.finish();
      const fork = window.__editchainFixture.rows[0];
      window.__editchainLiveDelta([{ ...fork, node_key: 'live:merge', op_id: 'live:merge',
        timestamp_ms: fork.timestamp_ms + 1000, summary: 'Merge both branches',
        parents: ['live:row:0', 'live:fork'] }]);
    });
    await driver.waitFor(page, () => window.__editchainLiveResult !== null);
    const merged = await page.evaluate(() => ({
      result: window.__editchainLiveResult, total: window.__editchainGetTotal(),
      paths: document.querySelectorAll('.row[data-key="live:merge"] path.graph-live-edge').length,
      x: window.__graphOriginal.getAttribute('cx'),
      radius: window.__graphOriginal.getAttribute('r'),
      animations: document.getAnimations().filter(animation => animation.animationName === 'ec-graph-grow').length,
    }));
    assert.equal(merged.result.error, null);
    assert.equal(merged.total, 42);
    assert.ok(merged.paths > 0, 'the merge grows its curved connection');
    assert.ok(merged.animations > 0);
    assert.equal(merged.x, before.x);
    assert.equal(merged.radius, '4');
    await page.emulateMediaFeatures([{ name: 'prefers-reduced-motion', value: 'reduce' }]);
    assert.equal(await page.evaluate(() => Array.from(document.querySelectorAll('.graph-live-edge')).every(part =>
      part.getAnimations().length === 0 && getComputedStyle(part).strokeDasharray === 'none')), true);
    assertNoErrors(errors);
  } finally { await page.close(); }
});

test('dense graphs preserve lane spacing and scroll horizontally with aligned readable columns', { skip: SKIP }, async () => {
  const { page, errors } = await openRustPage('large');
  try {
    await page.evaluate(() => {
      window.__editchainFixture.max_lane = 190;
      window.__editchainFixture.rows[0].lane = 190;
      window.__editchainLiveUpdate();
    });
    await driver.waitFor(page, () => window.__editchainLiveResult !== null);
    for (const width of [1440, 380]) {
      await page.setViewport({ width, height: 900 });
      await settleRust(page);
      const layout = await page.evaluate(() => {
        const rows = document.getElementById('rows');
        rows.scrollLeft = rows.scrollWidth;
        const first = rows.querySelector('.row[data-row="0"]');
        const content = first.querySelector('.text-cell').getBoundingClientRect();
        const header = rows.querySelector('.tbl-header .th.content').getBoundingClientRect();
        const dot = first.querySelector('.graphDot');
        return { centers: window.__editchainRendererDebug.laneXAll(), scroll: rows.scrollLeft,
          contentWidth: content.width, columnError: content.x - header.x,
          x: Number(dot.getAttribute('cx')), radius: Number(dot.getAttribute('r')) };
      });
      assert.equal(layout.centers.length, 191);
      assert.ok(layout.centers.slice(1).every((x, index) => Math.abs(x - layout.centers[index] - 14.76) < 0.01));
      assert.ok(Math.abs(layout.x - 2819.16) < 0.01);
      assert.equal(layout.radius, 4);
      assert.ok(layout.scroll > 0, 'all lanes remain reachable by horizontal scrolling');
      assert.ok(layout.contentWidth >= 159, 'summary text retains readable width');
      assert.ok(Math.abs(layout.columnError) < 1, 'header and row columns scroll together');
    }
    assertNoErrors(errors);
  } finally { await page.close(); }
});

function assertNoErrors(errors, label) {
  assert.deepEqual(errors.page, [], (label || 'page') + ' errors: ' + JSON.stringify(errors.page));
  assert.deepEqual(errors.console, [],
    (label || 'console') + ' errors: ' + JSON.stringify(errors.console));
}

function assertRustHealthy(state, label) {
  assert.equal(state.loader, 'rust-history', label + ': __editchainRendererDebug must be the rust loader');
  assert.equal(state.wasmReady, true, label + ': __editchainWasmReady');
  assert.equal(state.rendererReady, true, label + ': renderer ready');
  assert.equal(state.dataReady, true, label + ': dataReady');
  assert.equal(state.lastError, null, label + ': no renderer error');
  assert.equal(state.backend, 'svg', label + ': the Rust shell reports the per-row SVG backend');
  assert.equal(state.canvasCount, 0, label + ': no canvas renderer exists');
  assert.ok(state.metrics && state.metrics.renderCount > 0,
    label + ': renderCount > 0, got ' + JSON.stringify(state.metrics));
  assert.equal(state.fragmentCount, state.rows.length,
    label + ': every hydrated row owns exactly one svg.graph-row-fragment');
  assert.equal(state.fragmentMissing, 0,
    label + ': no hydrated row misses its fragment, got ' + JSON.stringify(state.fragmentIssues));
  assert.ok(state.maxAlignDelta <= 1,
    label + ': fragment centres sit on the row centre (<= 1px), got ' + state.maxAlignDelta +
    ' ' + JSON.stringify(state.alignExamples));
  for (const row of state.rows) {
    assert.equal(row.fragmentCount, 1, 'row ' + row.index + ' owns exactly one fragment');
    assert.equal(row.fragmentAriaHidden, 'true', 'row ' + row.index + ' fragment is aria-hidden');
  }
}

async function settleRust(page) {
  await page.bringToFront();
  await page.evaluate((ms) => window.__editchainRendererDebug.whenIdle(ms),
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
    // Renderer health (SVG backend, zero canvases, one per-row
    // fragment, fragment centres on the row centres).
    assertRustHealthy(state, 'boot');

    const visibleChrome = await page.evaluate(() => {
      const dateCells = Array.from(document.querySelectorAll('.date-cell'))
        .filter((cell) => (cell.textContent || '').trim() !== '' &&
          getComputedStyle(cell).display !== 'none');
      return {
        rendererStatusBars: document.querySelectorAll(
          '#gpu-toolbar, #gpu-backend, #gpu-status').length,
        datedRows: dateCells.length,
        clippedDates: dateCells.filter((cell) =>
          cell.scrollWidth > cell.clientWidth + 1).map((cell) => ({
            text: (cell.textContent || '').trim(),
            clientWidth: cell.clientWidth,
            scrollWidth: cell.scrollWidth,
          })),
      };
    });
    assert.equal(visibleChrome.rendererStatusBars, 0,
      'the SVG renderer status bar is absent');
    assert.ok(visibleChrome.datedRows > 0, 'fixture renders dated rows');
    assert.deepEqual(visibleChrome.clippedDates, [],
      'the default Date column renders complete timestamps without ellipsis');

    // Requests carry only the fixed Activity-view window fields.
    assert.ok(state.windowRequests.length >= 1, 'boot issued a GetWindow');
    assert.ok(state.windowRequests.every((request) =>
      JSON.stringify(request.keys) === JSON.stringify(['include_layout', 'limit', 'offset', 'snapshot_id'])),
    'every window uses the minimal protocol shape');
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
    await page.evaluate((ms) => window.__editchainRendererDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
    state = await rustState(page);
    assert.notEqual(state.rowsWidth, rowsWidthBefore,
      'the available width actually changed');
    assert.deepEqual(state.laneXAll, laneXBefore,
      'laneXAll is unchanged after changing the graph host/available width');
    assertRustHealthy(state, 'after resize');

    // Runtime proof that the retired main.js and gpu-preview bootstrap stay
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

test('Tags owns every chip while Content keeps regular prefix-free prose',
  { skip: SKIP }, async () => {
    const { page, errors } = await openRustPage('workUnits');
    try {
      const presentation = await page.evaluate(() => {
        const styleOf = (element) => {
          if (!element) return null;
          const style = getComputedStyle(element);
          return {
            color: style.color,
            backgroundColor: style.backgroundColor,
            borderTop: style.borderTop,
            borderRight: style.borderRight,
            borderBottom: style.borderBottom,
            borderLeft: style.borderLeft,
            borderRadius: style.borderRadius,
            fontFamily: style.fontFamily,
            fontSize: style.fontSize,
            fontWeight: style.fontWeight,
            lineHeight: style.lineHeight,
            paddingTop: style.paddingTop,
            paddingRight: style.paddingRight,
            paddingBottom: style.paddingBottom,
            paddingLeft: style.paddingLeft,
          };
        };
        const gitChip = Array.from(document.querySelectorAll('.git-prefix-chip'))
          .find((element) => (element.textContent || '').trim() === 'chore') || null;
        const gitRow = gitChip?.closest('.row') || null;
        const gitContent = gitRow?.querySelector('.text-cell .git-summary-text') || null;
        const activityChip = document.querySelector('.bundle-count');
        const agentChip = document.querySelector('.session-chip-agent');
        const workUnitChip = document.querySelector('.work-unit-count');
        const sessionRow = document.querySelector('.row-session-summary');
        const chipSelector = [
          '.git-prefix-chip', '.bundle-count', '.bundle-status',
          '.session-chip', '.rel-badge', '.out-badge', '.work-unit-count',
        ].join(',');
        const chips = Array.from(document.querySelectorAll(chipSelector));
        const rows = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
        const structuredRows = rows.filter((row) =>
          !row.classList.contains('row-file'));
        const omitsObviousTitle = (row) => {
          const wire = window.__editchainRowAt?.(Number(row.getAttribute('data-row'))) || {};
          const kind = String(wire.kind || '');
          const role = String(wire.record_role || '');
          return kind === 'git' || !!wire.git_oid || kind === 'message' ||
            kind === 'work-group' ||
            (!kind && role === 'narrative');
        };
        const structuredIssues = structuredRows.filter((row) => {
          const icon = row.querySelector('.text-cell .content-icon');
          const svg = icon?.querySelector('svg.content-icon-svg');
          const title = row.querySelector('.text-cell .content-title');
          const subtitle = row.querySelector('.text-cell .content-subtitle');
          return !icon?.getAttribute('data-content-icon') ||
            icon.getAttribute('aria-hidden') !== 'true' || !svg ||
            svg.querySelectorAll('path').length === 0 ||
            svg.getBoundingClientRect().width <= 0 || svg.getBoundingClientRect().height <= 0 ||
            (omitsObviousTitle(row) ? !!title : !(title?.textContent || '').trim()) ||
            !(subtitle?.textContent || '').trim();
        }).map((row) => row.getAttribute('data-row'));
        const obviousTitleRows = structuredRows.filter(omitsObviousTitle);
        const workGroupRows = rows.filter((row) =>
          row.getAttribute('data-activity-bundle') === 'work-group');

        const humanRow = document.createElement('div');
        humanRow.className = 'row row-human';
        humanRow.style.position = 'fixed';
        humanRow.style.visibility = 'hidden';
        const humanSummary = document.createElement('span');
        humanSummary.className = 'summary';
        humanSummary.textContent = 'human content';
        humanRow.appendChild(humanSummary);
        document.body.appendChild(humanRow);
        const humanWeight = getComputedStyle(humanSummary).fontWeight;
        humanRow.remove();

        return {
          humanWeight,
          gitPrefix: (gitChip?.textContent || '').trim(),
          gitContent: (gitContent?.textContent || '').trim(),
          gitStyle: styleOf(gitChip),
          activityStyle: styleOf(activityChip),
          agentStyle: styleOf(agentChip),
          workUnitStyle: styleOf(workUnitChip),
          headerLabels: Array.from(document.querySelectorAll('.tbl-header .th'))
            .map((cell) => (cell.textContent || '').trim()),
          chipCount: chips.length,
          misplacedChips: chips.filter((chip) =>
            !chip.parentElement?.classList.contains('tags-cell')).length,
          contentChipCount: document.querySelectorAll(
            '.text-cell :is(' + chipSelector + ')').length,
          activityIconCount: document.querySelectorAll(
            '.activity-cell :is(svg, .content-icon)').length,
          structuredRowCount: structuredRows.length,
          structuredIssues,
          obviousTitleRowCount: obviousTitleRows.length,
          obviousTitleIssues: obviousTitleRows.filter((row) =>
            row.querySelector('.text-cell .content-title')).map((row) =>
            row.getAttribute('data-row')),
          workGroupCount: workGroupRows.length,
          workGroupIssues: workGroupRows.filter((row) =>
            !row.querySelector(
              '.text-cell .content-icon[data-content-icon="layers"] svg.content-icon-svg path') ||
            !!row.querySelector('.text-cell .content-title') ||
            !(row.querySelector('.text-cell .content-subtitle')?.textContent || '').trim()
          ).map((row) => row.getAttribute('data-row')),
          maxTagsPerRow: Math.max(0, ...rows.map((row) =>
            row.querySelector('.tags-cell')?.children.length || 0)),
          sessionSummary: sessionRow ? {
            classification: sessionRow.getAttribute('data-classification'),
            sessionCount: sessionRow.getAttribute('data-session-count'),
            workUnitId: sessionRow.getAttribute('data-work-unit-id'),
            activity: (sessionRow.querySelector('.activity-label')?.textContent || '').trim(),
            countChip: (sessionRow.querySelector('.work-unit-count')?.textContent || '').trim(),
          } : null,
        };
      });

      assert.equal(presentation.humanWeight, '400', 'human content uses regular weight');
      assert.equal(presentation.gitPrefix, 'chore', 'fixture exposes a conventional git prefix');
      assert.equal(presentation.gitContent, 'ops one',
        'git content excludes the prefix already shown in its chip');
      assert.ok(presentation.gitStyle, 'git prefix chip is rendered');
      assert.deepEqual(presentation.activityStyle, presentation.gitStyle,
        'activity-count chip matches the git-prefix visual treatment');
      assert.deepEqual(presentation.agentStyle, presentation.gitStyle,
        'named-agent chip matches the git-prefix visual treatment');
      assert.deepEqual(presentation.workUnitStyle, presentation.gitStyle,
        'work-unit count chip matches the git-prefix visual treatment');
      assert.deepEqual(presentation.headerLabels, ['Graph', 'Activity', 'Tags', 'Content', 'Date'],
        'Tags is a real column directly before Content');
      assert.ok(presentation.chipCount > 0, 'fixture renders row tags');
      assert.equal(presentation.misplacedChips, 0, 'every chip is a direct Tags-cell child');
      assert.equal(presentation.contentChipCount, 0, 'Content contains no chips');
      assert.equal(presentation.activityIconCount, 0, 'Activity remains readable text, not an icon');
      assert.ok(presentation.structuredRowCount > 0, 'fixture renders structured Content rows');
      assert.deepEqual(presentation.structuredIssues, [],
        'rows render icon/subtitle and a title only when it adds information');
      assert.ok(presentation.obviousTitleRowCount > 0,
        'fixture covers Commit/Message rows with intentionally omitted titles');
      assert.deepEqual(presentation.obviousTitleIssues, [],
        'Commit/Message rows do not render redundant Content titles');
      assert.deepEqual(presentation.workGroupIssues, [],
        'any WorkGroups render a layers icon directly into their aggregate subtitle');
      assert.ok(presentation.maxTagsPerRow > 1, 'one row can render multiple tags');
      assert.deepEqual(presentation.sessionSummary, {
        classification: 'session',
        sessionCount: '11',
        workUnitId: 'session:s1/turn:t1',
        activity: 'session',
        countChip: '11 entries',
      }, 'whole-session presentation overrides the count without replacing the turn unit');
      assertNoErrors(errors, 'content and chip presentation');
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
        await page.evaluate((ms) => window.__editchainRendererDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
        state = await driver.readState(page);
        guard += 1;
      }
      assert.ok(state.findCurrentRow > 500,
        'navigation reached an off-cache match at row ' + state.findCurrentRow);
      assert.ok(Math.max(...state.windowOffsets, 0) > 0,
        'off-cache jumps fetch deep GetWindow offsets');

      await driver.clearSearch(page);
      await page.bringToFront();
      await page.evaluate((ms) => window.__editchainRendererDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
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
        const el = document.querySelector('.row-expandable[data-activity-bundle]');
        return el ? Number(el.getAttribute('data-row')) : null;
      });
      assert.ok(expandable !== null, 'scenario contains an expandable group row');
      const affordance = await page.evaluate((n) => {
        const row = document.querySelector('.row[data-row="' + n + '"]');
        const activity = row?.querySelector('.activity-cell');
        const label = activity?.querySelector('.activity-label');
        const chevron = activity?.querySelector('.subop-chevron');
        return {
          children: activity ? Array.from(activity.children).map((child) => child.className) : [],
          label: label?.textContent ?? '',
          iconCount: activity?.querySelectorAll('svg, .content-icon').length ?? 0,
          glyph: chevron?.textContent ?? '',
          labelColor: label ? getComputedStyle(label).color : '',
          chevronColor: chevron ? getComputedStyle(chevron).color : '',
          contentChevron: !!row?.querySelector('.text-cell .subop-chevron'),
        };
      }, expandable);
      assert.deepEqual(
        affordance.children,
        ['activity-label', 'subop-chevron'],
        'disclosure follows the readable Activity text');
      assert.ok(affordance.label, 'Activity text remains visible');
      assert.equal(affordance.iconCount, 0, 'Activity owns no icon');
      assert.equal(affordance.glyph, '\u25b8', 'collapsed Activity affordance points right');
      assert.equal(
        affordance.chevronColor,
        affordance.labelColor,
        'disclosure inherits the Activity title color');
      assert.equal(affordance.contentChevron, false, 'content cell no longer owns disclosure');
      const collapsedGroupMarker = await page.evaluate((n) => {
        const graph = document.querySelector('.row[data-row="' + n + '"] .graph-cell');
        return {
          dots: graph?.querySelectorAll('.graphDot').length ?? 0,
          capsules: graph?.querySelectorAll('.graphBundleCapsule').length ?? 0,
        };
      }, expandable);
      assert.deepEqual(collapsedGroupMarker, { dots: 0, capsules: 1 },
        'a folded group summary uses the capsule marker');
      await page.evaluate((n) => document
        .querySelector('.row[data-row="' + n + '"] .subop-chevron')?.click(), expandable);
      await page.bringToFront();
      await page.evaluate((ms) => window.__editchainRendererDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
      const aria = await page.evaluate((n) =>
        document.querySelector('.row[data-row="' + n + '"]')?.getAttribute('aria-expanded'),
      expandable);
      assert.equal(aria, 'true', 'chevron toggles aria-expanded');
      assert.equal(
        await page.evaluate((n) => document
          .querySelector('.row[data-row="' + n + '"] .activity-cell .subop-chevron')
          ?.textContent, expandable),
        '\u25be',
        'expanded Activity affordance points down');
      assert.ok(
        await page.evaluate(() => document.querySelectorAll('#rows .row-subop').length) > 0,
        'expanding reveals sub-op rows');
      const unfoldedGroupMarker = await page.evaluate((n) => {
        const graph = document.querySelector('.row[data-row="' + n + '"] .graph-cell');
        return {
          dots: graph?.querySelectorAll('.graphDot').length ?? 0,
          capsules: graph?.querySelectorAll('.graphBundleCapsule').length ?? 0,
          terminals: graph?.querySelectorAll('.graphBundleTerminal').length ?? 0,
        };
      }, expandable);
      assert.deepEqual(unfoldedGroupMarker, { dots: 1, capsules: 0, terminals: 0 },
        'an unfolded group summary uses one ordinary circle');
      const openedGroupMarkers = await page.evaluate(() => {
        const members = Array.from(document.querySelectorAll('#rows .row-subop'));
        const structured = members.filter((row) =>
          !row.classList.contains('row-file'));
        const titleIsCorrect = (row) => {
          const wire = window.__editchainRowAt?.(Number(row.getAttribute('data-row'))) || {};
          const kind = String(wire.kind || '');
          const role = String(wire.record_role || '');
          const omit = kind === 'git' || !!wire.git_oid || kind === 'message' ||
            kind === 'work-group' ||
            (!kind && role === 'narrative');
          const title = row.querySelector('.text-cell .content-title');
          return omit ? !title : !!(title?.textContent || '').trim();
        };
        return {
          rows: members.length,
          dots: members.filter((row) =>
            row.querySelectorAll('.graph-cell .graphDot').length === 1).length,
          nestedCapsules: members.filter((row) =>
            row.querySelector('.graph-cell .graphBundleCapsule')).length,
          structured: structured.length,
          structureIssues: structured.filter((row) =>
            !row.querySelector('.text-cell .content-icon[data-content-icon] svg.content-icon-svg path') ||
            !titleIsCorrect(row) ||
            !(row.querySelector('.text-cell .content-subtitle')?.textContent || '').trim()
          ).length,
        };
      });
      assert.equal(openedGroupMarkers.dots, openedGroupMarkers.rows,
        'every revealed group member owns one graph dot');
      assert.equal(openedGroupMarkers.nestedCapsules, 0,
        'revealed members use dots instead of nested heavy capsules');
      assert.ok(openedGroupMarkers.structured > 0,
        'revealed ordinary members use the shared Content structure');
      assert.equal(openedGroupMarkers.structureIssues, 0,
        'revealed ordinary members contain icon/subtitle and only useful titles');
      assert.equal(
        await page.evaluate(() =>
          Array.from(document.querySelectorAll('#rows .row'))
            .filter((row) => row.getAttribute('tabindex') === '0').length),
        1,
        'exactly one tabbable row');

      // Read-only find facade reflects the session.
      const findState = await page.evaluate(() => window.__editchainRendererDebug.findState());
      assert.equal(findState.active, false, 'find facade reports the cleared session');
      assertNoErrors(errors, 'rust smoke (interactions)');
    } finally {
      await page.close();
    }
  });

test('find keyboard and pending/zero/error ARIA are functional',
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
        snapshot_id: await page.evaluate(() => window.__editchainRequestLog[0].GetWindow.snapshot_id),
        op_id: null,
        git_oid: expected.git_oid,
        repository: expected.repository,
      }, 'double-click posts the exact git identity');

      const resizeAffordances = await page.evaluate(() => {
        const header = document.querySelector('.tbl-header');
        const handles = Array.from(document.querySelectorAll('.col-resize-handle'));
        return {
          columns: handles.map((handle) => handle.getAttribute('data-col')),
          allInHeader: handles.every((handle) => handle.parentElement === header),
          positions: handles.map((handle) => handle.getBoundingClientRect().left),
          indicators: handles.map((handle) => {
            const style = getComputedStyle(handle, '::after');
            return { width: style.width, backgroundColor: style.backgroundColor };
          }),
        };
      });
      assert.deepEqual(resizeAffordances.columns,
        ['graph', 'activity', 'tags', 'content', 'date'],
        'every visible column exposes a resize handle');
      assert.equal(resizeAffordances.allInHeader, true,
        'resize handles stay in the sticky header');
      assert.ok(resizeAffordances.positions.every((left, index, positions) =>
        index === 0 || left > positions[index - 1]), 'resize handles follow column order');
      assert.ok(resizeAffordances.indicators.every((indicator) =>
        indicator.width === '1px' && indicator.backgroundColor !== 'rgba(0, 0, 0, 0)'),
      'every handle has a visible centre indicator');

      const resizedFixedColumns = await page.evaluate(() => {
        const drag = (column, delta) => {
          const header = document.querySelector('.tbl-header .th.' + column);
          const handle = document.querySelector('.col-resize-handle[data-col="' + column + '"]');
          const before = header.getBoundingClientRect().width;
          const rect = handle.getBoundingClientRect();
          const startX = rect.left + rect.width / 2;
          const clientY = rect.top + rect.height / 2;
          handle.dispatchEvent(new MouseEvent('mousedown', {
            bubbles: true, cancelable: true, clientX: startX, clientY,
          }));
          window.dispatchEvent(new MouseEvent('mousemove', {
            bubbles: true, cancelable: true, clientX: startX + delta, clientY,
          }));
          window.dispatchEvent(new MouseEvent('mouseup', {
            bubbles: true, cancelable: true, clientX: startX + delta, clientY,
          }));
          return {
            before,
            after: document.querySelector('.tbl-header .th.' + column)
              .getBoundingClientRect().width,
          };
        };
        const activity = drag('activity', 18);
        const tags = drag('tags', 24);
        const activityRestored = drag('activity', -18);
        const tagsRestored = drag('tags', -24);
        return { activity, tags, activityRestored, tagsRestored };
      });
      assert.ok(resizedFixedColumns.activity.after > resizedFixedColumns.activity.before,
        'Activity divider resizes its column');
      assert.ok(resizedFixedColumns.tags.after > resizedFixedColumns.tags.before,
        'Tags divider resizes its column');
      assert.ok(Math.abs(resizedFixedColumns.activityRestored.after -
        resizedFixedColumns.activity.before) <= 1, 'Activity width restores');
      assert.ok(Math.abs(resizedFixedColumns.tagsRestored.after -
        resizedFixedColumns.tags.before) <= 1, 'Tags width restores');
      await settleRust(page);

      const replacedDrag = await page.evaluate(() => {
        const add = window.addEventListener;
        const remove = window.removeEventListener;
        const active = { mousemove: new Set(), mouseup: new Set() };
        window.addEventListener = function (type, callback, options) {
          active[type]?.add(callback);
          return add.call(this, type, callback, options);
        };
        window.removeEventListener = function (type, callback, options) {
          active[type]?.delete(callback);
          return remove.call(this, type, callback, options);
        };
        try {
          const handle = document.querySelector('.col-resize-handle[data-col="graph"]');
          const during = [];
          for (let replacement = 0; replacement < 3; replacement++) {
            handle.dispatchEvent(new MouseEvent('mousedown', {
              bubbles: true, cancelable: true, clientX: 100,
            }));
            during.push([active.mousemove.size, active.mouseup.size]);
          }
          window.dispatchEvent(new MouseEvent('mousemove', { clientX: 100 }));
          window.dispatchEvent(new MouseEvent('mouseup', { clientX: 100 }));
          return {
            during,
            after: [active.mousemove.size, active.mouseup.size],
            resizing: document.body.classList.contains('col-resizing'),
          };
        } finally {
          window.addEventListener = add;
          window.removeEventListener = remove;
        }
      });
      assert.deepEqual(replacedDrag.during, [[1, 1], [1, 1], [1, 1]],
        'a new drag replaces the previous window listeners');
      assert.deepEqual(replacedDrag.after, [0, 0], 'mouseup releases both listeners');
      assert.equal(replacedDrag.resizing, false);

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
        'divider drag re-renders the per-row SVG fragments');
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

test('a DOM failure is reported without reborrowing or disabling the shell',
  { skip: SKIP }, async () => {
    const { page, errors } = await openRustPage('merge');
    try {
      const injected = await page.evaluate(() => {
        const rows = document.getElementById('rows');
        const query = rows.querySelectorAll;
        let injected = false;
        rows.querySelectorAll = function (selector) {
          if (!injected && selector === '.row.row-selected') {
            injected = true;
            throw new Error('fixture selection failure');
          }
          return query.call(this, selector);
        };
        try {
          rows.querySelector('.row[data-row="0"]').dispatchEvent(new MouseEvent('click', {
            bubbles: true, cancelable: true, detail: 1,
          }));
          return injected;
        } finally {
          rows.querySelectorAll = query;
        }
      });
      assert.equal(injected, true, 'the production selection effect encounters the failure');
      await settleRust(page);
      assert.deepEqual(errors.page, [], 'diagnostics do not panic inside a borrowed shell');
      assert.equal(errors.console.length, 1);
      assert.match(errors.console[0], /^selection apply failed:/);
      errors.console.length = 0;
      await page.click('.row[data-row="1"] .text-cell');
      await settleRust(page);
      assert.equal(await page.$eval('.row[data-row="1"]',
        (row) => row.getAttribute('aria-selected')), 'true', 'later transitions still run');
      assertRustHealthy(await rustState(page), 'after a recoverable DOM failure');
      assertNoErrors(errors, 'recovered DOM effects');
    } finally {
      await page.close();
    }
  });

test('repeated Retry panes retire their listeners before the next history load',
  { skip: SKIP }, async () => {
    const { page, errors } = await openRustPage('merge');
    try {
      await page.evaluate(() => {
        const add = EventTarget.prototype.addEventListener;
        const remove = EventTarget.prototype.removeEventListener;
        window.__retryListeners = new Set();
        EventTarget.prototype.addEventListener = function (type, callback, options) {
          if (type === 'click' && this instanceof HTMLElement && this.matches('.retry-btn')) {
            window.__retryListeners.add(callback);
          }
          return add.call(this, type, callback, options);
        };
        EventTarget.prototype.removeEventListener = function (type, callback, options) {
          window.__retryListeners.delete(callback);
          return remove.call(this, type, callback, options);
        };
      });
      for (let attempt = 0; attempt < 3; attempt++) {
        await page.evaluate(() => {
          window.__editchainFixture.rows[0].content = {
            tool_label: { text: 'x'.repeat(257), complete: true },
          };
          window.__editchainStart();
        });
        await driver.waitFor(page, () => !!document.querySelector('.retry-btn'),
          { timeout: driver.IDLE_TIMEOUT_MS });
        assert.equal(await page.evaluate(() => window.__retryListeners.size), 1);
        await page.evaluate(() => {
          delete window.__editchainFixture.rows[0].content;
          window.__retiredRetry = document.querySelector('.retry-btn');
          window.__retiredRetry.click();
        });
        await driver.waitFor(page, () => !!document.querySelector('.row[data-key]'),
          { timeout: driver.IDLE_TIMEOUT_MS });
        await settleRust(page);
        const retired = await page.evaluate(() => {
          const before = window.__editchainRequestLog.length;
          window.__retiredRetry.click();
          return { active: window.__retryListeners.size, before };
        });
        assert.equal(retired.active, 0, 'the removed pane owns no live click listener');
        await settleRust(page);
        assert.equal(await page.evaluate(() => window.__editchainRequestLog.length),
          retired.before, 'a detached Retry button cannot restart history');
      }
      assertRustHealthy(await rustState(page), 'after repeated Retry');
      assertNoErrors(errors, 'Retry lifetime');
    } finally {
      await page.close();
    }
  });

test('Git and agent parents reveal column-aligned file rows whose click opens an exact diff identity',
  { skip: SKIP }, async () => {
    const { page, errors } = await openRustPage('fileEdits');
    try {
      await driver.installHarnessSpies(page);

      await driver.clickRow(page, 0);
      await settleRust(page);
      const gitFile = await page.evaluate(() => {
        const row = document.querySelector('.row-file[data-row="1"]');
        if (!row) return null;
        const style = getComputedStyle(row.querySelector('.text-cell'));
        const icon = row.querySelector('.file-icon');
        const iconOutline = getComputedStyle(icon, '::before');
        const iconStroke = getComputedStyle(icon, '::after');
        return {
          path: row.getAttribute('data-file-path'),
          status: row.getAttribute('data-file-status'),
          source: row.getAttribute('data-file-source'),
          name: row.querySelector('.file-name')?.textContent,
          directory: row.querySelector('.file-directory')?.textContent,
          statusText: row.querySelector('.file-status')?.textContent,
          fidelity: row.querySelector('.file-fidelity')?.textContent || '',
          activityText: row.querySelector('.activity-label')?.textContent,
          activityDisplay: getComputedStyle(row.querySelector('.activity-cell')).display,
          textGridStart: style.gridColumnStart,
          statusParent: row.querySelector('.file-status')?.parentElement?.className,
          contentParent: row.querySelector('.file-name')?.closest('.text-cell')?.className,
          iconWidth: icon.getBoundingClientRect().width,
          iconOutline: iconOutline.borderTopWidth,
          iconStrokeVisible: iconStroke.backgroundColor !== 'rgba(0, 0, 0, 0)',
          aria: row.getAttribute('aria-label'),
        };
      });
      assert.deepEqual(gitFile, {
        path: 'crates/editchain-git/src/diff.rs',
        status: 'modified',
        source: 'git',
        name: 'diff.rs',
        directory: 'crates/editchain-git/src',
        statusText: 'M',
        fidelity: '',
        activityText: 'change',
        activityDisplay: 'flex',
        textGridStart: 'auto',
        statusParent: 'tags-cell',
        contentParent: 'text-cell',
        iconWidth: 16,
        iconOutline: '1px',
        iconStrokeVisible: true,
        aria: 'Modified crates/editchain-git/src/diff.rs, Git commit; open diff',
      });
      await driver.clickRow(page, 1);
      let state = await driver.readState(page);
      assert.equal(state.selectedRow, 1, 'file click selects the edit child');
      assert.equal(state.openDiffLog.length, 1, 'file click posts one openDiff action');
      assert.deepEqual(state.openDiffLog[0].change,
        windowlessGitChange(), 'Git envelope preserves every immutable identity field');

      await page.evaluate(() => {
        document.querySelector('.row-file[data-row="1"]')?.dispatchEvent(new MouseEvent('dblclick', {
          bubbles: true,
          cancelable: true,
          detail: 2,
        }));
      });
      state = await driver.readState(page);
      assert.equal(state.openJsonLog.length, 0, 'double-click never replaces a file diff with JSON');

      await driver.clickRow(page, 2);
      await settleRust(page);
      const agentFile = await page.evaluate(() => {
        const row = document.querySelector('.row-file[data-row="3"]');
        return row ? {
          path: row.getAttribute('data-file-path'),
          source: row.getAttribute('data-file-source'),
          fidelity: row.querySelector('.file-fidelity')?.textContent,
          statusParent: row.querySelector('.file-status')?.parentElement?.className,
          fidelityParent: row.querySelector('.file-fidelity')?.parentElement?.className,
          contentParent: row.querySelector('.file-name')?.closest('.text-cell')?.className,
          activity: row.querySelector('.activity-label')?.textContent,
          classes: Array.from(row.classList),
        } : null;
      });
      assert.equal(agentFile?.path, 'extensions/vscode-editchain/src/extension.ts');
      assert.equal(agentFile?.source, 'agent');
      assert.equal(agentFile?.fidelity, 'recorded');
      assert.equal(agentFile?.statusParent, 'tags-cell');
      assert.equal(agentFile?.fidelityParent, 'tags-cell');
      assert.equal(agentFile?.contentParent, 'text-cell');
      assert.equal(agentFile?.activity, 'change');
      assert.ok(agentFile?.classes.includes('row-file-partial'));
      await saveParityScreenshot(page, 'file-edits');
      await driver.pressRowKey(page, 'Enter', 3);
      state = await driver.readState(page);
      assert.equal(state.openDiffLog.length, 2, 'Enter activates the agent diff');
      assert.equal(state.openDiffLog[1].change.op_id, 'node:agent:edit:normalized');
      assertNoErrors(errors, 'SCM file rows');
    } finally {
      await page.close();
    }
  });

function windowlessGitChange() {
  return {
    source: 'git',
    path: 'crates/editchain-git/src/diff.rs',
    status: 'modified',
    binary: false,
    partial: false,
    repository: '9007199254740993',
    repository_path: 'crates/editchain-git/src/diff.rs',
    commit_oid: '0123456789abcdef0123456789abcdef01234567',
    old_oid: '1111111111111111111111111111111111111111',
    new_oid: '2222222222222222222222222222222222222222',
    old_mode: 'blob',
    new_mode: 'blob',
  };
}

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
    await page.evaluate((ms) => window.__editchainRendererDebug.whenIdle(ms), driver.IDLE_TIMEOUT_MS);
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
    assertRustHealthy(state, 'after deep scroll');
    await saveParityScreenshot(page, 'deep-scroll');
    assertNoErrors(errors, 'rust smoke (large)');
  } finally {
    await page.close();
  }
});
