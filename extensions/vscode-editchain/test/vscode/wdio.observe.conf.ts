import type { Options } from '@wdio/types';
import fs from 'node:fs';
import path from 'node:path';

// Read a real, actively appended rollout. A hard link limits discovery to that
// one session without copying, rewriting, or appending any provider events.
const repository = path.resolve(__dirname, '../../../..');
const workspace = process.env.EDITCHAIN_OBSERVE_WORKSPACE || repository;
const source = process.env.EDITCHAIN_OBSERVE_SOURCE;
if (!source) throw new Error('EDITCHAIN_OBSERVE_SOURCE must name the active Codex rollout');
const ownsFixture = !process.env.EDITCHAIN_OBSERVE_ROOT;
const root = process.env.EDITCHAIN_OBSERVE_ROOT || fs.mkdtempSync(path.join(repository, 'extensions/vscode-editchain/.ui-out/observe-'));
process.env.EDITCHAIN_OBSERVE_ROOT = root;
const sessions = path.join(root, 'sessions');
if (ownsFixture) {
  fs.mkdirSync(sessions);
  fs.linkSync(source, path.join(sessions, path.basename(source)));
}

export const config: Options.Testrunner = {
  outputDir: 'trace', specs: ['./history-observe.e2e.ts'],
  capabilities: [{ browserName: 'vscode', browserVersion: 'stable',
    'wdio:enforceWebDriverClassic': true,
    'wdio:vscodeOptions': {
      extensionPath: path.resolve(__dirname, '../..'), workspacePath: workspace,
      userSettings: {
        'security.workspace.trust.enabled': false,
        'editchain-history.chainDir': path.join(root, 'chain'),
        'editchain-history.servicePath': path.join(repository, 'target/release/editchain-vscode-service'),
        'editchain-history.live.sessionsPath': sessions,
        'editchain-history.live.codexHelperPath': path.join(repository, 'tools/codex-session-exporter/target/release/codex-session-exporter'),
      },
    },
  }],
  services: ['vscode'], framework: 'mocha', mochaOpts: { ui: 'bdd', timeout: 600000 }, logLevel: 'warn',
  onComplete() {
    if (ownsFixture) fs.unlinkSync(path.join(sessions, path.basename(source)));
  },
};
