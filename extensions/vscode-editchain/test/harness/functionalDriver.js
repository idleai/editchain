// Browser driver for deterministic GPU functional-parity tests
// (functionalParity.test.js). This is the OFFSCREEN regression-oracle driver:
// both harness pages run the SAME production renderer (media/main.js) +
// fixture bridge, so every interaction is driven through the real controls on
// BOTH pages and the outcomes are compared:
// profile switches, find-in-chain navigation, legacy flat-list Search, virtual
// scrolling/paging, row selection/keyboard/raw-JSON identity, and work-unit /
// bundle expansion. The GPU page additionally exposes
// window.__editchainGpuDebug (dataReady, lastError, backend, snapshot,
// metrics, whenIdle) and overlays its wgpu canvas only over .graph-cell.
//
// Everything waits on concrete renderer state (debug whenIdle / DOM
// predicates) — never wall-clock sleeps — so the suite is deterministic.
//
// This file deliberately reuses the harness helpers (fixtures.js,
// fixtureBridge.js, layoutProbe's __editchainDebug, main.js debug hooks) and
// mirrors the interaction patterns of searchKeyboard.test.js; it does not
// duplicate the CPU-only probes.

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const puppeteer = require('puppeteer-core');

const EXT_ROOT = path.join(__dirname, '..', '..');

const DEFAULT_CHROME =
  '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';
const CHROME = process.env.CHROME_PATH || DEFAULT_CHROME;

const BOOT_TIMEOUT_MS = Number(process.env.GPU_BOOT_TIMEOUT_MS) || 60_000;
const IDLE_TIMEOUT_MS = Number(process.env.GPU_IDLE_TIMEOUT_MS) || 60_000;

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.map': 'application/json; charset=utf-8',
};

/** Whether this machine can run the browser functional suite at all. */
function suitePrereqs() {
  if (!fs.existsSync(CHROME)) {
    return { ok: false, reason: 'Chrome not found at ' + CHROME + ' (set CHROME_PATH)' };
  }
  for (const rel of [
    'media/gpu-preview/bootstrap.js',
    'media/gpu-preview/pkg/editchain_gpu_preview.js',
    'media/gpu-preview/pkg/editchain_gpu_preview_bg.wasm',
  ]) {
    if (!fs.existsSync(path.join(EXT_ROOT, rel))) {
      return { ok: false, reason: 'missing GPU asset ' + rel + ' (run npm run build:gpu first)' };
    }
  }
  return { ok: true, reason: '' };
}

/** Minimal static server over the extension root (node:http, no deps). */
function startServer(rootDir) {
  const root = path.resolve(rootDir);
  const server = http.createServer((req, res) => {
    let pathname;
    try {
      pathname = decodeURIComponent(new URL(req.url, 'http://127.0.0.1').pathname);
    } catch {
      res.writeHead(400);
      res.end('bad request');
      return;
    }
    if (pathname.endsWith('/')) pathname += 'index.html';
    const filePath = path.resolve(root, '.' + pathname);
    if (filePath !== root && !filePath.startsWith(root + path.sep)) {
      res.writeHead(403);
      res.end('forbidden');
      return;
    }
    fs.stat(filePath, (err, st) => {
      if (err || !st.isFile()) {
        res.writeHead(404);
        res.end('not found');
        return;
      }
      res.writeHead(200, {
        'Content-Type': MIME[path.extname(filePath).toLowerCase()] || 'application/octet-stream',
      });
      fs.createReadStream(filePath).pipe(res);
    });
  });
  return new Promise((resolve, reject) => {
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

/** Headless Chrome with the deterministic software-rasterized WebGL backend
 * (same flags as ui-gpu-preview.mjs; never requires hardware WebGPU). */
function launchBrowser() {
  return puppeteer.launch({
    executablePath: CHROME,
    headless: 'new',
    args: [
      '--no-sandbox',
      '--disable-setuid-sandbox',
      '--use-angle=swiftshader',
      '--enable-unsafe-swiftshader',
      '--ignore-gpu-blocklist',
      '--enable-webgl',
      '--disable-gpu-sandbox',
    ],
  });
}

/** Open the CPU harness page and the GPU harness page against the same base. */
async function openPages(browser, baseUrl) {
  const viewport = { width: 1440, height: 900 };
  const cpu = await browser.newPage();
  await cpu.setViewport(viewport);
  const gpu = await browser.newPage();
  await gpu.setViewport(viewport);
  const errors = { cpu: [], gpu: [] };
  cpu.on('pageerror', (e) => errors.cpu.push(e.message));
  gpu.on('pageerror', (e) => errors.gpu.push(e.message));
  // These are local static pages. Renderer readiness is asserted explicitly
  // by bootScenario below; waiting for Chrome's incidental network-idle state
  // made repeated WebGL oracle boots randomly consume the full timeout.
  await cpu.goto(baseUrl + '/test/harness/index.html', {
    waitUntil: 'domcontentloaded', timeout: BOOT_TIMEOUT_MS,
  });
  await gpu.goto(baseUrl + '/test/harness/gpu.html?backend=webgl', {
    waitUntil: 'domcontentloaded', timeout: BOOT_TIMEOUT_MS,
  });
  return { cpu, gpu, errors };
}

/** Wait for a page-side predicate (serializable function body). */
async function waitFor(page, fn, opts) {
  opts = opts || {};
  // Timer polling keeps cross-page predicates live when Chromium throttles
  // requestAnimationFrame in the background tab.
  await page.waitForFunction(fn, {
    timeout: opts.timeout || BOOT_TIMEOUT_MS,
    polling: 50,
  }, ...(opts.args || []));
}

/** Load a scenario through the real harness startup handshake and settle. */
async function bootScenario(page, kind, scenario) {
  await page.evaluate((name) => {
    window.__editchainSetScenario(name);
    window.__editchainStart();
  }, scenario);
  if (kind === 'cpu') {
    await waitFor(page, () =>
      typeof window.__editchainDebug === 'object' && window.__editchainDataReady === true);
    await page.bringToFront();
    await page.evaluate((ms) => window.__editchainDebug.whenIdle(ms), IDLE_TIMEOUT_MS);
    return;
  }
  // GPU page: the bootstrap must exist; readiness may come from main.js even
  // for expected empty/error scenarios (which never render rows or geometry).
  await waitFor(page, () =>
    typeof window.__editchainGpuDebug === 'object' &&
    (window.__editchainDataReady === true || !!window.__editchainGpuDebug.lastError));
  const startupError = await page.evaluate(() => window.__editchainGpuDebug.lastError || null);
  if (startupError) throw new Error('GPU startup failed: ' + startupError);
  await page.bringToFront();
  const rowsExpected = scenario !== 'empty' && scenario !== 'error';
  if (rowsExpected) {
    await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), IDLE_TIMEOUT_MS);
  } else {
    await waitFor(page, () => !!document.querySelector('#rows .view-message'));
  }
}

/** Re-settle a page after an interaction (debug whenIdle, never a sleep). */
async function settle(page, kind) {
  // Both renderer idle contracts advance on requestAnimationFrame. Chromium
  // suspends rAF in background tabs, so every settle must foreground its page
  // and callers must settle the CPU/GPU pages sequentially.
  await page.bringToFront();
  if (kind === 'cpu') {
    await page.evaluate((ms) => window.__editchainDebug.whenIdle(ms), IDLE_TIMEOUT_MS);
  } else {
    await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), IDLE_TIMEOUT_MS);
  }
}

/** Wrap postMessage on a page so raw-JSON identity is captured and (optionally)
 * FindInHistory requests are rewritten down the legacy Search path. */
async function installHarnessSpies(page, { legacySearch = false } = {}) {
  await page.evaluate((rewrite) => {
    const orig = window.vscode.postMessage.bind(window.vscode);
    window.__editchainOpenJsonLog = [];
    window.vscode.postMessage = function (msg) {
      if (msg && msg.type === 'openJson') {
        window.__editchainOpenJsonLog.push({
          type: 'openJson',
          op_id: msg.op_id !== undefined ? msg.op_id : null,
          git_oid: msg.git_oid !== undefined ? msg.git_oid : null,
          repository: msg.repository !== undefined ? msg.repository : null,
        });
      }
      if (rewrite && msg && msg.body && msg.body.FindInHistory) {
        const f = msg.body.FindInHistory;
        msg.body = {
          Search: { query: f.query, mode: 'Lexical', top_k: f.top_k, filters: f.filters || {} },
        };
      }
      return orig(msg);
    };
  }, legacySearch);
}

/** Fill + Enter on the real #search control (the keyboard path). */
async function runSearch(page, query) {
  await page.evaluate((q) => {
    const input = document.getElementById('search');
    input.value = q;
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  }, query);
}

/** Clear the search input through the real input handler (exits find / restores
 * the chain from a legacy flat-list view). */
async function clearSearch(page) {
  await page.evaluate(() => {
    const input = document.getElementById('search');
    input.value = '';
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

/** Focus the search input (real focus so focus-dependent handlers fire). */
async function focusSearch(page) {
  await page.evaluate(() => document.getElementById('search').focus());
}

/** Switch the profile through the real segmented control. */
async function clickProfile(page, name) {
  await page.evaluate((n) => {
    document.getElementById('profile-' + n).click();
  }, name);
}

/** Real-mouse click on a find-navigation button (mousedown + click pipeline). */
async function clickNav(page, which) {
  await page.bringToFront();
  await page.click('#' + (which === 'prev' ? 'search-prev' : 'search-next'));
}

/** Scroll #rows to an absolute row (pixel target; the renderer pages as it
 * scrolls) and wait for the row to be a real (non-placeholder) element. */
async function scrollToRow(page, absRow) {
  const ROW_H = 34;
  await page.bringToFront();
  await page.evaluate((target) => {
    const rows = document.getElementById('rows');
    rows.scrollTop = Math.max(0, target);
  }, absRow * ROW_H);
  await waitFor(page, (target) => {
    const el = document.querySelector('.row[data-row="' + target + '"]');
    return !!el && !el.classList.contains('row-placeholder') &&
      typeof window.__editchainRowAt === 'function' &&
      window.__editchainRowAt(target) !== null;
  }, { timeout: IDLE_TIMEOUT_MS, args: [absRow] });
}

/** Scroll #rows to the bottom in bounded steps (pages load as it scrolls). */
async function scrollToBottom(page) {
  await page.bringToFront();
  let prevHeight = -1;
  for (let i = 0; i < 60; i++) {
    const height = await page.evaluate(() => {
      const rows = document.getElementById('rows');
      rows.scrollTop = rows.scrollHeight;
      return rows.scrollHeight;
    });
    await page.waitForFunction(() => {
      const rows = document.getElementById('rows');
      const rendered = Array.from(document.querySelectorAll(
        '#rows .row:not(.row-placeholder)[data-row]'));
      const last = rendered.at(-1);
      const total = typeof window.__editchainGetTotal === 'function'
        ? Number(window.__editchainGetTotal())
        : 0;
      return rows.scrollTop >= rows.scrollHeight - rows.clientHeight - 1 &&
        document.querySelectorAll('.row-placeholder').length === 0 &&
        last !== undefined && Number(last.getAttribute('data-row')) === total - 1;
    }, { timeout: IDLE_TIMEOUT_MS, polling: 50 });
    if (height === prevHeight) break;
    prevHeight = height;
  }
}

/** Scroll #rows back to the top and wait for the top row to be real. */
async function scrollToTop(page) {
  await page.bringToFront();
  await page.evaluate(() => {
    document.getElementById('rows').scrollTop = 0;
  });
  await waitFor(page, () => {
    const el = document.querySelector('.row[data-row="0"]');
    return !!el && !el.classList.contains('row-placeholder');
  }, { timeout: IDLE_TIMEOUT_MS });
}

/** Click a rendered row by absolute index (inline selection; disclosure also
 * toggles for expandable rows, exactly like the production click handler). */
async function clickRow(page, absRow) {
  await page.evaluate((n) => {
    const el = document.querySelector('.row[data-row="' + n + '"]');
    if (!el) throw new Error('no .row[data-row=' + n + '] to click');
    el.click();
  }, absRow);
}

/** Click the first expandable row's disclosure chevron. */
async function clickFirstExpandable(page) {
  await page.evaluate(() => {
    const chevron = document.querySelector('.row-expandable .subop-chevron');
    if (!chevron) throw new Error('no .row-expandable .subop-chevron to click');
    chevron.click();
  });
}

/** Focus a row and dispatch a keyboard key (roving focus / activation). */
async function pressRowKey(page, key, absRow) {
  await page.evaluate(({ key, absRow }) => {
    const el = document.querySelector('.row[data-row="' + absRow + '"]');
    if (!el) throw new Error('no .row[data-row=' + absRow + '] to focus');
    el.focus();
    el.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
  }, { key, absRow });
}

/** Read the full functional state of a page (production DOM + debug APIs). */
async function readState(page) {
  return page.evaluate(() => {
    const rows = [];
    for (const el of document.querySelectorAll('#rows .row:not(.row-placeholder)')) {
      const abs = Number(el.getAttribute('data-row'));
      const wire = (typeof window.__editchainRowAt === 'function' ? window.__editchainRowAt(abs) : null) || {};
      rows.push({
        index: abs,
        key: el.getAttribute('data-key') || wire.node_key || wire.op_id || wire.git_oid || String(abs),
        lane: wire.lane,
        above: wire.above,
        below: wire.below,
        transitions: wire.transitions,
        isSubop: wire.is_subop === true || el.classList.contains('row-subop'),
      });
    }
    const counter = document.getElementById('search-counter');
    const selected = document.querySelector('.row.row-selected');
    const findCurrent = document.querySelector('.row.row-find-current');
    const messageEl = document.querySelector('#rows .view-message');
    const bannerEl = document.querySelector('.search-banner');
    const log = window.__editchainRequestLog || [];
    const windowOffsets = [];
    const windowHideTraces = [];
    for (const req of log) {
      if (req && typeof req === 'object' && req.GetWindow) {
        windowOffsets.push(req.GetWindow.offset);
        if (req.GetWindow.filter) windowHideTraces.push(req.GetWindow.filter.hide_trace);
      }
    }
    const activeRow = document.activeElement && document.activeElement.closest
      ? (() => {
          const r = document.activeElement.closest('.row');
          return r ? Number(r.getAttribute('data-row')) : null;
        })()
      : null;
    const gpu = window.__editchainGpuDebug
      ? (() => {
          const g = window.__editchainGpuDebug;
          const snap = typeof g.snapshot === 'function' ? g.snapshot() : null;
          // The landed bootstrap creates ONE transparent canvas inside
          // #gpu-canvas-host (positioned over the .graph-cell column) and
          // mirrors one [data-row][data-key] marker per FRAME row into the
          // hidden #gpu-rows element.
          const canvases = document.querySelectorAll('#gpu-canvas-host canvas');
          return {
            present: true,
            dataReady: g.dataReady === true,
            lastError: g.lastError || null,
            backend: typeof g.backend === 'function' ? g.backend() : (snap && snap.backend) || null,
            snapshotRows: snap && Array.isArray(snap.rows) ? snap.rows.length : 0,
            snapshotTotal: snap && typeof snap.total === 'number' ? snap.total : -1,
            hasWhenIdle: typeof g.whenIdle === 'function',
            metrics: typeof g.metrics === 'function' ? g.metrics() : null,
            canvasCount: canvases.length,
            foreignCanvasCount: document.querySelectorAll('canvas:not(#gpu-canvas-host canvas)').length,
            mirrorRows: document.querySelectorAll('#gpu-rows [data-row][data-key]').length,
          };
        })()
      : { present: false };
    return {
      profile: typeof window.__editchainGetProfile === 'function' ? window.__editchainGetProfile() : null,
      total: typeof window.__editchainGetTotal === 'function' ? window.__editchainGetTotal() : -1,
      rows,
      rowKeys: Array.from(document.querySelectorAll('#rows .row')).map((r) => r.getAttribute('data-key')),
      rowCount: document.querySelectorAll('#rows .row:not(.row-placeholder)').length,
      placeholderCount: document.querySelectorAll('#rows .row-placeholder').length,
      counter: counter ? (counter.textContent || '').trim() : '',
      counterBusy: counter ? (counter.getAttribute('aria-busy') || '') : '',
      searchBanner: bannerEl ? (bannerEl.textContent || '').trim() : '',
      header: !!document.querySelector('#rows .tbl-header'),
      warningBanner: !!document.querySelector('#rows .open-warning'),
      message: messageEl
        ? { error: messageEl.classList.contains('error'), text: (messageEl.textContent || '').trim() }
        : null,
      selectedRow: selected ? Number(selected.getAttribute('data-row')) : null,
      selectedKey: selected ? selected.getAttribute('data-key') : null,
      selectedAria: selected ? selected.getAttribute('aria-selected') : null,
      findCurrentRow: findCurrent ? Number(findCurrent.getAttribute('data-row')) : null,
      activeRow,
      windowOffsets,
      windowHideTraces,
      openJsonLog: window.__editchainOpenJsonLog || [],
      gpu,
    };
  });
}

module.exports = {
  EXT_ROOT,
  CHROME,
  BOOT_TIMEOUT_MS,
  IDLE_TIMEOUT_MS,
  suitePrereqs,
  startServer,
  launchBrowser,
  openPages,
  bootScenario,
  settle,
  waitFor,
  installHarnessSpies,
  runSearch,
  clearSearch,
  focusSearch,
  clickProfile,
  clickNav,
  scrollToRow,
  scrollToBottom,
  scrollToTop,
  clickRow,
  clickFirstExpandable,
  pressRowKey,
  readState,
};
