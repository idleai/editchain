import type { Options } from '@wdio/types';
import path from 'node:path';

// WebdriverIO config for the Rust/WASM GPU history renderer e2e in REAL VS
// Code.
//
// Mirrors wdio.conf.ts exactly (same workspace/service settings) but runs ONLY
// gpu-preview.e2e.ts: the DEFAULT `editchain-history.open` command opens ONE
// panel titled "EditChain History", which loads the exact production scaffold
// (media/main.css + media/gpu-preview/gpu-preview.css) and ONLY the tiny
// media/rust-history/loader.js bootstrap. Rust/web-sys owns the DOM and
// accessibility surface, while wgpu owns the transparent graph canvas. The
// test drives the production controls
// (profile, find-in-chain, scroll, selection) inside that GPU-backed webview,
// asserts the debug renderer contract (backend, snapshot, renderCount/
// vertexCount, canvas over .graph-cell), and captures a single-panel
// screenshot. There is deliberately no second panel and no side-by-side
// capture — CPU-vs-GPU parity lives in the offscreen regression oracle
// (test/harness/functionalParity.test.js + scripts/ui-gpu-preview.mjs).
//
// Requires the Rust production assets (media/rust-history/loader.js and
// media/rust-history/pkg/editchain_gpu_preview.*) and the default history command
// editchain-history.open to exist.
//
// Run:  npx wdio run ./test/vscode/wdio.gpu.conf.ts

const repositoryPath = process.env.EDITCHAIN_GPU_E2E_WORKSPACE ??
  path.resolve(__dirname, '../../../..');
const servicePath = process.env.EDITCHAIN_GPU_E2E_SERVICE ??
  path.join(repositoryPath, 'target', 'release', 'editchain-vscode-service');

export const config: Options.Testrunner = {
  outputDir: 'trace',
  specs: ['./gpu-preview.e2e.ts'],
  capabilities: [
    {
      browserName: 'vscode',
      browserVersion: 'stable',
      'wdio:enforceWebDriverClassic': true,
      'wdio:vscodeOptions': {
        extensionPath: path.resolve(__dirname, '../..'),
        workspacePath: repositoryPath,
        // CI/Xvfb has no hardware GPU. Keep WebGL available through Chromium's
        // supported SwiftShader fallback so the real-webview run exercises the
        // same deterministic backend as the standalone parity harness.
        vscodeArgs: {
          useAngle: 'swiftshader',
          enableUnsafeSwiftshader: true,
          ignoreGpuBlocklist: true,
          enableWebgl: true,
          disableGpuSandbox: true,
        },
        userSettings: {
          'editchain-history.servicePath': servicePath,
          'editchain-history.chainDir': '.editchain',
        },
      },
    },
  ],
  services: ['vscode'],
  framework: 'mocha',
  mochaOpts: {
    ui: 'bdd',
    timeout: 240000,
  },
  logLevel: 'info',
};
