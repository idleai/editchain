// Node contract tests for the Rust/WASM history renderer without a built wasm
// artifact. The SHIPPED VS Code UI is ONE panel titled "EditChain History",
// opened by the default `editchain-history.open` command: it loads
// media/rust-history/loader.js as its ONLY script (the Rust shell owns the
// whole runtime, including the wgpu canvas under #gpu-canvas-host and the
// window.__editchainGpuDebug facade: loader, dataReady, lastError, backend,
// snapshot, metrics, laneXAll, whenIdle). Production media/main.js and
// media/gpu-preview/bootstrap.js are NOT loaded in production; they remain as
// the offscreen regression-oracle side of the parity tests below.
//
// test/harness/gpu.html is that OFFSCREEN regression-oracle page: it mirrors
// test/harness/index.html (the same fixtures.js + fixtureBridge.js,
// media/main.css, media/main.js, and the same control/search/row semantics),
// and the GPU bootstrap overlays a transparent wgpu canvas only over
// .graph-cell. The oracle is NOT the shipped VS Code UI; it exists so the
// CPU/SVG renderer (main.js) can be compared deterministically against the
// GPU view.
//
// The deterministic browser parity oracle (CPU-vs-GPU over HTTP) is `node
// --test test/harness/functionalParity.test.js` (Chrome + built GPU assets
// required; it skips otherwise), and `npm run ui:gpu` runs
// scripts/ui-gpu-preview.mjs only when media/gpu-preview/build artifacts exist.
// This file deliberately never touches the network or Chromium, so the generic
// `node --test test/harness/*.test.js` suite stays green without GPU build
// artifacts on disk. The static host tests below additionally enforce the
// single-panel contract against src/extension.ts + package.json: exactly one
// public history command, no side-by-side host path, and a Rust-only
// production webview (rust-history loader, never main.js/bootstrap).
//
// Run: node --test test/harness/gpuContract.test.js

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const HARNESS_DIR = __dirname;
const EXT_ROOT = path.join(HARNESS_DIR, '..', '..');
const GPU_HTML = fs.readFileSync(path.join(HARNESS_DIR, 'gpu.html'), 'utf8');
const CPU_HTML = fs.readFileSync(path.join(HARNESS_DIR, 'index.html'), 'utf8');
const GPU_BOOTSTRAP = fs.readFileSync(
  path.join(EXT_ROOT, 'media', 'gpu-preview', 'bootstrap.js'), 'utf8');
const EXTENSION_SOURCE = fs.readFileSync(
  path.join(EXT_ROOT, 'src', 'extension.ts'), 'utf8');
const PACKAGE = JSON.parse(fs.readFileSync(path.join(EXT_ROOT, 'package.json'), 'utf8'));

// Extract attributes from the harness <body> tag.
function bodyAttrs(html) {
  const m = /<body\b([^>]*)>/i.exec(html);
  assert.ok(m, 'harness page must have a <body> tag');
  const attrs = {};
  for (const [, key, value] of m[1].matchAll(/([a-z-]+)(?:="([^"]*)")?/gi)) {
    attrs[key] = value !== undefined ? value : '';
  }
  return attrs;
}

// The production control/search scaffold both harness pages must share.
const PRODUCTION_CONTROL_IDS = [
  'controls', 'profile-control', 'profile-activity', 'profile-raw',
  'search-control', 'search', 'search-counter', 'search-prev', 'search-next',
  'layout', 'rows', 'status-live',
];

function exactScriptSrc(html, basename) {
  const m = new RegExp('<script src="([^"]*' + basename.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + ')"></script>').exec(html);
  return m ? m[1] : null;
}

test('gpu.html shares the exact production scaffold with index.html', () => {
  // Same deterministic protocol fixtures + fixture bridge as the CPU page.
  assert.match(GPU_HTML, /<script src="\.\/fixtures\.js"><\/script>/,
    'loads fixtures.js with the exact relative src used by index.html');
  assert.match(GPU_HTML, /<script src="\.\/fixtureBridge\.js"><\/script>/,
    'loads fixtureBridge.js with the exact relative src used by index.html');
  // The oracle loads the SHARED production stylesheet + renderer, with the
  // exact same HTTP-relative srcs the CPU harness page uses.
  assert.match(GPU_HTML, /<link rel="stylesheet" href="\.\.\/\.\.\/media\/main\.css">/,
    'links the shared production media/main.css');
  assert.equal(exactScriptSrc(GPU_HTML, 'main.js'), exactScriptSrc(CPU_HTML, 'main.js'),
    'loads the shared production media/main.js with the exact src index.html uses');
  assert.ok(exactScriptSrc(GPU_HTML, 'main.js'), 'gpu.html must load media/main.js');
  // CPU-only probes stay off the GPU page: it has its own bootstrap debug API.
  assert.doesNotMatch(GPU_HTML, /layoutProbe\.js|graphProbe\.js|serviceBridge\.js/,
    'must not load CPU-only probes');
});

test('gpu.html carries the full production controls scaffold, matching index.html', () => {
  for (const id of PRODUCTION_CONTROL_IDS) {
    assert.ok(GPU_HTML.includes('id="' + id + '"'), 'gpu.html missing production id #' + id);
    assert.ok(CPU_HTML.includes('id="' + id + '"'), 'index.html missing production id #' + id);
  }
  // Both pages announce state changes to screen readers the same way.
  assert.ok(GPU_HTML.includes('id="status-live"') &&
    GPU_HTML.includes('role="status"') && GPU_HTML.includes('aria-live="polite"'),
    'live region announces state changes to screen readers');
  assert.ok(GPU_HTML.includes('id="search"') && GPU_HTML.includes('id="search-counter"'),
    'search control is part of the shared scaffold');
});

test('gpu.html loads the production GPU bootstrap and backend override', () => {
  assert.match(GPU_HTML, /<script type="module" src="\.\.\/\.\.\/media\/gpu-preview\/bootstrap\.js"><\/script>/,
    'loads media/gpu-preview/bootstrap.js as an ES module');
  const attrs = bodyAttrs(GPU_HTML);
  assert.equal(attrs['data-treatment'], 'pulse', 'keeps the treatment attribute of the CPU scaffold');
  assert.equal(attrs['data-gpu-backend'], 'webgl', 'default backend is webgl');
  assert.match(GPU_HTML, /new URLSearchParams\(location\.search\)\.get\('backend'\)/,
    'inline script reads the backend query override');
  assert.match(GPU_HTML, /data-gpu-backend/, 'the effective backend is stored on the body');
});

test('gpu.html ships VS Code theme tokens like index.html', () => {
  assert.match(GPU_HTML, /--vscode-editor-background/);
  assert.match(GPU_HTML, /--vscode-list-activeSelectionBackground/);
  assert.match(GPU_HTML, /--vscode-focusBorder/);
});

test('production bootstrap overlays a wgpu canvas only over .graph-cell and exposes the debug contract', () => {
  assert.match(GPU_BOOTSTRAP, /\.graph-cell/,
    'overlays the transparent wgpu canvas on the production graph cells');
  assert.match(GPU_BOOTSTRAP, /createElement\('canvas'\)|createElement\("canvas"\)/,
    'creates the canvas surface at runtime');
  assert.match(GPU_BOOTSTRAP, /GpuRenderer\.create\(/,
    'creates the wasm-bindgen wgpu renderer');
  assert.match(GPU_BOOTSTRAP, /renderer\.render\(/,
    'submits normalized row geometry through Rust');
  // Snapshot derives from the HARNESS rendered DOM (.row[data-row]) and the
  // harness main.js row DTOs (window.__editchainRowAt / node_key) — the
  // oracle's main.js debug hooks remain and are what the snapshot reads.
  assert.match(GPU_BOOTSTRAP, /\.row\b|data-row|__editchainRowAt/,
    'snapshot reads the production rendered .row[data-row] DOM and row DTOs');
  for (const member of ['whenIdle', 'snapshot', 'metrics', 'backend', 'dataReady', 'lastError']) {
    assert.ok(GPU_BOOTSTRAP.includes(member), 'missing debug member: ' + member);
  }
  for (const metric of ['renderCount', 'vertexCount']) {
    assert.ok(GPU_BOOTSTRAP.includes(metric), 'missing renderer metric: ' + metric);
  }
  assert.match(GPU_BOOTSTRAP, /navigator\.gpu\.requestAdapter\(\)/,
    'explicit WebGPU diagnostics probe adapter capability before creating wgpu');
  assert.match(GPU_BOOTSTRAP, /WEBGPU_PROBE_TIMEOUT_MS/,
    'the explicit WebGPU capability probe has a bounded failure path');
  // The bootstrap never invents its own mutating/unknown service DTOs: it runs
  // the harness main.js request path (GetWindow / FindInHistory / Search).
  assert.doesNotMatch(GPU_BOOTSTRAP, /(?:ResolveObject|GetNodeDetails|Open):\s*\{/,
    'the bootstrap must not issue mutating or unknown service requests itself');
});

test('production host forwards an exact read-only allowlist and rejects mutating/unknown calls', () => {
  const guardStart = EXTENSION_SOURCE.indexOf('Object.keys(body).length === 1');
  assert.notEqual(guardStart, -1,
    'host guards the generic request bridge with a single-key envelope check');
  const guard = EXTENSION_SOURCE.slice(guardStart, guardStart + 900);
  for (const allowed of ['GetWindow', 'FindInHistory', 'Search']) {
    assert.match(guard, new RegExp("hasOwnProperty\\(body,\\s*'" + allowed + "'\\)"),
      'read-only allowlist forwards ' + allowed);
  }
  for (const blocked of ['Open', 'GetNodeDetails', 'ResolveObject', 'GetLayout']) {
    assert.doesNotMatch(guard, new RegExp("hasOwnProperty\\(body,\\s*'" + blocked + "'\\)"),
      'allowlist excludes ' + blocked);
  }
  // The rejected branch must be visible (an Error envelope back to the
  // webview), and the forwarding call must sit AFTER the allowlist guard.
  assert.match(EXTENSION_SOURCE, /rejected request \([^)]*only/,
    'the rejection branch names the allowlist');
  assert.match(EXTENSION_SOURCE, /body:\s*\{ Error:\s*'EditChain History:/,
    'rejected envelopes get a visible Error response');
  const forward = EXTENSION_SOURCE.indexOf('client.request(body,');
  assert.ok(forward > guardStart,
    'service forwarding happens only after the allowlist guard');
  // Production controls remain host-side: the read-only raw-JSON viewer is
  // still explicitly handled on the single history panel.
  assert.match(EXTENSION_SOURCE, /msg\.type === 'openJson'/,
    'production openJson control remains handled');
});

test('production webview is Rust-only: rust-history loader, never main.js or the gpu-preview bootstrap', () => {
  // The single production panel loads media/rust-history/loader.js as its ONLY
  // script, exactly like the rust.html harness page.
  assert.match(EXTENSION_SOURCE, /'media', 'rust-history', 'loader\.js'/,
    'getHtml resolves media/rust-history/loader.js');
  assert.match(
    EXTENSION_SOURCE,
    /<script type="module" src="\$\{rustLoaderUri\}"><\/script>/,
    'the production webview loads rust-history/loader.js as an ES module'
  );
  // The production renderer and the gpu-preview bootstrap must not load.
  assert.doesNotMatch(EXTENSION_SOURCE, /'media', 'main\.js'/,
    'production getHtml must not reference media/main.js');
  assert.doesNotMatch(EXTENSION_SOURCE, /gpu-preview', 'bootstrap\.js'/,
    'production getHtml must not reference the gpu-preview bootstrap');
  assert.doesNotMatch(EXTENSION_SOURCE, /<script src="\$\{mainScriptUri\}">/,
    'no classic-script main.js tag may remain in production');
  // The wasm glue/blob URIs are loader-resolved; body data attributes must not
  // carry them.
  assert.doesNotMatch(EXTENSION_SOURCE, /data-gpu-module/,
    'no data-gpu-module glue URI attribute on the production body');
  assert.doesNotMatch(EXTENSION_SOURCE, /data-gpu-wasm/,
    'no data-gpu-wasm blob URI attribute on the production body');
  // The exact scaffold/CSP/body backend survives the cutover.
  assert.match(EXTENSION_SOURCE, /data-treatment="pulse"/,
    'the production body keeps the pulse treatment');
  assert.match(EXTENSION_SOURCE, /data-gpu-backend="auto"/,
    'the production body keeps the auto backend request');
  assert.match(EXTENSION_SOURCE, /id="gpu-toolbar"/,
    'the production scaffold keeps the GPU toolbar');
  assert.match(EXTENSION_SOURCE, /id="gpu-canvas-host"/,
    'the production scaffold keeps the canvas host');
  assert.match(EXTENSION_SOURCE, /id="gpu-rows"/,
    'the production scaffold keeps the row mirror');
  assert.match(EXTENSION_SOURCE, /script-src \$\{cspSource\} 'wasm-unsafe-eval'/,
    'CSP permits local wasm initialization');
  assert.match(EXTENSION_SOURCE, /connect-src \$\{cspSource\}/,
    'CSP limits wasm fetches to the extension resource origin');
  assert.match(EXTENSION_SOURCE, /media', 'main\.css'/,
    'the production webview keeps media/main.css');
  assert.match(EXTENSION_SOURCE, /gpu-preview', 'gpu-preview\.css'/,
    'the production webview keeps the GPU overlay stylesheet');
});

test('extension contributes exactly one public history command (editchain-history.open)', () => {
  // The shipped UI is the single default history view: exactly one public
  // command opens it, and the side-by-side preview command must not exist.
  assert.ok(PACKAGE.activationEvents.includes('onCommand:editchain-history.open'),
    'the default history command activates the extension');
  assert.ok(!PACKAGE.activationEvents.some((event) => event.includes('openGpuPreview')),
    'no side-by-side GPU preview activation event may remain');
  const commands = PACKAGE.contributes.commands;
  assert.ok(Array.isArray(commands) && commands.length === 1,
    'exactly one public command must be contributed, got ' + JSON.stringify(commands));
  assert.equal(commands[0].command, 'editchain-history.open',
    'the one public command is the default history open');
  assert.doesNotMatch(EXTENSION_SOURCE, /openGpuPreview/,
    'the extension host must not register or reference an openGpuPreview command');
});

test('no side-by-side host path: one panel titled "EditChain History" hosts the wgpu canvas', () => {
  // A single webview panel: no distinct GPU panel identity, no companion
  // column-two reveal, no second panel title.
  assert.doesNotMatch(EXTENSION_SOURCE, /openGpuPreviewView/,
    'no side-by-side GPU preview open function may remain');
  assert.doesNotMatch(EXTENSION_SOURCE, /createWebviewPanel\(\s*'editchainHistoryGpu'/,
    'no second webview panel identity for a GPU preview may remain');
  assert.doesNotMatch(EXTENSION_SOURCE, /gpuPanel\.reveal\(vscode\.ViewColumn\.Two\)/,
    'no column-two reveal of a companion GPU panel may remain');
  assert.doesNotMatch(EXTENSION_SOURCE, /EditChain History — Rust\/WASM GPU/,
    'the side-by-side GPU panel title must not exist');
  // The DEFAULT panel is the history panel, and it hosts the GPU canvas.
  assert.match(EXTENSION_SOURCE, /createWebviewPanel\(/,
    'the host still creates the history webview panel');
  assert.match(EXTENSION_SOURCE, /'EditChain History'/,
    'the single panel is titled "EditChain History"');
  assert.match(EXTENSION_SOURCE, /rust-history', 'loader\.js/,
    'the single panel HTML loads the Rust/WASM loader');
  assert.match(EXTENSION_SOURCE, /gpu-canvas-host/,
    'the single panel HTML carries the wgpu canvas host');
  assert.match(EXTENSION_SOURCE, /script-src \$\{cspSource\} 'wasm-unsafe-eval'/,
    'CSP permits local wasm initialization');
  assert.match(EXTENSION_SOURCE, /connect-src \$\{cspSource\}/,
    'CSP limits wasm fetches to the extension resource origin');
});

test('ui-gpu-preview.mjs normalizeRow canonicalizes both naming conventions', async () => {
  const mod = await import('../../scripts/ui-gpu-preview.mjs');
  const row = mod.normalizeRow({
    index: 3,
    node_key: 'git:m1',
    lane: '2',
    above: [1, 0, 0],
    below: [2],
    transitions: [[1, 0], [2, 1]],
  });
  assert.deepEqual(row, {
    index: 3,
    key: 'git:m1',
    lane: 2,
    above: [0, 1],
    below: [2],
    transitions: [[1, 0], [2, 1]],
  });
  // GPU snapshot naming (key + absolute) normalizes identically.
  assert.deepEqual(mod.normalizeRow({ absolute: 0, key: 'git:m0', lane: 0 }).key, 'git:m0');
  // Missing geometry defaults like the renderer draws (lane 0, empty lanes).
  assert.deepEqual(mod.normalizeRow({ index: 1, key: 'x' }), {
    index: 1, key: 'x', lane: 0, above: [], below: [], transitions: [],
  });
});

test('ui-gpu-preview.mjs compareParity matches common rows and total', async () => {
  const mod = await import('../../scripts/ui-gpu-preview.mjs');
  const cpu = {
    total: 5,
    rows: [
      { index: 0, key: 'git:m3', lane: 0, above: [], below: [0, 1], transitions: [] },
      { index: 1, key: 'git:m2', lane: 0, above: [0], below: [0], transitions: [] },
    ],
  };
  const gpu = {
    total: 5,
    rows: [
      { index: 0, key: 'git:m3', lane: 0, above: [], below: [1, 0], transitions: [] },
      { index: 1, key: 'git:m2', lane: 0, above: [0], below: [0], transitions: [] },
    ],
  };
  const ok = mod.compareParity(cpu, gpu);
  assert.equal(ok.pass, true);
  assert.equal(ok.commonRows, 2);
  assert.equal(ok.totalsEqual, true);
  assert.equal(ok.mismatches.length, 0);
});

test('ui-gpu-preview.mjs compareParity flags field/total/coverage failures', async () => {
  const mod = await import('../../scripts/ui-gpu-preview.mjs');
  const cpu = { total: 5, rows: [{ index: 0, key: 'git:m3', lane: 0, above: [], below: [1], transitions: [] }] };
  // Lane mismatch on a common row.
  const lane = mod.compareParity(cpu, { total: 5, rows: [{ index: 0, key: 'git:m3', lane: 1, above: [], below: [1], transitions: [] }] });
  assert.equal(lane.pass, false);
  assert.deepEqual(lane.mismatches, [{ index: 0, field: 'lane', cpuValue: 0, gpuValue: 1 }]);
  // Total mismatch.
  const total = mod.compareParity(cpu, { total: 6, rows: [{ index: 0, key: 'git:m3', lane: 0, above: [], below: [1], transitions: [] }] });
  assert.equal(total.pass, false);
  assert.equal(total.totalsEqual, false);
  // Disjoint views (no common row) never pass.
  const disjoint = mod.compareParity(cpu, { total: 5, rows: [{ index: 4, key: 'git:m0', lane: 0, above: [], below: [], transitions: [] }] });
  assert.equal(disjoint.pass, false);
  assert.equal(disjoint.commonRows, 0);
  // Coverage gaps are recorded but do not fail a matched common row set.
  const gap = mod.compareParity(cpu, {
    total: 5,
    rows: [
      { index: 0, key: 'git:m3', lane: 0, above: [], below: [1], transitions: [] },
      { index: 4, key: 'git:m0', lane: 0, above: [], below: [], transitions: [] },
    ],
  });
  assert.equal(gap.pass, true);
  assert.deepEqual(gap.gpuOnlyRows, [{ index: 4, key: 'git:m0' }]);
  assert.deepEqual(gap.cpuOnlyRows, []);

  // Transitions are directed (child/from lane -> parent/to lane): reversing a
  // pair changes the rendered diagonal and must be reported as a mismatch.
  const direction = mod.compareParity({
    total: 1,
    rows: [{ index: 0, key: 'x', lane: 0, transitions: [[0, 1]] }],
  }, {
    total: 1,
    rows: [{ index: 0, key: 'x', lane: 0, transitions: [[1, 0]] }],
  });
  assert.equal(direction.pass, false);
  assert.deepEqual(direction.mismatches, [{
    index: 0,
    field: 'transitions',
    cpuValue: [[0, 1]],
    gpuValue: [[1, 0]],
  }]);
});

test('ui-gpu-preview.mjs compareFunctionalState fails on missing common functional state', async () => {
  const mod = await import('../../scripts/ui-gpu-preview.mjs');
  const state = {
    functional: {
      profile: 'activity',
      total: 5,
      searchActive: false,
      header: true,
      warningBanner: false,
      message: null,
      rowIndexes: [0, 1, 2, 3, 4],
      lastWindowHideTrace: true,
    },
  };
  assert.equal(mod.compareFunctionalState(state, state).pass, true);
  // A profile drift (geometry could still match) must fail.
  const profile = mod.compareFunctionalState(state, {
    functional: { ...state.functional, profile: 'raw' },
  });
  assert.equal(profile.pass, false);
  assert.deepEqual(profile.mismatches, [{ field: 'profile', cpuValue: 'activity', gpuValue: 'raw' }]);
  // Different buffer extents are valid when both views share an anchor and
  // overlap (the GPU status strip changes viewport height).
  const shorter = mod.compareFunctionalState(state, {
    functional: { ...state.functional, rowIndexes: [0, 1, 2] },
  });
  assert.equal(shorter.pass, true);
  // Disjoint render windows still fail functional parity.
  const rows = mod.compareFunctionalState(state, {
    functional: { ...state.functional, rowIndexes: [3, 4] },
  });
  assert.equal(rows.pass, false);
  assert.deepEqual(rows.mismatches, [{
    field: 'rowWindow',
    cpuValue: { first: 0, last: 4, count: 5 },
    gpuValue: { first: 3, last: 4, count: 2 },
  }]);
  // Message state parity (empty/error scenarios) is explicit, not inferred.
  const msg = mod.compareFunctionalState(
    { functional: { message: { error: true, text: 'Failed to open history: x' } } },
    { functional: { message: { error: false, text: 'Failed to open history: x' } } },
  );
  assert.equal(msg.pass, false);
  assert.deepEqual(msg.mismatches, [{ field: 'message', cpuValue: { error: true, text: 'Failed to open history: x' }, gpuValue: { error: false, text: 'Failed to open history: x' } }]);
});

test('ui-gpu-preview.mjs scenarioExpectation covers multigroup + expected empty/error handling', async () => {
  const mod = await import('../../scripts/ui-gpu-preview.mjs');
  assert.equal(mod.scenarioExpectation('merge'), 'rows');
  assert.equal(mod.scenarioExpectation('multigroup'), 'rows');
  assert.equal(mod.scenarioExpectation('workUnitsDeep'), 'rows');
  assert.equal(mod.scenarioExpectation('empty'), 'empty');
  assert.equal(mod.scenarioExpectation('error'), 'error');
});

test('ui-gpu-preview.mjs parseArgs handles the full CLI surface', async () => {
  const mod = await import('../../scripts/ui-gpu-preview.mjs');
  assert.deepEqual(mod.parseArgs([]), { scenario: 'merge', backend: 'webgl', viewport: '1440x900', out: null, shot: false });
  assert.deepEqual(mod.parseArgs(['--scenario', 'mixed', '--backend', 'webgpu', '--viewport', '800x600', '--out', '/tmp/x', '--shot']),
    { scenario: 'mixed', backend: 'webgpu', viewport: '800x600', out: '/tmp/x', shot: true });
  assert.throws(() => mod.parseArgs(['--nope']), /unknown argument/);
});
