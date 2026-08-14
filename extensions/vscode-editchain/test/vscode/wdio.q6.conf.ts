import type { Options } from '@wdio/types';

// WebdriverIO config for validating the q6 chain (lane reuse + subagent
// branch/reconnect) in REAL VS Code.
//
// Same as wdio.conf.ts but points the extension at the q6 import chain
// (./outputs/cc-chain-q6) instead of the default .editchain, so the recorded
// session exercises the new lane-reuse and subagent-linking layout.
//
// Run:  npx wdio run ./test/vscode/wdio.q6.conf.ts

export const config: Options.Testrunner = {
  outputDir: 'trace',
  specs: ['./history.e2e.ts'],
  capabilities: [
    {
      browserName: 'vscode',
      browserVersion: 'stable',
      'wdio:enforceWebDriverClassic': true,
      'wdio:vscodeOptions': {
        extensionPath: __dirname + '/../..',
        workspacePath: '/mnt/hot/ambientlight/repos/editchain',
        userSettings: {
          'editchain-history.servicePath':
            '/mnt/hot/ambientlight/repos/editchain/target/debug/editchain-vscode-service',
          // Point at the q6 import chain (lane reuse + subagent edges).
          'editchain-history.chainDir': 'outputs/cc-chain-q6',
        },
      },
    },
  ],
  services: ['vscode'],
  framework: 'mocha',
  mochaOpts: {
    ui: 'bdd',
    timeout: 120000,
  },
  logLevel: 'info',
};