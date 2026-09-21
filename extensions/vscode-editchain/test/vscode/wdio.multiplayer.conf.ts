import type {} from '@wdio/types';
import path from 'node:path';

const fixture = process.env.EDITCHAIN_MULTIPLAYER_UI_FIXTURE!;
const role = process.env.EDITCHAIN_MULTIPLAYER_UI_ROLE!;
if (!fixture || !['host', 'guest'].includes(role)) throw new Error('Run scripts/multiplayer-vscode-e2e.cjs');
const extension = path.join(fixture, 'package', 'extension');
const output = path.join(process.env.EDITCHAIN_MULTIPLAYER_UI_OUTPUT!, role);
const version = process.env.EDITCHAIN_MULTIPLAYER_UI_VERSION || '1.132.0';

export const config: WebdriverIO.Config = {
  outputDir: output, specs: ['./multiplayer.e2e.ts'], maxInstances: 1,
  capabilities: [{ browserName: 'vscode', browserVersion: version,
    'wdio:enforceWebDriverClassic': true,
    'wdio:chromedriverOptions': { cacheDir: path.join(fixture, role + '-driver') },
    'wdio:vscodeOptions': {
      extensionPath: path.join(fixture, 'probe'), workspacePath: path.join(fixture, role), storagePath: path.join(fixture, role + '-profile'),
      vscodeArgs: { disableExtensions: [], extensionTestsPath: [],
        disableExtension: ['vscode.github-authentication'], passwordStore: 'basic' },
      userSettings: {
        'security.workspace.trust.enabled': false, 'telemetry.telemetryLevel': 'off',
        'window.dialogStyle': 'custom',
        'editchain-history.live.enabled': true,
        'editchain-history.live.sessionsPath': path.join(fixture, 'sessions'),
        'editchain-history.live.codexHelperPath': path.join(fixture, 'missing-codex-session-exporter'),
        'editchain-history.tracking.readDwellMs': 2000,
        'workbench.startupEditor': 'none', 'files.autoSave': 'off', 'files.hotExit': 'off',
        'editor.minimap.enabled': false, 'editor.quickSuggestions': false, 'editor.wordWrap': 'off',
        'editor.formatOnSave': false, 'editor.formatOnType': false, 'editor.fontSize': 14,
        'editor.lineHeight': 20, 'editor.stickyScroll.enabled': false,
        'editor.autoClosingBrackets': 'never', 'editor.autoClosingQuotes': 'never',
      },
    },
  }], services: ['vscode'], framework: 'mocha', mochaOpts: { ui: 'bdd', timeout: 240000 },
  logLevel: 'warn', connectionRetryTimeout: 90000,
};
