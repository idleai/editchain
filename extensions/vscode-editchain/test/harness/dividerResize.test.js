// Deterministic regression test for graph-lane divider invariance (offscreen
// regression oracle — NOT the shipped VS Code UI, which is the single
// "EditChain History" panel with the wgpu canvas): resizing the graph column
// divider left/right must change ONLY the column width — lane X positions must
// stay exactly where they were. The deprecated SVG fallback cells read lane
// positions from `laneX()`, and the Rust/WASM GPU renderer serializes the
// same positions through the shared adapter
// (window.__editchainGraphAdapter.laneXAll -> media/gpu-preview/bootstrap.js
// graph.lane_x), so asserting adapter lane positions AND rendered dot cx covers
// both the live SVG DOM and the GPU frame input verbatim.
//
// Bug fixed: the default Pulse lane pitch (LANE_W x 0.82 = 14.76px) was
// CONDITIONAL on divider state — the first divider drag switched the column to
// unscaled 18px spacing, and dragging the column narrower than the lane extent
// re-distributed every lane centre proportionally across the dragged width —
// so touching the divider rescrambled the graph topology (lane X
// [14.76, 29.52] natural -> [18, 36] after the first drag pixel, and squashed
// toward the left edge when dragged narrower than numLanes x 14.76). The pitch
// is now a fixed constant applied in every divider state, so the default
// [14.76, 29.52, ...] positions are preserved AND invariant.
//
// Run:  node --test test/harness/dividerResize.test.js
//
// The GPU-page test boots over HTTP exactly like functionalParity.test.js and
// skips gracefully when Chrome or the built GPU assets are missing, so the
// generic `node --test test/harness/*.test.js` run stays green without them.
'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const puppeteer = require('puppeteer-core');
const driver = require('./functionalDriver.js');

const EXT_ROOT = path.join(__dirname, '..', '..');
const HARNESS_CPU = 'file://' + path.join(EXT_ROOT, 'test', 'harness', 'index.html');
const CHROME = process.env.CHROME_PATH || driver.CHROME;

// The default Pulse lane pitch: `LANE_W * LANE_W_PULSE_SCALE` = 18 * 0.82.
const LANE_PITCH = 14.76;
const LANE_X_EXPECTED = [14.76, 29.52, 44.28, 59.04];
// 4 x 14.76 accumulates float error (59.040000000000006), so compare lane
// positions with a tiny epsilon; the invariance assertions below stay strict.
const near = (a, b) => Math.abs(a - b) <= 1e-6;

/** Assert a laneXAll array matches the expected positions within float error. */
function assertLaneXs(actual, expected, label) {
  assert.ok(Array.isArray(actual) && actual.length === expected.length,
    label + ': expected ' + expected.length + ' lanes, got ' + JSON.stringify(actual));
  actual.forEach((x, i) => assert.ok(near(x, expected[i]),
    label + ': lane ' + i + ' x=' + x + ' expected ' + expected[i]));
}

const PREREQS = driver.suitePrereqs();
const SKIP = PREREQS.ok ? false : PREREQS.reason;
const GPU_ASSETS_OK = [
  'bootstrap.js',
  'pkg/editchain_gpu_preview.js',
  'pkg/editchain_gpu_preview_bg.wasm',
].every((rel) => fs.existsSync(path.join(driver.EXT_ROOT, 'media/gpu-preview', rel)));
const GPU_SKIP = SKIP || (GPU_ASSETS_OK ? false : 'built GPU assets missing (run npm run build:gpu)');

let browser = null;
let server = null;
let baseUrl = '';

before(async () => {
  if (!PREREQS.ok) return;
  browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: 'new',
    args: ['--no-sandbox', '--disable-setuid-sandbox'],
  });
  server = await driver.startServer(driver.EXT_ROOT);
  baseUrl = 'http://127.0.0.1:' + server.address().port;
});

after(async () => {
  if (browser) await browser.close();
  if (server) server.close();
});

/** Boot a scenario through the real harness startup handshake and settle. */
async function bootCpuPage(page, scenario) {
  await page.evaluate((name) => {
    window.__editchainSetScenario(name);
    window.__editchainStart();
  }, scenario);
  await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
}

/** Read lane geometry: adapter laneXAll (the GPU frame input), lane width,
 * effective graph column width, rendered SVG dot cx values, and the first
 * cell's SVG width. */
function captureLaneGeometry(page) {
  return page.evaluate(() => {
    const dots = [];
    document.querySelectorAll('.row .graph-cell circle.graphDot')
      .forEach((d) => dots.push(+d.getAttribute('cx')));
    const cell = document.querySelector('.row .graph-cell svg.graphCell');
    const adapter = window.__editchainGraphAdapter || null;
    return {
      laneXAll: adapter ? adapter.laneXAll() : null,
      laneWidth: adapter ? adapter.laneWidth() : null,
      graphWidth: adapter ? adapter.graphWidth() : null,
      dots,
      svgW: cell ? +cell.getAttribute('width') : null,
    };
  });
}

/** Drag the graph column divider by `deltaX` CSS px through the real handle
 * (mousedown -> mousemove -> mouseup on .col-resize-handle[data-col="graph"]),
 * then settle the rebuild the drag triggers on `kind` ('cpu' | 'gpu'). */
async function dragGraphDivider(page, kind, deltaX) {
  await page.evaluate((delta) => {
    const handle = document.querySelector('.col-resize-handle[data-col="graph"]');
    if (!handle) throw new Error('graph column resize handle missing');
    const r = handle.getBoundingClientRect();
    const startX = r.left + r.width / 2;
    const startY = r.top + r.height / 2;
    handle.dispatchEvent(new MouseEvent('mousedown', {
      bubbles: true, cancelable: true, clientX: startX, clientY: startY, button: 0,
    }));
    window.dispatchEvent(new MouseEvent('mousemove', {
      bubbles: true, cancelable: true, clientX: startX + delta, clientY: startY, button: 0,
    }));
    window.dispatchEvent(new MouseEvent('mouseup', {
      bubbles: true, cancelable: true, clientX: startX + delta, clientY: startY, button: 0,
    }));
  }, deltaX);
  await driver.settle(page, kind);
}

test('divider resize prerequisites (Chrome available)', { skip: SKIP }, () => {});

test('lane X positions stay invariant when the graph column divider is resized (SVG + GPU adapter)',
  { skip: SKIP }, async () => {
    const page = await browser.newPage();
    try {
      await page.setViewport({ width: 1440, height: 900 });
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      await page.goto(HARNESS_CPU, { waitUntil: 'networkidle0' });
      await bootCpuPage(page, 'large'); // 600 rows, lanes 0..3 (maxLane 3)

      const before = await captureLaneGeometry(page);
      assert.ok(before.laneXAll && before.laneXAll.length === 4,
        'large scenario exposes lanes 0..3, got ' + JSON.stringify(before.laneXAll));
      assertLaneXs(before.laneXAll, LANE_X_EXPECTED,
        'lanes sit at (lane+1) x Pulse pitch (LANE_W x 0.82) before any divider touch');
      assert.equal(before.laneWidth, LANE_PITCH, 'lane width is the fixed Pulse pitch (LANE_W x 0.82)');
      assert.ok(before.dots.length > 0, 'rendered SVG dots exist to measure');

      // Drag WIDER: the column must widen without moving a single lane centre.
      // Before this fix the very first drag pixel switched Pulse's compressed
      // pitch back to unscaled 18px, jumping every lane ~22% right.
      await dragGraphDivider(page, 'cpu', +90);
      const wider = await captureLaneGeometry(page);
      assert.ok(wider.graphWidth > before.graphWidth + 80,
        'divider drag actually widened the column, got ' + before.graphWidth + ' -> ' + wider.graphWidth);
      assertLaneXs(wider.laneXAll, LANE_X_EXPECTED,
        'adapter laneXAll unchanged after dragging the divider wider (GPU frame input invariant)');
      assert.equal(wider.laneWidth, before.laneWidth, 'lane width unchanged after divider drag');
      assert.deepEqual(wider.dots, before.dots, 'rendered SVG dot cx unchanged after divider drag');
      assert.ok(wider.svgW > before.svgW,
        'SVG cell width tracks the wider column, got ' + before.svgW + ' -> ' + wider.svgW);

      // Drag NARROWER than the lanes' natural extent (numLanes x 14.76 ≈ 59px
      // vs the 40px column floor): the column clips lanes at the edge instead
      // of re-distributing centres proportionally across the dragged width;
      // lane positions must not move a pixel either way.
      await dragGraphDivider(page, 'cpu', -250);
      const narrower = await captureLaneGeometry(page);
      assert.ok(narrower.graphWidth < wider.graphWidth - 80,
        'second drag actually narrowed the column, got ' + wider.graphWidth + ' -> ' + narrower.graphWidth);
      assert.equal(narrower.graphWidth, 40, 'narrow drag clamps at MIN_COL_W.graph');
      assert.ok(narrower.graphWidth < 4 * before.laneWidth,
        'column is now narrower than the lane extent, so the old proportional squeeze would fire');
      assertLaneXs(narrower.laneXAll, LANE_X_EXPECTED,
        'adapter laneXAll unchanged after a narrow divider drag (no proportional re-distribution)');
      assert.deepEqual(narrower.dots, before.dots, 'rendered SVG dot cx unchanged after narrow divider drag');
      assert.equal(narrower.svgW, narrower.graphWidth, 'SVG cell width tracks the narrow column');
      assert.deepEqual(errors, [], 'page errors: ' + JSON.stringify(errors));
    } finally {
      await page.close();
    }
  });

test('GPU page serializes the same invariant lane positions through the shared adapter',
  { skip: GPU_SKIP }, async () => {
    const gpu = await browser.newPage();
    try {
      await gpu.setViewport({ width: 1440, height: 900 });
      const errors = [];
      gpu.on('pageerror', (e) => errors.push(e.message));
      await gpu.goto(baseUrl + '/test/harness/gpu.html?backend=webgl', { waitUntil: 'networkidle0' });
      await driver.bootScenario(gpu, 'gpu', 'large');

      const before = await captureLaneGeometry(gpu);
      assert.ok(before.laneXAll && before.laneXAll.length === 4,
        'GPU page adapter exposes lanes 0..3, got ' + JSON.stringify(before.laneXAll));
      assertLaneXs(before.laneXAll, LANE_X_EXPECTED,
        'GPU frame lane_x starts at the invariant default Pulse pitch');

      await dragGraphDivider(gpu, 'gpu', +90);
      const wider = await captureLaneGeometry(gpu);
      assert.ok(wider.graphWidth > before.graphWidth + 80, 'GPU column widened by the drag');
      assertLaneXs(wider.laneXAll, LANE_X_EXPECTED, 'GPU frame lane_x unchanged after wider drag');

      await dragGraphDivider(gpu, 'gpu', -250);
      const narrower = await captureLaneGeometry(gpu);
      assert.equal(narrower.graphWidth, 40, 'GPU column narrowed and clamped at MIN_COL_W.graph');
      assertLaneXs(narrower.laneXAll, LANE_X_EXPECTED, 'GPU frame lane_x unchanged after narrow drag');

      const gpuDebug = await gpu.evaluate(() => window.__editchainGpuDebug || null);
      assert.ok(gpuDebug && gpuDebug.dataReady, 'GPU page booted the WASM renderer pipeline');
      assert.deepEqual(errors, [], 'GPU page errors: ' + JSON.stringify(errors));
    } finally {
      await gpu.close();
    }
  });
