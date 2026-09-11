// Browser driver for the Rust/WASM history adapter smoke suite
// (test/harness/rustSmoke.test.js). Every helper drives the REAL Rust-owned
// renderer through the fixture page test/harness/rust.html: the page loads
// media/rust-history/loader.js as its only bootstrap, the Rust shell owns the
// DOM, the per-row SVG graph fragments, and find-in-chain, and the loader
// mirrors the wasm-bindgen debug exports as a read-only
// window.__editchainRendererDebug facade (loader: 'rust-history', dataReady,
// lastError, backend, snapshot, metrics, laneXAll, whenIdle).
//
// Everything waits on concrete renderer state (debug whenIdle / DOM
// predicates) — never wall-clock sleeps — so the suite is deterministic.

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const puppeteer = require('puppeteer-core');

const EXT_ROOT = path.join(__dirname, '..', '..');

const DEFAULT_CHROME =
  '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';
const CHROME = process.env.CHROME_PATH || DEFAULT_CHROME;

const BOOT_TIMEOUT_MS = Number(process.env.RENDERER_BOOT_TIMEOUT_MS) || 60_000;
const IDLE_TIMEOUT_MS = Number(process.env.RENDERER_IDLE_TIMEOUT_MS) || 60_000;

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

/** Headless Chrome for the deterministic SVG renderer tests. */
function launchBrowser() {
  return puppeteer.launch({
    executablePath: CHROME,
    headless: 'new',
    args: [
      '--no-sandbox',
      '--disable-setuid-sandbox',
    ],
  });
}

/** Wait for a page-side predicate (serializable function body). */
async function waitFor(page, fn, opts) {
  opts = opts || {};
  // Timer polling keeps page-side predicates live when Chromium throttles
  // requestAnimationFrame in the background tab.
  await page.waitForFunction(fn, {
    timeout: opts.timeout || BOOT_TIMEOUT_MS,
    polling: 50,
  }, ...(opts.args || []));
}

/** Wrap postMessage on a page so raw-JSON and diff identities are captured. */
async function installHarnessSpies(page) {
  await page.evaluate(() => {
    const orig = window.vscode.postMessage.bind(window.vscode);
    window.__editchainOpenJsonLog = [];
    window.__editchainOpenDiffLog = [];
    window.vscode.postMessage = function (msg) {
      if (msg && msg.type === 'openJson') {
        window.__editchainOpenJsonLog.push({
          type: 'openJson',
          snapshot_id: msg.snapshot_id,
          op_id: msg.op_id !== undefined ? msg.op_id : null,
          git_oid: msg.git_oid !== undefined ? msg.git_oid : null,
          repository: msg.repository !== undefined ? msg.repository : null,
        });
      }
      if (msg && msg.type === 'openDiff') {
        window.__editchainOpenDiffLog.push(msg);
      }
      return orig(msg);
    };
  });
}

/** Fill + Enter on the real #search control (the keyboard path). */
async function runSearch(page, query) {
  await page.evaluate((q) => {
    const input = document.getElementById('search');
    input.value = q;
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  }, query);
}

/** Clear the search input through the real input handler. */
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

/** Real-mouse click on a find-navigation button (mousedown + click pipeline). */
async function clickNav(page, which) {
  await page.bringToFront();
  await page.click('#' + (which === 'prev' ? 'search-prev' : 'search-next'));
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

/** Read the full functional state of the rust.html page (production DOM +
 * the Rust loader's __editchainRendererDebug facade). */
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
    const log = window.__editchainRequestLog || [];
    const windowOffsets = [];
    for (const req of log) {
      if (req && typeof req === 'object' && req.GetWindow) {
        windowOffsets.push(req.GetWindow.offset);
      }
    }
    const activeRow = document.activeElement && document.activeElement.closest
      ? (() => {
          const r = document.activeElement.closest('.row');
          return r ? Number(r.getAttribute('data-row')) : null;
        })()
      : null;
    return {
      total: typeof window.__editchainGetTotal === 'function' ? window.__editchainGetTotal() : -1,
      rows,
      rowKeys: Array.from(document.querySelectorAll('#rows .row')).map((r) => r.getAttribute('data-key')),
      rowCount: document.querySelectorAll('#rows .row:not(.row-placeholder)').length,
      placeholderCount: document.querySelectorAll('#rows .row-placeholder').length,
      counter: counter ? (counter.textContent || '').trim() : '',
      counterBusy: counter ? (counter.getAttribute('aria-busy') || '') : '',
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
      openJsonLog: window.__editchainOpenJsonLog || [],
      openDiffLog: window.__editchainOpenDiffLog || [],
    };
  });
}

module.exports = {
  EXT_ROOT,
  CHROME,
  BOOT_TIMEOUT_MS,
  IDLE_TIMEOUT_MS,
  startServer,
  launchBrowser,
  waitFor,
  installHarnessSpies,
  runSearch,
  clearSearch,
  focusSearch,
  clickNav,
  clickRow,
  clickFirstExpandable,
  pressRowKey,
  readState,
};
