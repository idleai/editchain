import type { Options } from '@wdio/types';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const repository = path.resolve(__dirname, '../../../..');
const ownsFixture = !process.env.EDITCHAIN_LIVE_TEST_ROOT;
const workspace = process.env.EDITCHAIN_LIVE_TEST_ROOT || fs.mkdtempSync(path.join(os.tmpdir(), 'editchain-live-vscode-'));
process.env.EDITCHAIN_LIVE_TEST_ROOT = workspace;
const sessions = path.join(workspace, 'sessions');
const source = path.join(sessions, 'rollout-live.jsonl');
if (ownsFixture) {
  fs.mkdirSync(sessions);
  fs.writeFileSync(path.join(workspace, 'README.md'), 'Live history fixture\n');
  execFileSync('git', ['init', '-q', workspace]);
  execFileSync('git', ['-C', workspace, 'add', 'README.md']);
  execFileSync('git', ['-C', workspace, '-c', 'user.name=EditChain Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'Initial live fixture']);
  const timestamp = new Date().toISOString();
  const records: object[] = [
    { timestamp, type: 'session_meta', payload: { id: '11111111-1111-7111-8111-111111111111', timestamp,
      cwd: workspace, originator: 'codex_cli_rs', cli_version: '0.154.0', source: 'cli' } },
    { timestamp, type: 'event_msg', payload: { type: 'task_started', turn_id: 'turn-live', model_context_window: null } },
  ];
  (records[0] as any).payload.git = { commit_hash: execFileSync('git', ['-C', workspace, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim() };
  for (let index = 0; index < 80; index++) records.push({
    timestamp: new Date(Date.now() + index).toISOString(), type: 'response_item', payload: {
      type: 'message', id: `msg_${index}`, role: 'assistant', content: [{ type: 'output_text', text: `Initial live history message ${index}` }],
      phase: 'commentary', internal_chat_message_metadata_passthrough: { turn_id: 'turn-live' },
    },
  });
  fs.writeFileSync(source, records.map(record => JSON.stringify(record) + '\n').join(''));
}

export const config: Options.Testrunner = {
  outputDir: 'trace', specs: ['./history-live.e2e.ts'],
  capabilities: [{ browserName: 'vscode', browserVersion: 'stable',
    'wdio:enforceWebDriverClassic': true,
    'wdio:vscodeOptions': {
      extensionPath: path.resolve(__dirname, '../..'), workspacePath: workspace,
      userSettings: {
        'security.workspace.trust.enabled': false,
        'editchain-history.servicePath': path.join(repository, 'target/release/editchain-vscode-service'),
        'editchain-history.live.cliPath': path.join(repository, 'target/release/editchain'),
        'editchain-history.live.sessionsPath': sessions,
        'editchain-history.live.codexHelperPath': path.join(repository, 'tools/codex-session-exporter/target/release/codex-session-exporter'),
      },
    },
  }],
  services: ['vscode'], framework: 'mocha', mochaOpts: { ui: 'bdd', timeout: 180000 }, logLevel: 'warn',
  onComplete() { if (ownsFixture) fs.rmSync(workspace, { recursive: true, force: true }); },
};
