import type { Options } from '@wdio/types';
import path from 'node:path';

// WebdriverIO config for the Rust/WASM history renderer e2e in REAL VS
// Code.
//
// Mirrors wdio.conf.ts exactly (same workspace/service settings) but runs ONLY
// history-renderer.e2e.ts: the DEFAULT `editchain-history.open` command opens ONE
// panel titled "EditChain History", which loads the exact production scaffold
// media/main.css and ONLY the tiny
// media/rust-history/loader.js bootstrap. Rust/web-sys owns the DOM,
// accessibility surface, and the per-row SVG graph fragments (no canvas
// surface is created). The test drives the production controls (find-in-chain,
// scroll, selection) inside that Rust-backed webview, asserts
// the debug renderer contract (backend 'svg', snapshot, renderCount > 0,
// zero canvases, one aria-hidden svg.graph-row-fragment per
// hydrated row), and captures a single-panel screenshot. There is
// deliberately no second panel and no side-by-side capture — the single
// Rust/WASM panel is the only shipped UI.
//
// Requires the Rust production assets (media/rust-history/loader.js and
// media/rust-history/pkg/editchain_history_renderer.*) and the default history command
// editchain-history.open to exist.
//
// Run:  npx wdio run ./test/vscode/wdio.renderer.conf.ts

const repositoryPath = process.env.EDITCHAIN_RENDERER_E2E_WORKSPACE ??
  path.resolve(__dirname, '../../../..');
const servicePath = process.env.EDITCHAIN_RENDERER_E2E_SERVICE ??
  path.join(repositoryPath, 'target', 'release', 'editchain-vscode-service');

export const config: Options.Testrunner = {
  outputDir: 'trace',
  specs: ['./history-renderer.e2e.ts'],
  capabilities: [
    {
      browserName: 'vscode',
      browserVersion: 'stable',
      'wdio:enforceWebDriverClassic': true,
      'wdio:vscodeOptions': {
        extensionPath: path.resolve(__dirname, '../..'),
        workspacePath: repositoryPath,
        userSettings: {
          // This fixture asserts historical Activity work-group presentation.
          'editchain-history.live.enabled': false,
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
