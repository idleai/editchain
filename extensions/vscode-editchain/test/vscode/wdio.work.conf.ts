import type {} from '@wdio/types';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const repository = path.resolve(__dirname, '../../../..');
const owner = !process.env.EDITCHAIN_WORK_FIXTURE;
const fixture = process.env.EDITCHAIN_WORK_FIXTURE || fs.mkdtempSync(path.join(os.tmpdir(), 'editchain-work-'));
process.env.EDITCHAIN_WORK_FIXTURE = fixture;
const workspace = path.join(fixture, 'workspace');
const version = process.env.EDITCHAIN_CAPTURE_VSCODE || '1.137.0';
const output = path.resolve('trace', `work-${version}`);
if (owner) {
  fs.mkdirSync(workspace);
  fs.mkdirSync(path.join(fixture, 'sessions'));
  execFileSync(path.join(repository, 'target/debug/examples/editor_work_fixture'), [workspace]);
  fs.rmSync(output, { recursive: true, force: true });
}
fs.mkdirSync(output, { recursive: true });
export const config: WebdriverIO.Config = {
  outputDir: output, specs: ['./human-work.e2e.ts'], maxInstances: 1,
  capabilities: [{ browserName: 'vscode', browserVersion: version,
    'wdio:enforceWebDriverClassic': true,
    'wdio:vscodeOptions': {
      extensionPath: path.resolve(__dirname, '../..'), workspacePath: workspace,
      storagePath: path.join(fixture, 'profile'),
      userSettings: {
        'security.workspace.trust.enabled': false, 'telemetry.telemetryLevel': 'off',
        'editchain-history.servicePath': path.join(repository, 'target/debug/editchain-vscode-service'),
        'editchain-history.live.enabled': true, 'editchain-history.tracking.enabled': true,
        'editchain-history.live.sessionsPath': path.join(fixture, 'sessions'),
        'editchain-history.tracking.readDwellMs': 2000,
        'workbench.startupEditor': 'none', 'files.autoSave': 'off', 'files.hotExit': 'off',
        'editor.minimap.enabled': false, 'editor.quickSuggestions': false, 'editor.wordWrap': 'off',
        'editor.formatOnSave': false, 'editor.formatOnType': false, 'editor.fontSize': 14,
        'editor.lineHeight': 20, 'editor.stickyScroll.enabled': false,
      },
    },
  }], services: ['vscode'], framework: 'mocha', mochaOpts: { ui: 'bdd', timeout: 120000 }, logLevel: 'warn',
  onComplete(exitCode) {
    fs.writeFileSync(path.join(output, 'run.json'), JSON.stringify({ version, exitCode }));
    // Keep the tiny synthetic chain with the trace as a reproducible artifact.
    if (owner) {
      fs.cpSync(path.join(workspace, '.editchain'), path.join(output, 'chain'), { recursive: true });
      const logs = path.join(fixture, 'profile/settings/logs');
      if (fs.existsSync(logs)) fs.cpSync(logs, path.join(output, 'vscode-logs'), { recursive: true });
      fs.rmSync(fixture, { recursive: true, force: true });
    }
  },
};
