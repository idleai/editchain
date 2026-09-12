// Node contract tests for the Rust/WASM history renderer without a built wasm
// artifact. The SHIPPED VS Code UI is ONE panel titled "EditChain History",
// opened by the default `editchain-history.open` command: it loads
// media/rust-history/loader.js as its ONLY script (the Rust shell owns the
// whole runtime: it renders per-row SVG graph fragments — an aria-hidden
// svg.graph-row-fragment inside every hydrated row's .graph-cell — and
// exposes the window.__editchainRendererDebug facade: loader, dataReady,
// lastError, backend ('svg'), snapshot, metrics, laneXAll, whenIdle).
//
// The deprecated pre-Rust raw-JS/oracle stack (media/main.js,
// media/gpu-preview/bootstrap.js, the duplicate media/gpu-preview/pkg
// wasm/glue tree, and the legacy CPU/GPU oracle harness pages/scripts/probes)
// is retired: this file also guards that it stays gone and that the build
// pipeline emits exactly one pkg tree (media/rust-history/pkg).
//
// This file deliberately never touches the network or Chromium, so the generic
// `node --test test/harness/*.test.js` suite stays green without renderer build
// artifacts on disk. The static host tests below enforce the single-panel
// contract against src/extension.ts + package.json: exactly one public history
// command, no side-by-side host path, and a Rust-only production webview
// (rust-history loader, never main.js/bootstrap).
//
// Run: node --test test/harness/rendererContract.test.js

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const HARNESS_DIR = __dirname;
const EXT_ROOT = path.join(HARNESS_DIR, '..', '..');
const EXTENSION_SOURCE = fs.readFileSync(
  path.join(EXT_ROOT, 'src', 'extension.ts'), 'utf8');
const PACKAGE = JSON.parse(fs.readFileSync(path.join(EXT_ROOT, 'package.json'), 'utf8'));
const BUILD_SCRIPT = fs.readFileSync(
  path.join(EXT_ROOT, 'scripts', 'build-history-renderer.sh'), 'utf8');

test('production host forwards an exact read-only allowlist and rejects mutating/unknown calls', () => {
  const guardStart = EXTENSION_SOURCE.indexOf('Object.keys(body).length === 1');
  assert.notEqual(guardStart, -1,
    'host guards the generic request bridge with a single-key envelope check');
  const guard = EXTENSION_SOURCE.slice(guardStart, guardStart + 900);
  for (const allowed of ['GetWindow', 'FindInHistory']) {
    assert.match(guard, new RegExp("hasOwnProperty\\(body,\\s*'" + allowed + "'\\)"),
      'read-only allowlist forwards ' + allowed);
  }
  for (const blocked of [
    'Open', 'GetNodeDetails', 'ResolveObject', 'GetFileDiff', 'GetLayout', 'Search',
  ]) {
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
  assert.match(EXTENSION_SOURCE, /msg\.type === 'openDiff'/,
    'production openDiff control remains explicitly handled');
  assert.match(EXTENSION_SOURCE, /\{ GetFileDiff: \{ snapshot_id: msg\.snapshot_id, change: msg\.change \} \}/,
    'openDiff materializes only the service-advertised file-change identity');
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
  // The retired production renderer and the gpu-preview bootstrap must not
  // load (and must not exist anywhere on disk).
  assert.doesNotMatch(EXTENSION_SOURCE, /'media', 'main\.js'/,
    'production getHtml must not reference media/main.js');
  assert.doesNotMatch(EXTENSION_SOURCE, /gpu-preview', 'bootstrap\.js'/,
    'production getHtml must not reference the gpu-preview bootstrap');
  assert.doesNotMatch(EXTENSION_SOURCE, /<script src="\$\{mainScriptUri\}">/,
    'no classic-script main.js tag may remain in production');
  assert.equal(fs.existsSync(path.join(EXT_ROOT, 'media', 'main.js')), false,
    'the deprecated media/main.js oracle renderer must stay deleted');
  assert.equal(
    fs.existsSync(path.join(EXT_ROOT, 'media', 'gpu-preview', 'bootstrap.js')),
    false,
    'the deprecated media/gpu-preview/bootstrap.js must stay deleted');
  assert.equal(
    fs.existsSync(path.join(EXT_ROOT, 'media', 'gpu-preview', 'pkg')),
    false,
    'the duplicate media/gpu-preview/pkg wasm/glue tree must stay deleted');
  // The single production asset tree must exist.
  for (const rel of [
    'media/rust-history/loader.js',
    'media/rust-history/pkg/editchain_history_renderer.js',
    'media/rust-history/pkg/editchain_history_renderer_bg.wasm',
  ]) {
    assert.ok(fs.existsSync(path.join(EXT_ROOT, rel)), 'missing production asset ' + rel);
  }
  // The wasm glue/blob URIs are loader-resolved; body data attributes must not
  // carry them.
  assert.doesNotMatch(EXTENSION_SOURCE, /data-gpu-module/,
    'no data-gpu-module glue URI attribute on the production body');
  assert.doesNotMatch(EXTENSION_SOURCE, /data-gpu-wasm/,
    'no data-gpu-wasm blob URI attribute on the production body');
  // The exact SVG scaffold and CSP survive the cutover.
  assert.match(EXTENSION_SOURCE, /data-treatment="pulse"/,
    'the production body keeps the pulse treatment');
  assert.doesNotMatch(EXTENSION_SOURCE, /data-gpu-backend/,
    'the removed GPU backend selector is absent');
  assert.doesNotMatch(EXTENSION_SOURCE, /id="gpu-toolbar"|id="gpu-backend"|id="gpu-status"/,
    'the production scaffold has no visible renderer-status toolbar');
  assert.doesNotMatch(EXTENSION_SOURCE, /id="gpu-canvas-host"|id="gpu-rows"/,
    'the removed canvas and frame-mirror scaffolds are absent');
  assert.match(EXTENSION_SOURCE, /script-src \$\{cspSource\} 'wasm-unsafe-eval'/,
    'CSP permits local wasm initialization');
  assert.match(EXTENSION_SOURCE, /connect-src \$\{cspSource\}/,
    'CSP limits wasm fetches to the extension resource origin');
  assert.match(EXTENSION_SOURCE, /media', 'main\.css'/,
    'the production webview keeps media/main.css');
  assert.doesNotMatch(EXTENSION_SOURCE, /gpu-preview', 'gpu-preview\.css'/,
    'the removed GPU overlay stylesheet is not loaded');
});

test('extension contributes one history view with explicit live start/stop controls', () => {
  // Live controls operate the same history panel; no companion preview exists.
  assert.ok(PACKAGE.activationEvents.includes('onCommand:editchain-history.open'),
    'the default history command activates the extension');
  assert.ok(!PACKAGE.activationEvents.some((event) => event.includes('openGpuPreview')),
    'no side-by-side GPU preview activation event may remain');
  const commands = PACKAGE.contributes.commands;
  assert.deepEqual(commands.map(entry => entry.command), [
    'editchain-history.open', 'editchain-history.startLive', 'editchain-history.stopLive',
  ]);
  assert.doesNotMatch(EXTENSION_SOURCE, /openGpuPreview/,
    'the extension host must not register or reference an openGpuPreview command');
});

test('no side-by-side host path: one panel titled "EditChain History" hosts the Rust per-row SVG renderer', () => {
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
  // The DEFAULT panel is the history panel, and it hosts the Rust per-row SVG
  // renderer (no canvas surface is created).
  assert.match(EXTENSION_SOURCE, /createWebviewPanel\(/,
    'the host still creates the history webview panel');
  assert.match(EXTENSION_SOURCE, /'EditChain History'/,
    'the single panel is titled "EditChain History"');
  assert.match(EXTENSION_SOURCE, /rust-history', 'loader\.js/,
    'the single panel HTML loads the Rust/WASM loader');
  assert.doesNotMatch(EXTENSION_SOURCE, /gpu-canvas-host|gpu-rows/,
    'the single panel HTML has no inert GPU scaffold');
  assert.match(EXTENSION_SOURCE, /script-src \$\{cspSource\} 'wasm-unsafe-eval'/,
    'CSP permits local wasm initialization');
  assert.match(EXTENSION_SOURCE, /connect-src \$\{cspSource\}/,
    'CSP limits wasm fetches to the extension resource origin');
});

test('package scripts stop invoking the retired oracle tools and keep the Rust suites', () => {
  const scripts = PACKAGE.scripts;
  // Retired oracle CLIs must not be reachable from package scripts.
  for (const retired of ['ui:dump', 'ui:inspect', 'ui:check', 'ui:real', 'ui:graph', 'ui:gpu']) {
    assert.equal(scripts[retired], undefined,
      'retired oracle script ' + retired + ' must not exist');
  }
  // Rust-current pipeline stays intact.
  for (const kept of [
    'compile', 'build:renderer', 'test:harness', 'test:rust-smoke',
    'ui:vscode', 'ui:vscode:renderer', 'ui:vscode:visual',
  ]) {
    assert.ok(typeof scripts[kept] === 'string' && scripts[kept].length > 0,
      'kept script ' + kept + ' must exist');
  }
  assert.match(scripts['build:renderer'], /build-history-renderer\.sh/,
    'build:renderer invokes the single-output build script');
  assert.match(scripts['test:rust-smoke'], /rustSmoke\.test\.js/,
    'test:rust-smoke targets the Rust-owned adapter smoke suite');
  assert.match(scripts['test:harness'], /test\/harness\/\*\.test\.js/,
    'test:harness globs the retained harness tests');
  assert.match(scripts['ui:vscode:renderer'], /wdio\.renderer\.conf\.ts/,
    'the real VS Code Rust e2e config is retained');
  assert.doesNotMatch(scripts['ui:vscode:renderer'], /ui-gpu-preview|functionalParity|dividerResize/,
    'the real VS Code e2e must not invoke retired oracle tools');
});

test('build-history-renderer.sh emits exactly one deterministic pkg tree (media/rust-history/pkg)', () => {
  // The build script must produce the production tree only.
  assert.match(BUILD_SCRIPT, /media\/rust-history\/pkg/,
    'the build script targets media/rust-history/pkg');
  assert.doesNotMatch(BUILD_SCRIPT, /media\/gpu-preview\/pkg/,
    'the build script must not emit the retired media/gpu-preview/pkg oracle tree');
  assert.doesNotMatch(BUILD_SCRIPT, /bootstrap\.js/,
    'the build script must not reference the retired gpu-preview bootstrap');
  assert.doesNotMatch(BUILD_SCRIPT, /main\.js/,
    'the build script must not reference the retired media/main.js renderer');
  assert.doesNotMatch(BUILD_SCRIPT, /ui-gpu-preview|functionalParity|dividerResize/,
    'the build script must not reference retired oracle suites');
});

test('legacy CPU/GPU oracle harness pages, probes, and tests are retired', () => {
  const retired = [
    'index.html', 'gpu.html', 'layoutProbe.js', 'graphProbe.js',
    'functionalParity.test.js', 'dividerResize.test.js',
    'scrollParity.test.js', 'searchKeyboard.test.js', 'domRace.test.js',
  ];
  for (const rel of retired) {
    assert.equal(fs.existsSync(path.join(HARNESS_DIR, rel)), false,
      'retired oracle harness file ' + rel + ' must stay deleted');
  }
  // The Rust harness + current host tests remain.
  for (const rel of [
    'rust.html', 'rustSmoke.test.js', 'fixtures.js', 'fixtureBridge.js',
    'workUnitBridge.test.js',
    'extension.lifecycle.test.js', 'stdioClient.lifecycle.test.js',
    'serviceBridge.js', 'functionalDriver.js', 'rendererContract.test.js',
  ]) {
    assert.ok(fs.existsSync(path.join(HARNESS_DIR, rel)),
      'kept harness file ' + rel + ' must exist');
  }
  // The retired oracle UI scripts are gone too.
  for (const rel of [
    'ui-dump.mjs', 'ui-real.mjs', 'ui-graph.mjs', 'ui-gpu-preview.mjs',
  ]) {
    assert.equal(fs.existsSync(path.join(EXT_ROOT, 'scripts', rel)), false,
      'retired oracle script ' + rel + ' must stay deleted');
  }
});
