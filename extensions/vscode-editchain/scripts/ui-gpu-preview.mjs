#!/usr/bin/env node
// Deterministic CPU-vs-GPU renderer parity regression oracle (offscreen
// only — NOT the shipped VS Code UI, which is the single "EditChain History"
// panel with the wgpu canvas): the Rust/WASM GPU history renderer
// (media/gpu-preview) against the deprecated SVG/DOM renderer fallback.
//
// Serves the extension directory over a minimal local static HTTP server (no
// external deps — node:http only), opens BOTH the current CPU harness
// (test/harness/index.html) and the GPU harness (test/harness/gpu.html) against
// the SAME scenario, starts both through __editchainSetScenario +
// __editchainStart, waits for each side via its own debug `whenIdle` (no
// arbitrary sleeps), and compares normalized visible rows AND shared
// production functional state:
//
//   absolute index | key/node_key | lane | above | below | transitions | total
//   profile | search mode | view-message state | header | group warnings |
//   rendered row index set | last GetWindow hide_trace
//
// The CPU (deprecated SVG fallback) side is read from the rendered #rows DOM
// enriched with the wire-row geometry the renderer draws
// (window.__editchainRowAt). The GPU side is read
// from window.__editchainGpuDebug.snapshot() (the WASM bootstrap's rendered-row
// contract over the SHARED production #rows DOM) plus a #rows [data-key] DOM
// presence check and the transparent wgpu canvas overlay on .graph-cell.
//
// Scenarios declare an expected end state ('rows' by default; 'empty' and
// 'error' expect the production full-pane message on both sides instead), so
// the runner fails when expected empty/error handling decays rather than on
// the generic "zero rows" path.
//
// Usage:
//   node scripts/ui-gpu-preview.mjs [--scenario merge] [--backend webgl]
//                                  [--viewport 1440x900] [--out DIR] [--shot]
//
// Artifacts written to --out (default ./.ui-out/gpu-<scenario>):
//   parity.json   normalized per-side rows + comparison result
//   metrics.json  renderer metrics from both pages
//   console.txt   browser console output for both pages (incl. page errors)
//   summary.md    scenario/state/counts/errors/parity summary
//   dom.png gpu.png  optional full-page screenshots (--shot)
//
// Exit code is non-zero on usage errors, page errors, GPU errors, missing
// wasm/bootstrap artifacts, zero rows on either side, or parity mismatch.
//
// The comparison helpers (normalizeRow / compareParity / parseArgs) are
// exported so the generic `node --test` suite can contract-test them without
// a built wasm artifact; the real browser check is `npm run ui:gpu`.

import puppeteer from 'puppeteer-core';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const EXT_ROOT = path.join(__dirname, '..');

const CHROME = process.env.CHROME_PATH ||
  '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';

// Same deterministic scenarios the CPU harness supports (see ui-dump.mjs),
// plus `multigroup`: a 600-row virtual window with three explicit group runs
// at KNOWN absolute boundaries (rows 0..99 repo:a, 100..199 repo:b, 200+
// session:s1) so paging/scroll parity can assert group chips land on the
// exact same absolute rows on both sides.
const SCENARIOS = ['empty', 'linear', 'merge', 'mixed', 'sessionBranch', 'filtered', 'undated', 'error', 'warned', 'large', 'longsummary', 'combined', 'traced', 'badges', 'fork', 'highLanes', 'multigroup', 'workUnits', 'workUnitsDeep'];
const BACKENDS = ['webgl', 'webgpu'];

// A scenario's EXPECTED end state. The runner treats the expected state as the
// pass condition: `rows` requires rendered rows + nonempty wgpu geometry,
// `empty` requires the explicit empty-state message on both sides (no rows,
// no geometry), and `error` requires the visible open-failure message on both
// sides. Anything else is reported as a parity failure instead of a generic
// "no rows" failure, so expected empty/error handling cannot silently decay.
const SCENARIO_EXPECTATIONS = Object.freeze({
  empty: 'empty',
  error: 'error',
});

/** Expected end state for a scenario name ('rows' unless declared otherwise). */
export function scenarioExpectation(name) {
  return SCENARIO_EXPECTATIONS[name] || 'rows';
}

// Bounded deadlines — not sleeps. Both debug `whenIdle` implementations poll
// renderer state (in-flight counts, dataReady, stable animation frames), so a
// run settles exactly as fast as the renderer itself and never waits a fixed
// wall-clock duration. Overridable via env for CI/debugging; the defaults are
// generous because the real service Open can take 20s+ on a large chain.
const BOOT_TIMEOUT_MS = Number(process.env.GPU_BOOT_TIMEOUT_MS) || 60_000;
const IDLE_TIMEOUT_MS = Number(process.env.GPU_IDLE_TIMEOUT_MS) || 60_000;
const WEBGPU_DIAGNOSTIC_TIMEOUT_MS = Number(process.env.GPU_WEBGPU_TIMEOUT_MS) || 15_000;
const VERBOSE = process.env.UI_GPU_VERBOSE === '1';

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

// --- pure comparison helpers (exported for node:test) ------------------------

/** Canonicalize one row for parity: numeric absolute index, string key,
 * numeric lane, sorted lane arrays, and a sorted list of directed transition
 * pairs. Accepts both the
 * GPU snapshot naming (`key`, `node_key`) and the wire/DOM naming (`index`,
 * `data-row`) so either side can feed the same normalizer. */
export function normalizeRow(row) {
  const raw = row || {};
  const index = typeof raw.index === 'number' ? raw.index
    : typeof raw.absolute === 'number' ? raw.absolute
    : typeof raw.row === 'number' ? raw.row
    : typeof raw.abs === 'number' ? raw.abs
    : Number.NaN;
  const keyRaw = raw.key !== undefined && raw.key !== null ? raw.key
    : raw.node_key !== undefined && raw.node_key !== null ? raw.node_key
    : raw.node;
  const num = (v) => {
    const n = Number(v);
    return Number.isFinite(n) ? n : 0;
  };
  // Lane sets are canonicalized to unique sorted values: a lane is either
  // present in a row's above/below set or not, and duplicated entries in a
  // fixture's raw payload must not read as a parity difference.
  const uniqueSorted = (arr) => [...new Set((Array.isArray(arr) ? arr : []).map(num).filter((n) => Number.isFinite(n)))].sort((a, b) => a - b);
  // A transition is directed: [child/from lane, parent/to lane]. Preserve the
  // order inside each pair so the comparison catches a renderer that flips a
  // diagonal, while sorting/deduplicating the outer list for stable parity.
  const transitions = [...new Set((Array.isArray(raw.transitions) ? raw.transitions : [])
    .map((tr) => {
      const from = num(Array.isArray(tr) ? tr[0] : (tr && tr.from));
      const to = num(Array.isArray(tr) ? tr[1] : (tr && tr.to));
      return [from, to];
    }).map((pair) => pair.join(',')))].map((pair) => pair.split(',').map(Number))
    .sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  return {
    index,
    key: keyRaw === undefined || keyRaw === null ? String(index) : String(keyRaw),
    lane: num(raw.lane),
    above: uniqueSorted(raw.above),
    below: uniqueSorted(raw.below),
    transitions,
  };
}

/**
 * Compare two normalized row sets. `cpu`/`gpu` are { rows: [...], total }.
 * Rows are keyed by absolute index; every row present on BOTH sides is
 * compared field-for-field (key, lane, above, below, transitions), and the two
 * totals must match. Rows present on only one side are reported as coverage
 * gaps (parity.json / summary.md) but are not themselves failures — the
 * comparison is over the COMMON visible rows, exactly like the harness
 * contract states. `pass` additionally requires at least one common row (an
 * empty overlap means the two renderers showed disjoint views).
 */
export function compareParity(cpu, gpu) {
  const cpuRows = (cpu && Array.isArray(cpu.rows) ? cpu.rows : []).map(normalizeRow);
  const gpuRows = (gpu && Array.isArray(gpu.rows) ? gpu.rows : []).map(normalizeRow);
  const cpuByIndex = new Map(cpuRows.map((r) => [r.index, r]));
  const gpuByIndex = new Map(gpuRows.map((r) => [r.index, r]));
  const commonIndexes = [...cpuByIndex.keys()]
    .filter((i) => gpuByIndex.has(i))
    .sort((a, b) => a - b);
  const gaps = (mine, theirs) => [...mine.keys()]
    .filter((i) => !theirs.has(i))
    .sort((a, b) => a - b)
    .map((i) => ({ index: i, key: mine.get(i).key }));
  const mismatches = [];
  for (const index of commonIndexes) {
    const c = cpuByIndex.get(index);
    const g = gpuByIndex.get(index);
    for (const field of ['key', 'lane', 'above', 'below', 'transitions']) {
      if (JSON.stringify(c[field]) !== JSON.stringify(g[field])) {
        mismatches.push({ index, field, cpuValue: c[field], gpuValue: g[field] });
      }
    }
  }
  const totalsEqual = Number(cpu && cpu.total) === Number(gpu && gpu.total);
  return {
    pass: mismatches.length === 0 && totalsEqual && commonIndexes.length > 0,
    totalsEqual,
    commonRows: commonIndexes.length,
    cpuRows: cpuRows.length,
    gpuRows: gpuRows.length,
    cpuOnlyRows: gaps(cpuByIndex, gpuByIndex),
    gpuOnlyRows: gaps(gpuByIndex, cpuByIndex),
    mismatches,
  };
}

/**
 * Compare the two pages' shared production functional state (both run the same
 * media/main.js, so profile, totals, search mode, message state, header, and
 * group warnings must be identical). Render windows may differ in extent
 * because the GPU status strip changes viewport height; they must overlap and
 * share the same anchor, but need not contain the same invisible buffer tail.
 * This
 * is the "functional parity" layer: geometry could match while the GPU page
 * silently lost a control, a profile, or a message — those fail here.
 */
export function compareFunctionalState(cpu, gpu) {
  const c = cpu && cpu.functional ? cpu.functional : {};
  const g = gpu && gpu.functional ? gpu.functional : {};
  const mismatches = [];
  const scalar = (field) => {
    if (JSON.stringify(c[field]) !== JSON.stringify(g[field])) {
      mismatches.push({ field, cpuValue: c[field], gpuValue: g[field] });
    }
  };
  for (const field of ['profile', 'total', 'searchActive', 'header', 'warningBanner']) {
    scalar(field);
  }
  if (JSON.stringify(c.message || null) !== JSON.stringify(g.message || null)) {
    mismatches.push({ field: 'message', cpuValue: c.message || null, gpuValue: g.message || null });
  }
  const cpuIndexes = Array.isArray(c.rowIndexes) ? c.rowIndexes : [];
  const gpuIndexes = Array.isArray(g.rowIndexes) ? g.rowIndexes : [];
  if (cpuIndexes.length > 0 || gpuIndexes.length > 0) {
    const gpuSet = new Set(gpuIndexes);
    const common = cpuIndexes.filter((index) => gpuSet.has(index));
    const sameAnchor = cpuIndexes[0] === gpuIndexes[0];
    if (common.length === 0 || !sameAnchor) {
      mismatches.push({
        field: 'rowWindow',
        cpuValue: { first: cpuIndexes[0], last: cpuIndexes.at(-1), count: cpuIndexes.length },
        gpuValue: { first: gpuIndexes[0], last: gpuIndexes.at(-1), count: gpuIndexes.length },
      });
    }
  }
  if (c.lastWindowHideTrace !== g.lastWindowHideTrace) {
    mismatches.push({
      field: 'lastWindowHideTrace',
      cpuValue: c.lastWindowHideTrace,
      gpuValue: g.lastWindowHideTrace,
    });
  }
  return { pass: mismatches.length === 0, mismatches };
}

export function parseArgs(argv) {
  const args = { scenario: 'merge', backend: 'webgl', viewport: '1440x900', out: null, shot: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--scenario') args.scenario = argv[++i];
    else if (a === '--backend') args.backend = argv[++i];
    else if (a === '--viewport') args.viewport = argv[++i];
    else if (a === '--out') args.out = argv[++i];
    else if (a === '--shot') args.shot = true;
    else throw new Error('unknown argument: ' + a);
  }
  return args;
}

function parseViewport(vp) {
  const m = /^(\d+)x(\d+)$/.exec(vp);
  if (!m) throw new Error('bad viewport: ' + vp + ' (expected WxH)');
  return { width: +m[1], height: +m[2] };
}

// --- minimal static server (no external deps) --------------------------------

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

// --- browser-side extraction -------------------------------------------------

// NOTE: these extraction functions are serialized INTO the browser page by
// puppeteer, so they must be fully self-contained (no closure over Node-side
// helpers). The functional-state reader is deliberately inlined in each side.

function extractCpuRows() {
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
    });
  }
  const messageEl = document.querySelector('#rows .view-message');
  const counter = document.getElementById('search-counter');
  const log = window.__editchainRequestLog || [];
  let lastWindow = null;
  for (const req of log) {
    if (req && typeof req === 'object' && req.GetWindow) lastWindow = req.GetWindow;
  }
  return {
    rows,
    total: typeof window.__editchainGetTotal === 'function' ? window.__editchainGetTotal() : -1,
    dataReady: window.__editchainDataReady === true,
    functional: {
      profile: typeof window.__editchainGetProfile === 'function'
        ? window.__editchainGetProfile() : null,
      total: typeof window.__editchainGetTotal === 'function'
        ? window.__editchainGetTotal() : -1,
      searchActive: !!(counter && (counter.textContent || '').trim()) ||
        !!document.querySelector('.search-banner'),
      header: !!document.querySelector('#rows .tbl-header'),
      warningBanner: !!document.querySelector('#rows .open-warning'),
      message: messageEl
        ? { error: messageEl.classList.contains('error'), text: (messageEl.textContent || '').trim() }
        : null,
      rowIndexes: Array.from(document.querySelectorAll('#rows .row')).map((el) => {
        const abs = Number(el.getAttribute('data-row'));
        return Number.isFinite(abs) ? abs : -1;
      }).filter((abs) => abs >= 0).sort((a, b) => a - b),
      lastWindowHideTrace: lastWindow && lastWindow.filter
        ? lastWindow.filter.hide_trace
        : undefined,
    },
    metrics: (window.__editchainDebug && typeof window.__editchainDebug.getMetrics === 'function')
      ? window.__editchainDebug.getMetrics()
      : null,
  };
}

function extractGpuRows() {
  const g = window.__editchainGpuDebug;
  const snap = (g && typeof g.snapshot === 'function') ? g.snapshot() : null;
  // The refactored bootstrap creates ONE transparent canvas inside
  // #gpu-canvas-host (positioned over the .graph-cell column) and mirrors one
  // [data-row][data-key] marker per FRAME row into the hidden #gpu-rows
  // element; the VISIBLE rows are the production #rows .row elements that
  // media/main.js renders.
  const canvases = document.querySelectorAll('#gpu-canvas-host canvas');
  const foreignCanvases = document.querySelectorAll('canvas:not(#gpu-canvas-host canvas)');
  const messageEl = document.querySelector('#rows .view-message');
  const counter = document.getElementById('search-counter');
  const log = window.__editchainRequestLog || [];
  let lastWindow = null;
  for (const req of log) {
    if (req && typeof req === 'object' && req.GetWindow) lastWindow = req.GetWindow;
  }
  return {
    rows: snap && Array.isArray(snap.rows) ? snap.rows : [],
    total: snap && typeof snap.total === 'number' ? snap.total : -1,
    backend: g && typeof g.backend === 'function'
      ? g.backend()
      : (snap && snap.backend) || null,
    dataReady: !!g && g.dataReady === true,
    lastError: g ? (g.lastError || null) : 'window.__editchainGpuDebug missing',
    metrics: g && typeof g.metrics === 'function' ? g.metrics() : null,
    domRows: document.querySelectorAll('#rows .row[data-key]').length,
    mirrorRows: document.querySelectorAll('#gpu-rows [data-row][data-key]').length,
    canvas: canvases.length > 0,
    canvasCount: canvases.length,
    foreignCanvasCount: foreignCanvases.length,
    canvasWidth: canvases[0] ? (canvases[0].width || 0) : 0,
    canvasHeight: canvases[0] ? (canvases[0].height || 0) : 0,
    functional: {
      profile: typeof window.__editchainGetProfile === 'function'
        ? window.__editchainGetProfile() : null,
      total: typeof window.__editchainGetTotal === 'function'
        ? window.__editchainGetTotal() : -1,
      searchActive: !!(counter && (counter.textContent || '').trim()) ||
        !!document.querySelector('.search-banner'),
      header: !!document.querySelector('#rows .tbl-header'),
      warningBanner: !!document.querySelector('#rows .open-warning'),
      message: messageEl
        ? { error: messageEl.classList.contains('error'), text: (messageEl.textContent || '').trim() }
        : null,
      rowIndexes: Array.from(document.querySelectorAll('#rows .row')).map((el) => {
        const abs = Number(el.getAttribute('data-row'));
        return Number.isFinite(abs) ? abs : -1;
      }).filter((abs) => abs >= 0).sort((a, b) => a - b),
      lastWindowHideTrace: lastWindow && lastWindow.filter
        ? lastWindow.filter.hide_trace
        : undefined,
    },
  };
}

// --- page lifecycle (deterministic waits only) -------------------------------

async function bootAndSettle(page, kind, scenario, backend = 'webgl') {
  const expectation = scenarioExpectation(scenario);
  if (VERBOSE) console.error('[run] ' + kind + ' setScenario/start: ' + scenario);
  await page.evaluate((name) => {
    window.__editchainSetScenario(name);
    window.__editchainStart();
  }, scenario);
  const waitForSettledMessage = async () => {
    // Expected empty/error scenarios render an explicit full-pane message and
    // never produce rows or wgpu geometry, so whenIdle has nothing to settle.
    // Wait for the production message DOM instead (deterministic, not a sleep).
    await page.waitForFunction(() =>
      !!document.querySelector('#rows .view-message') ||
      (typeof window.__editchainDataReady === 'boolean' &&
        window.__editchainDataReady === true &&
        document.querySelectorAll('#rows .row').length > 0),
    { timeout: BOOT_TIMEOUT_MS });
  };
  if (kind === 'cpu') {
    await page.waitForFunction(() =>
      typeof window.__editchainDebug === 'object' && window.__editchainDataReady === true,
    { timeout: BOOT_TIMEOUT_MS });
    // The timeout is passed into the page context (page.evaluate cannot close
    // over Node-side constants). The OTHER harness page hides this one in the
    // same browser, and layoutProbe's whenIdle settles only after two
    // requestAnimationFrame callbacks — which never fire for a hidden page —
    // so the page is brought to front first (deterministic, not a sleep).
    if (VERBOSE) console.error('[run] cpu idle wait...');
    await page.bringToFront();
    if (expectation === 'rows') {
      await page.evaluate((ms) => window.__editchainDebug.whenIdle(ms), IDLE_TIMEOUT_MS);
    } else {
      await waitForSettledMessage();
    }
    if (VERBOSE) console.error('[run] cpu idle done');
  } else {
    // Capability probes use a bounded timer. Keep the GPU page visible while
    // it boots so Chromium does not throttle that timer merely because the CPU
    // comparison tab was brought forward first.
    await page.bringToFront();
    const startupTimeout = backend === 'webgpu'
      ? Math.min(BOOT_TIMEOUT_MS, WEBGPU_DIAGNOSTIC_TIMEOUT_MS)
      : BOOT_TIMEOUT_MS;
    await page.waitForFunction(() =>
      typeof window.__editchainGpuDebug === 'object' &&
      (window.__editchainGpuDebug.dataReady === true || !!window.__editchainGpuDebug.lastError),
    { timeout: startupTimeout });
    const startupError = await page.evaluate(() => window.__editchainGpuDebug.lastError || null);
    if (startupError) throw new Error('GPU startup failed: ' + startupError);
    if (VERBOSE) console.error('[run] gpu idle wait...');
    await page.bringToFront();
    if (expectation === 'rows') {
      await page.evaluate((ms) => window.__editchainGpuDebug.whenIdle(ms), IDLE_TIMEOUT_MS);
    } else {
      await waitForSettledMessage();
    }
    if (VERBOSE) console.error('[run] gpu idle done');
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!SCENARIOS.includes(args.scenario)) {
    throw new Error('unknown scenario "' + args.scenario + '" — choose from: ' + SCENARIOS.join(', '));
  }
  if (!BACKENDS.includes(args.backend)) {
    throw new Error('unknown backend "' + args.backend + '" — choose from: ' + BACKENDS.join(', '));
  }
  const vp = parseViewport(args.viewport);
  const backendSuffix = args.backend === 'webgl' ? '' : '-' + args.backend;
  const outDir = args.out || path.join(
    EXT_ROOT,
    '.ui-out',
    'gpu-' + args.scenario + backendSuffix,
  );
  fs.mkdirSync(outDir, { recursive: true });

  const server = await startServer(EXT_ROOT);
  const port = server.address().port;
  const baseUrl = 'http://127.0.0.1:' + port;
  const startedMs = Date.now();

  const consoleLines = [];
  const cpuPageErrors = [];
  const gpuPageErrors = [];
  const gpuConsoleErrors = [];
  let browser = null;
  let cpu = null;
  let gpu = null;
  let fatal = null;
  try {
    browser = await puppeteer.launch({
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
    const cpuPage = await browser.newPage();
    const gpuPage = await browser.newPage();
    await cpuPage.setViewport({ width: vp.width, height: vp.height });
    await gpuPage.setViewport({ width: vp.width, height: vp.height });

    const attach = (page, label, pageErrors, consoleErrors) => {
      page.on('console', (m) => {
        consoleLines.push('[' + label + '] ' + m.type() + ': ' + m.text());
        if (VERBOSE) console.error('[' + label + '] ' + m.type() + ': ' + m.text());
        if (m.type() === 'error') consoleErrors.push(m.text());
      });
      page.on('pageerror', (e) => {
        consoleLines.push('[' + label + '-pageerror] ' + e.message);
        if (VERBOSE) console.error('[' + label + '-pageerror] ' + e.message);
        pageErrors.push(e.message);
      });
      page.on('requestfailed', (r) => consoleLines.push(
        '[' + label + '-requestfailed] ' + r.url() + ' ' + (r.failure() ? r.failure().errorText : 'failed')));
    };
    attach(cpuPage, 'cpu', cpuPageErrors, []);
    attach(gpuPage, 'gpu', gpuPageErrors, gpuConsoleErrors);

    await cpuPage.goto(baseUrl + '/test/harness/index.html', { waitUntil: 'networkidle0', timeout: BOOT_TIMEOUT_MS });
    await gpuPage.goto(baseUrl + '/test/harness/gpu.html?backend=' + args.backend, { waitUntil: 'networkidle0', timeout: BOOT_TIMEOUT_MS });

    await bootAndSettle(cpuPage, 'cpu', args.scenario);
    // Capture the CPU side immediately after it settles: if the GPU side then
    // fails to boot, the artifacts still show exactly what the CPU renderer
    // produced for the same scenario.
    cpu = await cpuPage.evaluate(extractCpuRows);
    await bootAndSettle(gpuPage, 'gpu', args.scenario, args.backend);
    gpu = await gpuPage.evaluate(extractGpuRows);

    if (args.shot) {
      await cpuPage.screenshot({ path: path.join(outDir, 'dom.png'), fullPage: true });
      await gpuPage.screenshot({ path: path.join(outDir, 'gpu.png'), fullPage: true });
    }
  } catch (err) {
    const gpuReady = typeof gpu === 'object' && gpu !== null;
    const hint = !gpuReady && args.backend === 'webgpu'
      ? ' — no WebGPU adapter became available; use --backend webgl for the deterministic baseline'
      : !gpuReady
      ? ' — is media/gpu-preview/bootstrap.js + pkg/editchain_gpu_preview.* built and served over HTTP (server port ' + port + ')?'
      : '';
    fatal = (err && err.message || String(err)) + hint;
  } finally {
    if (browser) await browser.close();
    server.close();
  }

  // --- failure analysis -------------------------------------------------------
  const expectation = scenarioExpectation(args.scenario);
  const problems = [];
  if (fatal) problems.push(fatal);
  if (cpuPageErrors.length) problems.push(cpuPageErrors.length + ' CPU page error(s)');
  if (gpuPageErrors.length) problems.push(gpuPageErrors.length + ' GPU page error(s)');
  if (gpuConsoleErrors.length && gpu && !gpu.lastError) {
    problems.push(gpuConsoleErrors.length + ' GPU console error(s)');
  }
  const rowsExpected = expectation === 'rows';
  if (rowsExpected) {
    if (cpu && cpu.rows.length === 0) {
      problems.push('no rows: CPU page rendered 0 rows for scenario "' + args.scenario + '"');
    }
    if (gpu && gpu.rows.length === 0) {
      problems.push('no rows: GPU snapshot is empty for scenario "' + args.scenario + '"');
    }
    if (gpu && gpu.domRows === 0) {
      problems.push('no rows: #rows contains no [data-key] elements (main.js must render production DOM rows)');
    }
    if (gpu && gpu.mirrorRows !== gpu.rows.length) {
      problems.push('GPU mirror/snapshot row count differs (#gpu-rows=' + gpu.mirrorRows + ' snapshot=' + gpu.rows.length + ')');
    }
    if (gpu && !gpu.canvas) {
      problems.push('GPU canvas is missing (no canvas inside #gpu-canvas-host)');
    }
    if (gpu && (gpu.canvasWidth <= 0 || gpu.canvasHeight <= 0)) {
      problems.push('GPU canvas has invalid backing dimensions ' + gpu.canvasWidth + 'x' + gpu.canvasHeight);
    }
    if (gpu && gpu.rows.length > 0 && (
      !gpu.metrics || gpu.metrics.renderCount < 1 || gpu.metrics.vertexCount < 1
    )) {
      problems.push('GPU renderer submitted no non-empty geometry');
    }
  }
  if (gpu && gpu.foreignCanvasCount > 0) {
    problems.push('GPU canvas overlay leaks outside #gpu-canvas-host (' + gpu.foreignCanvasCount + ' foreign canvas(es))');
  }
  if (gpu && gpu.backend !== args.backend) {
    problems.push('GPU backend differs from explicit request (requested=' + args.backend + ' actual=' + gpu.backend + ')');
  }
  if (gpu && gpu.lastError) {
    problems.push('GPU error: ' + gpu.lastError);
  }
  const parity = (cpu && gpu) ? compareParity(cpu, gpu) : null;
  const functional = (cpu && gpu) ? compareFunctionalState(cpu, gpu) : null;
  if (functional && !functional.pass) {
    problems.push('functional state mismatch: ' +
      functional.mismatches.map((m) =>
        m.field + ' (cpu=' + JSON.stringify(m.cpuValue) + ' gpu=' + JSON.stringify(m.gpuValue) + ')')
      .join('; '));
  }
  if (rowsExpected && parity && !parity.pass) {
    const why = [];
    if (!parity.totalsEqual) why.push('totals differ (cpu=' + cpu.total + ' gpu=' + gpu.total + ')');
    if (parity.commonRows === 0) why.push('no common rows (cpu=' + cpu.rows.length + ' gpu=' + gpu.rows.length + ')');
    why.push(parity.mismatches.length + ' field mismatch(es) on common rows: ' +
      JSON.stringify(parity.mismatches.slice(0, 5)));
    problems.push('parity mismatch: ' + why.join('; '));
  }
  if (!rowsExpected && (cpu && gpu)) {
    // Expected empty/error: the MESSAGE state itself is the parity surface —
    // both sides must show the same production message (same error flag and
    // text), not just both be "not rows".
    const cMsg = cpu.functional && cpu.functional.message;
    const gMsg = gpu.functional && gpu.functional.message;
    if (!cMsg || !gMsg) {
      problems.push('expected ' + expectation + ' state: missing .view-message on cpu=' +
        JSON.stringify(cMsg) + ' gpu=' + JSON.stringify(gMsg));
    } else if (cMsg.error !== gMsg.error || cMsg.text !== gMsg.text) {
      problems.push('message state mismatch (expected ' + expectation + '): cpu=' +
        JSON.stringify(cMsg) + ' gpu=' + JSON.stringify(gMsg));
    }
  }

  // --- artifacts --------------------------------------------------------------
  const elapsedMs = Date.now() - startedMs;
  const runState = {
    scenario: args.scenario,
    backend: args.backend,
    viewport: args.viewport,
    baseUrl,
    outDir,
    elapsedMs,
    pageErrors: { cpu: cpuPageErrors.length, gpu: gpuPageErrors.length },
  };
  const parityJson = {
    scenario: args.scenario,
    backend: args.backend,
    viewport: args.viewport,
    cpu: cpu ? {
      rows: cpu.rows.map(normalizeRow),
      total: cpu.total,
      dataReady: cpu.dataReady,
      functional: cpu.functional,
      metrics: cpu.metrics,
    } : null,
    gpu: gpu ? {
      rows: gpu.rows.map(normalizeRow),
      total: gpu.total,
      backend: gpu.backend,
      dataReady: gpu.dataReady,
      lastError: gpu.lastError,
      domRows: gpu.domRows,
      mirrorRows: gpu.mirrorRows,
      canvas: gpu.canvas,
      canvasCount: gpu.canvasCount,
      foreignCanvasCount: gpu.foreignCanvasCount,
      canvasWidth: gpu.canvasWidth,
      canvasHeight: gpu.canvasHeight,
      functional: gpu.functional,
      metrics: gpu.metrics,
    } : null,
    parity,
    functional,
    problems,
  };
  fs.writeFileSync(path.join(outDir, 'parity.json'), JSON.stringify(parityJson, null, 2));
  fs.writeFileSync(path.join(outDir, 'metrics.json'), JSON.stringify({
    run: runState,
    cpu: cpu ? cpu.metrics : null,
    gpu: gpu ? gpu.metrics : null,
  }, null, 2));
  fs.writeFileSync(path.join(outDir, 'console.txt'), consoleLines.join('\n') + (consoleLines.length ? '\n' : ''));

  const summary = [
    '# GPU renderer parity (offscreen oracle) — ' + args.scenario,
    '',
    '- scenario: ' + args.scenario,
    '- backend: ' + (gpu ? (gpu.backend || args.backend) : args.backend) + ' (requested ' + args.backend + ')',
    '- viewport: ' + args.viewport,
    '- url: ' + baseUrl,
    '- elapsed: ' + elapsedMs + 'ms',
    '',
    '## CPU renderer (test/harness/index.html)',
    '',
    cpu ? [
      '- rows rendered: ' + cpu.rows.length,
      '- total: ' + cpu.total,
      '- dataReady: ' + cpu.dataReady,
      '- functional: ' + JSON.stringify(cpu.functional),
    ] : ['- not reached'],
    '',
    '## GPU harness (test/harness/gpu.html, offscreen oracle)',
    '',
    gpu ? [
      '- rows in snapshot: ' + gpu.rows.length,
      '- rows in #rows [data-key]: ' + gpu.domRows,
      '- rows in #gpu-rows mirror: ' + gpu.mirrorRows,
      '- canvas in #gpu-canvas-host: ' + gpu.canvas + ' (' + gpu.canvasCount + ')',
      '- total: ' + gpu.total,
      '- dataReady: ' + gpu.dataReady,
      '- lastError: ' + (gpu.lastError || 'none'),
      '- backend: ' + (gpu.backend || 'unknown'),
      '- functional: ' + JSON.stringify(gpu.functional),
    ] : ['- not reached'],
    '',
    '## Parity',
    '',
    parity ? [
      '- pass: ' + parity.pass,
      '- common rows compared: ' + parity.commonRows,
      '- totals equal: ' + parity.totalsEqual,
      '- CPU-only rows: ' + parity.cpuOnlyRows.length + ' ' + JSON.stringify(parity.cpuOnlyRows.slice(0, 8)),
      '- GPU-only rows: ' + parity.gpuOnlyRows.length + ' ' + JSON.stringify(parity.gpuOnlyRows.slice(0, 8)),
      '- mismatches: ' + parity.mismatches.length,
      ...parity.mismatches.slice(0, 20).map((m) =>
        '- row ' + m.index + ' ' + m.field + ': cpu=' + JSON.stringify(m.cpuValue) + ' gpu=' + JSON.stringify(m.gpuValue)),
    ] : ['- not computed'],
    '## Functional state parity',
    '',
    functional ? [
      '- pass: ' + functional.pass,
      ...functional.mismatches.map((m) =>
        '- ' + m.field + ': cpu=' + JSON.stringify(m.cpuValue) + ' gpu=' + JSON.stringify(m.gpuValue)),
    ] : ['- not computed'],
    '',
    '## Problems',
    '',
    problems.length ? problems.map((p) => '- ' + p) : ['_none_'],
    '',
    '## Console (' + consoleLines.length + ' lines)',
    '',
    ...consoleLines.slice(0, 200),
  ].flat();
  fs.writeFileSync(path.join(outDir, 'summary.md'), summary.join('\n') + '\n');

  console.log('gpu-parity scenario=' + args.scenario + ' backend=' + args.backend + ' viewport=' + args.viewport);
  if (parity) {
    console.log('parity pass=' + parity.pass + ' common=' + parity.commonRows +
      ' cpu=' + parity.cpuRows + ' gpu=' + parity.gpuRows + ' totalsEqual=' + parity.totalsEqual);
  }
  console.log('page errors: cpu=' + cpuPageErrors.length + ' gpu=' + gpuPageErrors.length);
  if (gpu) console.log('gpu backend=' + (gpu.backend || 'unknown') + ' dataReady=' + gpu.dataReady +
    ' rows=' + gpu.rows.length + ' total=' + gpu.total + ' lastError=' + (gpu.lastError || 'none'));
  console.log('artifacts -> ' + outDir);

  if (problems.length) {
    console.error('GPU PARITY FAILED:');
    problems.forEach((p) => console.error('- ' + p));
    process.exit(1);
  }
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  main().catch((err) => {
    console.error(err && err.stack || err);
    process.exit(1);
  });
}
