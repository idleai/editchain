// Production overlay bridge for the default Rust/WASM GPU history renderer.
//
// This module runs ALONGSIDE the production renderer (media/main.js) in the
// GPU webview and the GPU harness (test/harness/gpu.html). Production owns the
// controls, profile persistence, virtual paging (PAGE=500), FindInHistory
// search/nav, loading/error, work-unit/bundle/promotion rows, row
// selection/keyboard/disclosure, raw JSON routing, five responsive columns,
// accessibility and resize. This bridge ONLY:
//
//   - hides the per-row SVG graph cells (body.gpu-canvas-active) — the GPU
//     canvas visually replaces the graph, while the DOM rows and all their
//     text cells/interactions stay untouched;
//   - overlays ONE transparent canvas aligned over the rendered .graph-cell
//     column (pointer-events: none, below the sticky header);
//   - observes the production row DOM, scroll, resize and mutations;
//   - gathers the currently rendered row DTOs from window.__editchainRowAt and
//     their ACTUAL DOM rects, serializes ONE frame object, and submits it via
//     renderer.render(JSON.stringify(frame), width, height, scale);
//   - mirrors lightweight [data-row][data-key] markers into #gpu-rows so the
//     harness/e2e DOM contract stays intact;
//   - exposes window.__editchainGpuDebug (dataReady, lastError, backend,
//     snapshot, metrics, whenIdle) and emits the gpuPreviewReady handshake.
//
// Frame contract (serialized as ONE JSON object; all geometry is CSS pixels in
// the canvas coordinate space, so Rust never re-derives viewport/breakpoint
// math):
//
//   {
//     "canvas": { "width": <backing px>, "height": <backing px>,
//                 "scale": <backing/CSS factor> },
//     "graph": {
//       "left": 0,                      // canvas is aligned to the graph column
//       "width": <graph column CSS px>,
//       "lane_x": [<CSS px per lane 0..maxLane, production-computed>],
//       "dot_radius": <CSS px>,
//       "line_width": <CSS px>,
//       "bundle_half_height": <CSS px>,
//       "bundle_margin": <CSS px>,
//       "background_color": "<css color>"   // production editor background
//     },
//     "rows": [{
//       "index": <absolute row index>,
//       "top": <CSS px>, "bottom": <CSS px>, "middle": <CSS px>,
//       "lane": <u32>, "above": [u32...], "below": [u32...],
//       "transitions": [[fromLane, toLane]...],   // directed child->parent
//       "is_subop": bool, "is_bundle": bool
//     }...]
//   }

// @ts-ignore — VS Code provides this global; production main.js acquires it
// first and shares it on window.__editchainVscode (the harness installs the
// same acquireVsCodeApi before either renderer runs).
const vscode = window.__editchainVscode || (window.__editchainVscode = acquireVsCodeApi());
const body = document.body;
const rowsElement = document.getElementById('rows');
const layoutElement = document.getElementById('layout');
const canvasHost = document.getElementById('gpu-canvas-host');
// The canvas is a runtime-created GPU surface — never part of the production
// scaffold. The id (resolved by GpuRenderer.create) keeps the harness/e2e
// `#gpu-canvas-host canvas` contract intact.
const canvas = document.createElement('canvas');
canvas.id = 'gpu-canvas';
canvasHost.appendChild(canvas);
const mirrorElement = document.getElementById('gpu-rows');
const backendElement = document.getElementById('gpu-backend');
const statusElement = document.getElementById('gpu-status');
const liveElement = document.getElementById('gpu-live');

// Mirrors the production renderer's fixed row height (adapter.rowHeight).
const ROW_H = 34;
// Cap the backing surface edge so a long/narrow window never exceeds common
// WebGL texture limits; the CSS surface keeps its true graph-column size and
// the Rust renderer scales geometry by `scale`.
const MAX_SURFACE_EDGE = 2048;
// Bounded WebGPU capability probe (explicit negative diagnostics).
const WEBGPU_PROBE_TIMEOUT_MS = 10_000;
// Overscan rows included above/below the visible viewport so lane lines stay
// continuous at the canvas edges.
const OVERSCAN_ROWS = 1;

let renderer = null;
let wasmModule = null;
let rendererReady = false;
let rendererStarting = false;
let requestedBackend = 'auto';
let actualBackend = null;
let generation = 0;
let rendering = false;
let frameScheduled = false;
let lastSnapshotRows = [];
const startedAt = performance.now();
const metricsState = {
  initMs: null,
  firstWindowMs: null,
  lastRenderMs: null,
  renderCount: 0,
  vertexCount: 0,
  domRows: 0,
};

const debug = {
  dataReady: false,
  lastError: null,
  backend: () => actualBackend,
  snapshot: () => ({
    rows: lastSnapshotRows.map((row) => ({ ...row })),
    total: productionTotal(),
    backend: actualBackend,
  }),
  metrics: () => ({
    ...metricsState,
    generation,
    rendererReady,
    dataReady: debug.dataReady,
  }),
  whenIdle: (timeoutMs = 60000) => waitUntilIdle(timeoutMs),
};
window.__editchainGpuDebug = debug;

// Diagnostics: log wgpu surface/device stages as the Rust create() sets them.
const stageObserver = new MutationObserver(() => {
  const stage = canvas.getAttribute('data-gpu-stage');
  if (stage) console.info('[editchain-gpu] wgpu stage:', stage);
});
stageObserver.observe(canvas, { attributes: true, attributeFilter: ['data-gpu-stage'] });

function setStatus(text) {
  statusElement.textContent = text;
  liveElement.textContent = text;
  vscode.postMessage({ type: 'gpuPreviewStatus', text });
}

/** Terminal initialization failure (missing assets, WebGPU probe, wasm
 *  instantiation, surface/device creation). The production SVG graphs stay
 *  visible; the toolbar reports the explicit error. */
function fail(error) {
  const message = error instanceof Error ? error.message : String(error);
  debug.lastError = message;
  debug.dataReady = true;
  rendering = false;
  body.classList.remove('gpu-canvas-active');
  setStatus('error');
  vscode.postMessage({ type: 'gpuPreviewLog', text: message });
  console.error('[editchain-gpu]', message);
}

/** The authoritative production total (from main.js), 0 when not available. */
function productionTotal() {
  const totalFn = window.__editchainGetTotal;
  return typeof totalFn === 'function' && Number.isFinite(Number(totalFn()))
    ? Number(totalFn())
    : 0;
}

/** Round frame coordinates like the production SVG formatter (2 decimals),
 *  keeping serialized frames stable across identical DOM states. */
function fmt(value) {
  return Math.round(value * 100) / 100;
}

function finiteNum(value) {
  const n = Number(value);
  return Number.isFinite(n) ? n : 0;
}

function numberList(value) {
  return Array.isArray(value)
    ? value.map(Number).filter(Number.isFinite)
    : [];
}

/** Directed (from, to) transition pairs, production order untouched. */
function transitionList(value) {
  if (!Array.isArray(value)) return [];
  return value.flatMap((pair) => {
    if (!Array.isArray(pair) || pair.length < 2) return [];
    const from = Number(pair[0]);
    const to = Number(pair[1]);
    return Number.isFinite(from) && Number.isFinite(to) ? [[from, to]] : [];
  });
}

function editorBackground() {
  const value = getComputedStyle(document.documentElement)
    .getPropertyValue('--vscode-editor-background').trim();
  return value || '#0e1116';
}

/** The rendered sticky header's graph column header cell, or null before the
 *  table scaffold exists (loading/error views have no header). */
function graphHeaderCell() {
  const header = rowsElement.querySelector('.tbl-header');
  return header ? header.querySelector('.th.graph') : null;
}

/** Position the transparent canvas surface over the rendered .graph-cell
 *  column, sized to the #rows viewport. Returns true when the table exists. */
function positionOverlay() {
  const th = graphHeaderCell();
  if (!th) return false;
  const layoutRect = layoutElement.getBoundingClientRect();
  const rowsRect = rowsElement.getBoundingClientRect();
  const thRect = th.getBoundingClientRect();
  canvasHost.style.left = (thRect.left - layoutRect.left) + 'px';
  canvasHost.style.top = (rowsRect.top - layoutRect.top) + 'px';
  canvasHost.style.width = th.offsetWidth + 'px';
  canvasHost.style.height = rowsElement.clientHeight + 'px';
  return true;
}

/** Set the canvas backing dimensions (bounded, DPR-aware) for the current CSS
 *  surface. Returns the { width, height, scale } frame canvas descriptor. */
function canvasDimensions() {
  const positioned = positionOverlay();
  const cssWidth = positioned ? canvasHost.offsetWidth : Math.max(1, layoutElement.clientWidth);
  const cssHeight = positioned ? canvasHost.offsetHeight : Math.max(1, layoutElement.clientHeight);
  const preferredScale = Math.max(1, Math.min(2, window.devicePixelRatio || 1));
  // The graph text stays a crisp DOM overlay; the canvas may render at a lower
  // backing scale than the display when the surface is very tall, because CSS
  // stretches it back over the same row grid without changing coordinates.
  const scale = Math.max(0.125, Math.min(
    preferredScale,
    MAX_SURFACE_EDGE / cssWidth,
    MAX_SURFACE_EDGE / cssHeight,
  ));
  const width = Math.max(1, Math.round(cssWidth * scale));
  const height = Math.max(1, Math.round(cssHeight * scale));
  canvas.width = width;
  canvas.height = height;
  return { width, height, scale };
}

/** Collect the currently rendered rows: DTO geometry from the production cache
 *  (window.__editchainRowAt) + actual .graph-cell DOM rects. Only rows visible
 *  in the canvas viewport (plus a one-row overscan for edge continuity) are
 *  included. Returns the frame object, or null when nothing can render. */
function collectFrame() {
  const adapter = window.__editchainGraphAdapter;
  if (!adapter || !graphHeaderCell()) return null;

  const { width, height, scale } = canvasDimensions();
  const hostRect = canvasHost.getBoundingClientRect();
  const visibleTop = -OVERSCAN_ROWS * ROW_H;
  const visibleBottom = hostRect.height + OVERSCAN_ROWS * ROW_H;

  const frameRows = [];
  const snapshotRows = [];
  const rowElements = rowsElement.querySelectorAll('.row[data-row]');
  for (const element of rowElements) {
    // Placeholder rows carry no graph cell and no wire DTO — skip.
    const cell = element.querySelector('.graph-cell');
    if (!cell) continue;
    const absIdx = parseInt(element.getAttribute('data-row'), 10);
    if (!Number.isFinite(absIdx)) continue;
    const dto = typeof window.__editchainRowAt === 'function'
      ? window.__editchainRowAt(absIdx)
      : null;
    if (!dto) continue;
    const rect = cell.getBoundingClientRect();
    const top = rect.top - hostRect.top;
    const bottom = rect.bottom - hostRect.top;
    if (bottom < visibleTop || top > visibleBottom) continue;
    const middle = (top + bottom) / 2;
    const nodeKey = dto.node_key !== undefined && dto.node_key !== null
      ? String(dto.node_key)
      : String(absIdx);
    frameRows.push({
      index: absIdx,
      key: nodeKey,
      node_key: nodeKey,
      top: fmt(top),
      bottom: fmt(bottom),
      middle: fmt(middle),
      lane: finiteNum(dto.lane),
      above: numberList(dto.above),
      below: numberList(dto.below),
      transitions: transitionList(dto.transitions),
      is_subop: dto.is_subop === true,
      is_bundle: typeof adapter.isBundle === 'function'
        ? adapter.isBundle(dto) === true
        : false,
    });
    snapshotRows.push({
      index: absIdx,
      key: nodeKey,
      node_key: nodeKey,
      lane: finiteNum(dto.lane),
      above: numberList(dto.above),
      below: numberList(dto.below),
      transitions: transitionList(dto.transitions),
      top: fmt(top),
      bottom: fmt(bottom),
      middle: fmt(middle),
      is_subop: dto.is_subop === true,
      is_bundle: frameRows[frameRows.length - 1].is_bundle,
    });
  }

  const graph = {
    // The canvas is aligned to the graph column, so its left edge is 0.
    left: 0,
    width: fmt(hostRect.width),
    lane_x: typeof adapter.laneXAll === 'function' ? adapter.laneXAll() : [],
    dot_radius: typeof adapter.dotRadius === 'function' ? adapter.dotRadius() : 4,
    line_width: Number.isFinite(Number(adapter.lineWidth)) ? Number(adapter.lineWidth) : 1.4,
    bundle_half_height: typeof adapter.bundleHalfSpan === 'function' ? adapter.bundleHalfSpan() : 7,
    bundle_margin: typeof adapter.bundleMargin === 'function' ? adapter.bundleMargin() : 1,
    background_color: editorBackground(),
  };

  lastSnapshotRows = snapshotRows;
  return {
    canvas: { width, height, scale },
    graph,
    rows: frameRows,
  };
}

/** Replace the hidden #gpu-rows mirror with the frame's rows (data-row +
 *  data-key), keeping the harness/e2e DOM contract without touching the
 *  production #rows DOM. */
function renderMirror(frameRows) {
  const fragment = document.createDocumentFragment();
  for (const row of frameRows) {
    const element = document.createElement('div');
    element.className = 'gpu-row';
    element.setAttribute('data-row', String(row.index));
    element.setAttribute('data-key', String(row.key !== undefined ? row.key : row.index));
    fragment.appendChild(element);
  }
  mirrorElement.replaceChildren(fragment);
}

/** rAF-coalesced frame submission: size the surface, gather the visible row
 *  DTOs + rects, serialize ONE frame, and render it. */
async function renderFrame() {
  if (!rendererReady || !renderer || rendering) {
    if (rendering) scheduleFrame();
    return;
  }
  const frame = collectFrame();
  if (!frame || frame.rows.length === 0) {
    // No rendered rows yet — keep the production SVG graphs visible.
    body.classList.remove('gpu-canvas-active');
    // Empty and terminal-error production states intentionally have no graph
    // rows. They are nevertheless settled states, so the GPU debug contract
    // must not wait forever for geometry that cannot exist.
    if (window.__editchainDataReady === true) {
      lastSnapshotRows = [];
      renderMirror([]);
      metricsState.vertexCount = 0;
      metricsState.domRows = 0;
      debug.lastError = null;
      debug.dataReady = true;
      generation += 1;
      setStatus('0 / ' + productionTotal() + ' rows');
    }
    return;
  }
  rendering = true;
  const renderStartedAt = performance.now();
  try {
    const vertexCount = renderer.render(
      JSON.stringify(frame),
      frame.canvas.width,
      frame.canvas.height,
      frame.canvas.scale,
    );
    metricsState.vertexCount = vertexCount;
    metricsState.lastRenderMs = performance.now() - renderStartedAt;
    metricsState.renderCount += 1;
    metricsState.domRows = frame.rows.length;
    generation += 1;
    debug.lastError = null;
    debug.dataReady = true;
    body.classList.add('gpu-canvas-active');
    renderMirror(frame.rows);
    if (metricsState.firstWindowMs === null) {
      metricsState.firstWindowMs = performance.now() - startedAt;
    }
    setStatus(frame.rows.length + ' / ' + productionTotal() + ' rows');
  } catch (error) {
    // Transient surface errors (e.g. a surface that must be redrawn after
    // reconfiguration) recover on the next frame; the production SVG graph
    // stays visible until a render succeeds.
    const message = error instanceof Error ? error.message : String(error);
    debug.lastError = message;
    body.classList.remove('gpu-canvas-active');
    vscode.postMessage({ type: 'gpuPreviewLog', text: message });
    console.error('[editchain-gpu] render error:', message);
    setStatus('render error — retrying');
  } finally {
    rendering = false;
  }
}

function scheduleFrame() {
  if (frameScheduled) return;
  frameScheduled = true;
  requestAnimationFrame(() => {
    frameScheduled = false;
    void renderFrame();
  });
}

function startObserving() {
  // Any row DOM change (initial load, virtual paging, reanchor rebuilds,
  // profile switches, search, disclosure toggles, column resize) reframes.
  const observer = new MutationObserver(scheduleFrame);
  observer.observe(rowsElement, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ['style'],
  });
  rowsElement.addEventListener('scroll', scheduleFrame, { passive: true });
  window.addEventListener('resize', scheduleFrame);
}

function waitUntilIdle(timeoutMs) {
  const waitStartedAt = performance.now();
  let stableFrames = 0;
  let observedGeneration = generation;
  return new Promise((resolve, reject) => {
    const poll = () => {
      if (debug.lastError) {
        reject(new Error(debug.lastError));
        return;
      }
      const productionIdle = () => {
        const inFlight = window.__editchainInFlightCount;
        return typeof inFlight !== 'function' || inFlight() === 0;
      };
      const settled = rendererReady && debug.dataReady && !rendering &&
        !frameScheduled && productionIdle();
      if (settled && observedGeneration === generation) {
        stableFrames += 1;
      } else {
        stableFrames = 0;
        observedGeneration = generation;
      }
      if (stableFrames >= 2) {
        resolve({ generation, elapsedMs: performance.now() - waitStartedAt });
        return;
      }
      if (performance.now() - waitStartedAt >= timeoutMs) {
        reject(new Error('Rust/WASM renderer did not become idle within ' + timeoutMs + 'ms'));
        return;
      }
      requestAnimationFrame(poll);
    };
    requestAnimationFrame(poll);
  });
}

// GPU-ready handshake. The host accepts this instance id (and main.js emits
// its own webviewReady); the authoritative Open lifecycle belongs to the
// production main.js running in this same panel.
const rendererInstanceId = Date.now().toString(36) + '-' +
  Math.random().toString(36).slice(2);
vscode.postMessage({ type: 'gpuPreviewReady', instanceId: rendererInstanceId });

async function maybeCreateRenderer() {
  if (!wasmModule || rendererReady || rendererStarting) return;
  rendererStarting = true;
  try {
    // Size the surface BEFORE creating the WebGL swapchain: a browser
    // swapchain configured at its tiny default cannot grow to a tall history
    // canvas later without stalling the renderer.
    canvasDimensions();
    renderer = await wasmModule.GpuRenderer.create('gpu-canvas', requestedBackend);
    actualBackend = renderer.backend();
    rendererReady = true;
    metricsState.initMs = performance.now() - startedAt;
    backendElement.dataset.backend = actualBackend;
    backendElement.textContent = 'wgpu · ' + actualBackend;
    body.dataset.gpuBackend = actualBackend;
    setStatus('ready');
    startObserving();
    scheduleFrame();
  } catch (error) {
    fail(error);
  } finally {
    rendererStarting = false;
  }
}

/** Explicit WebGPU capability probe with a bounded failure path. WebGL is the
 *  automatic fallback when the requested backend is 'auto' (Rust selects GL);
 *  an explicit WebGPU request surfaces a clear negative diagnostic instead of
 *  silently downgrading. */
async function requireWebGpuAdapter() {
  if (!navigator.gpu || typeof navigator.gpu.requestAdapter !== 'function') {
    throw new Error('WebGPU was requested, but navigator.gpu is unavailable');
  }
  let timer = 0;
  try {
    const adapter = await Promise.race([
      navigator.gpu.requestAdapter(),
      new Promise((_, reject) => {
        timer = window.setTimeout(() => reject(new Error(
          'WebGPU adapter probe timed out after ' + WEBGPU_PROBE_TIMEOUT_MS + 'ms',
        )), WEBGPU_PROBE_TIMEOUT_MS);
      }),
    ]);
    if (!adapter) {
      throw new Error('WebGPU was requested, but no compatible adapter is available');
    }
  } finally {
    window.clearTimeout(timer);
  }
}

async function initialize() {
  try {
    const moduleUri = body.dataset.gpuModule;
    const wasmUri = body.dataset.gpuWasm;
    const queryBackend = new URLSearchParams(location.search).get('backend');
    requestedBackend = queryBackend === 'webgl' || queryBackend === 'webgpu'
      ? queryBackend
      : (body.dataset.gpuBackend || 'auto');
    if (!moduleUri || !wasmUri) throw new Error('Rust/WASM renderer module paths are missing');
    if (requestedBackend === 'webgpu') {
      setStatus('probing WebGPU…');
      await requireWebGpuAdapter();
    }
    setStatus('loading Rust/WASM…');
    const wasm = await import(moduleUri);
    await wasm.default({ module_or_path: wasmUri });
    wasmModule = wasm;
    setStatus('initializing ' + requestedBackend + '…');
    await maybeCreateRenderer();
  } catch (error) {
    fail(error);
  }
}

initialize();
