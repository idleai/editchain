import type { Options } from '@wdio/types';

// WebdriverIO config for testing the EditChain extension in REAL VS Code.
//
// wdio-vscode-service downloads/launches VS Code (Extension Development Host),
// installs the extension, and lets tests drive the workbench + webview.
//
// Run:  npx wdio run ./test/vscode/wdio.conf.ts

export const config: Options.Testrunner = {
  outputDir: 'trace',
  // Specs are resolved relative to this config file's directory (test/vscode/).
  specs: ['./history.e2e.ts'],
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
    // A real-chain FindInHistory run can spend over two minutes building the
    // lexical index and capturing full/webview screenshots before it reaches
    // its navigation assertions. Keep individual WebdriverIO waits strict,
    // but do not let Mocha terminate the Extension Development Host between a
    // click command and the corresponding Rust render.
    timeout: 480000,
  },
  // Keep logs concise; the harness artifacts go to trace/.
  logLevel: 'info',
};
