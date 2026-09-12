import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { config as live } from './wdio.live.conf';

if (!process.env.EDITCHAIN_OFFSCREEN_PREPARED) {
  process.env.EDITCHAIN_OFFSCREEN_PREPARED = '1';
  const workspace = process.env.EDITCHAIN_LIVE_TEST_ROOT!;
  const source = path.join(workspace, 'sessions', 'rollout-live.jsonl');
  const repository = path.resolve(__dirname, '../../../..');
  const session = JSON.parse(fs.readFileSync(source, 'utf8').split('\n')[0]);
  const records: object[] = [session];
  const start = Date.now() - 10000;
  let tick = 0;
  const record = (type: string, payload: object) => records.push({ timestamp: new Date(start + tick++).toISOString(), type, payload });
  for (let task = 0; task < 50; task++) {
    const turn = `offscreen-${task}`;
    record('event_msg', { type: 'task_started', turn_id: turn, model_context_window: null });
    for (let item = 0; item < 6; item++) record('response_item', {
      type: 'message', id: `seed-${task}-${item}`, role: 'assistant',
      content: [{ type: 'output_text', text: `Task ${task} activity ${item}` }], phase: 'commentary',
      internal_chat_message_metadata_passthrough: { turn_id: turn },
    });
    if (task < 49) record('event_msg', { type: 'task_complete', turn_id: turn, last_agent_message: null });
  }
  fs.writeFileSync(source, records.map(record => JSON.stringify(record) + '\n').join(''));
  execFileSync(path.join(repository, 'target/release/editchain'), ['import', '--provider', 'codex',
    '--sessions-dir', path.join(workspace, 'sessions'), '--workspace', workspace,
    '--chain', path.join(workspace, '.editchain'),
    '--codex-helper', path.join(repository, 'tools/codex-session-exporter/target/release/codex-session-exporter')],
  { cwd: workspace, stdio: 'pipe' });
}

export const config = { ...live, specs: ['./history-offscreen.e2e.ts'] };
