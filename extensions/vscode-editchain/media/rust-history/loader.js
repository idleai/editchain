// Rust-only history adapter loader (Slice 3A runtime completion).
//
// This ES module is the ONLY bootstrap the Rust-owned history webview loads:
// it initializes the generated wasm-bindgen module and calls the Rust shell's
// `startHistoryView` entry point. The deleted legacy JS bootstraps
// (`media/main.js`, `media/gpu-preview/bootstrap.js`) are not part of this
// path: the Rust shell owns the full runtime once the wasm module boots.
//
// CSP contract: static ES module imports only (no eval, no inline code, no
// dynamic import strings). The wasm URL is passed to `init()` explicitly so
// the loader needs no runtime URL derivation.
//
// This file owns no app state, events, DOM rendering, frame assembly, or
// host-request logic: the Rust shell owns all of those. It only mirrors the
// Rust shell's wasm-bindgen debug exports as a read-only
// `window.__editchainRendererDebug` facade for harness/e2e runners, tagged
// `loader: 'rust-history'` so suites can prove which loader produced it.
import init, {
  debugBackend,
  debugDataReady,
  debugFindState,
  debugGeneration,
  debugGraphState,
  debugInFlightCount,
  debugLaneXAll,
  debugMetrics,
  debugRenderCount,
  debugRendererInstanceId,
  debugRowAt,
  debugSnapshot,
  debugTotal,
  debugViewGen,
  startHistoryView,
} from './pkg/editchain_history_renderer.js';

const WASM_URL = new URL('./pkg/editchain_history_renderer_bg.wasm', import.meta.url);

// Clear wasm-started/error marker (body[data-rust-wasm] +
// window.__editchainRustLoader + the harness status bar).
function setMarker(state, error) {
  const message = error === null ? '' : String(error);
  document.body.dataset.rustWasm = state;
  document.body.dataset.rustWasmError = message;
  window.__editchainRustLoader = { state, error: message || null };
  const status = document.getElementById('harness-status');
  if (status) {
    status.textContent = state === 'started'
      ? 'EditChain Rust harness — wasm started'
      : 'EditChain Rust harness — wasm error: ' + message;
  }
}

function parseJson(value) {
  try {
    return JSON.parse(value);
  } catch {
    return null;
  }
}

// Read-only polling wrapper over the Rust debug exports: resolves once the
// shell reports dataReady, zero in-flight requests, and two stable frame
// generations (the shell's idle contract).
function whenIdle(timeoutMs) {
  const startedAt = performance.now();
  const limit = Number(timeoutMs) || 60000;
  let stable = 0;
  let lastGeneration = -1;
  return new Promise((resolve, reject) => {
    const poll = () => {
      const lastError = window.__editchainLastError;
      if (lastError !== null && lastError !== undefined) {
        reject(new Error(String(lastError)));
        return;
      }
      const generation = Number(debugGeneration());
      const settled = debugDataReady() && Number(debugInFlightCount()) === 0;
      if (settled && generation === lastGeneration) {
        stable += 1;
      } else {
        stable = 0;
        lastGeneration = generation;
      }
      if (stable >= 2) {
        resolve({ generation, elapsedMs: performance.now() - startedAt });
        return;
      }
      if (performance.now() - startedAt >= limit) {
        reject(new Error('Rust history renderer did not become idle within ' + limit + 'ms'));
        return;
      }
      requestAnimationFrame(poll);
    };
    requestAnimationFrame(poll);
  });
}

// Read-only debug facade over the Rust shell's generated exports.
window.__editchainRendererDebug = {
  loader: 'rust-history',
  get dataReady() {
    return debugDataReady();
  },
  get lastError() {
    return window.__editchainLastError || null;
  },
  backend: () => debugBackend(),
  total: () => debugTotal(),
  laneXAll: () => parseJson(debugLaneXAll()) || [],
  graphState: () => parseJson(debugGraphState()),
  rowAt: (index) => parseJson(debugRowAt(Number(index))),
  snapshot: () => parseJson(debugSnapshot()),
  findState: () => parseJson(debugFindState()),
  metrics: () => parseJson(debugMetrics()),
  renderCount: () => Number(debugRenderCount()),
  viewGen: () => Number(debugViewGen()),
  instanceId: () => debugRendererInstanceId(),
  whenIdle,
};

async function boot() {
  try {
    await init({ module_or_path: WASM_URL });
    await startHistoryView();
    setMarker('started', null);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    setMarker('error', message);
    window.__editchainLastError = message;
    throw error;
  }
}

boot();
