import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { config as live } from './wdio.live.conf';

// Import before the panel opens so its baseline includes completed history.
// Worker processes inherit this marker and reuse the same fixture.
if (!process.env.EDITCHAIN_GROUP_TEST_PREPARED) {
  process.env.EDITCHAIN_GROUP_TEST_PREPARED = '1';
  const workspace = process.env.EDITCHAIN_LIVE_TEST_ROOT!;
  const source = path.join(workspace, 'sessions', 'rollout-live.jsonl');
  const repository = path.resolve(__dirname, '../../../..');
  const session = JSON.parse(fs.readFileSync(source, 'utf8').split('\n')[0]);
  const start = Date.now();
  const records: object[] = [{ ...session, timestamp: new Date(start).toISOString() }];
  let tick = 1;
  const record = (type: string, payload: object) => records.push({ timestamp: new Date(start + tick++).toISOString(), type, payload });
  const message = (turn: string, id: string, text: string, role = 'assistant') => record('response_item', {
    type: 'message', id, role, content: [{ type: role === 'user' ? 'input_text' : 'output_text', text }],
    ...(role === 'assistant' ? { phase: 'commentary' } : {}),
    internal_chat_message_metadata_passthrough: { turn_id: turn },
  });
  record('event_msg', { type: 'task_started', turn_id: 'task-history', model_context_window: null });
  message('task-history', 'history-prompt', 'Implement the history importer', 'user');
  for (let index = 0; index < 40; index++) message('task-history', `history-${index}`,
    index === 20 ? 'historicalneedle verify exact hidden item' : `Historical activity ${index}`);
  record('event_msg', { type: 'task_complete', turn_id: 'task-history', last_agent_message: null });
  record('event_msg', { type: 'task_started', turn_id: 'task-live', model_context_window: null });
  message('task-live', 'live-prompt', 'Add native task grouping', 'user');
  for (let index = 0; index < 3; index++) message('task-live', `live-${index}`, `Working on task grouping ${index}`);
  fs.writeFileSync(source, records.map(record => JSON.stringify(record) + '\n').join(''));
  execFileSync(path.join(repository, 'target/release/editchain'), ['import', '--provider', 'codex',
    '--sessions-dir', path.join(workspace, 'sessions'), '--workspace', workspace,
    '--chain', path.join(workspace, '.editchain'),
    '--codex-helper', path.join(repository, 'tools/codex-session-exporter/target/release/codex-session-exporter')],
  { cwd: workspace, stdio: 'pipe' });
}

export const config = { ...live, specs: ['./history-grouping.e2e.ts'] };
