import type { Options } from '@wdio/types';

// WebdriverIO config for the deterministic visual state matrix in REAL VS
// Code (screenshots + animated-scroll recording).
//
// Mirrors wdio.conf.ts exactly (same extension/workspace/service settings and
// the same DEFAULT `editchain-history.open` command with exactly ONE
// "EditChain History" panel) but runs ONLY visual-matrix.e2e.ts. The suite
// drives the default production panel through a deterministic state matrix —
// initial Activity, Raw profile, find-in-chain current/next, inline selection
// + keyboard roving, expandable-bundle disclosure (when available),
// deep virtualized scroll (animated down and back up), and graph-column
// narrow/wide with the lane-geometry invariant — capturing clearly named
// full-workbench and webview screenshots plus a JSON/Markdown manifest under
// trace/visual-matrix/.
//
// Record it with:
//   ./scripts/ui-vscode-record.sh \
//     .ui-out/vscode-visual-matrix.mp4 \
//     ./test/vscode/wdio.visual.conf.ts
//
// Run:  npx wdio run ./test/vscode/wdio.visual.conf.ts

export const config: Options.Testrunner = {
  outputDir: 'trace',
  // Specs are resolved relative to this config file's directory (test/vscode/).
  specs: ['./visual-matrix.e2e.ts'],
  capabilities: [
    {
      browserName: 'vscode',
      browserVersion: 'stable',
      // Required for WebdriverIO v9.
      'wdio:enforceWebDriverClassic': true,
      'wdio:vscodeOptions': {
        // The extension folder (contains package.json + out/).
        extensionPath: __dirname + '/../..',
        // Open the editchain repo so the extension finds .editchain/ and git.
        workspacePath: '/mnt/hot/ambientlight/repos/editchain',
        // The default panel renders the Rust/WASM view with per-row SVG graph
        // fragments (no canvas surface). SwiftShader WebGL stays enabled so
        // any other webview/GPU surfaces still work under headless Xvfb.
        vscodeArgs: {
          useAngle: 'swiftshader',
          enableUnsafeSwiftshader: true,
          ignoreGpuBlocklist: true,
          enableWebgl: true,
          disableGpuSandbox: true,
        },
        userSettings: {
          // Point the extension at the built Rust service binary.
          'editchain-history.servicePath':
            '/mnt/hot/ambientlight/repos/editchain/target/release/editchain-vscode-service',
          'editchain-history.chainDir': '.editchain',
        },
      },
    },
  ],
  services: ['vscode'],
  framework: 'mocha',
  mochaOpts: {
    ui: 'bdd',
    // Bounds the whole matrix (first window + lazy find index + transitions).
    timeout: 720000,
  },
  // Keep logs concise; the harness artifacts go to trace/.
  logLevel: 'info',
};
