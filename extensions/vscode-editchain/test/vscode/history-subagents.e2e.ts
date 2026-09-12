import fs from 'node:fs';
import path from 'node:path';

const workspace = process.env.EDITCHAIN_LIVE_TEST_ROOT!;
const source = path.join(workspace, 'sessions', 'rollout-live.jsonl');
const parent = '11111111-1111-7111-8111-111111111111';
const children = [
  ['22222222-2222-7222-8222-222222222222', 'Feynman'],
  ['33333333-3333-7333-8333-333333333333', 'Huygens'],
  ['44444444-4444-7444-8444-444444444444', 'Lovelace'],
];

function append(file: string, type: string, payload: object, timestamp = new Date().toISOString()) {
  fs.appendFileSync(file, JSON.stringify({ timestamp, type, payload }) + '\n');
}

function message(file: string, id: string, text: string, turn = 'turn-live', role = 'assistant', timestamp?: string) {
  append(file, 'response_item', { type: 'message', id, role,
    content: [{ type: role === 'assistant' ? 'output_text' : 'input_text', text }],
    ...(role === 'assistant' ? { phase: 'commentary' } : {}),
    internal_chat_message_metadata_passthrough: { turn_id: turn } }, timestamp);
}

function collab(id: string, receivers: string[], completed = false) {
  append(source, 'event_msg', { type: 'item_completed', thread_id: parent, turn_id: 'turn-live',
    item: { type: 'CollabAgentToolCall', id, tool: completed ? 'wait' : 'spawn_agent', status: 'completed',
      sender_thread_id: parent, receiver_thread_ids: receivers,
      agents_states: completed ? Object.fromEntries(receivers.map(thread => [thread, { completed: 'Investigation complete' }])) : {} } });
}

async function rows() {
  return browser.execute(() => {
    const identities = (window as any).__subagentIdentities ||= {};
    const rows = Array.from({ length: 300 }, (_, index) => ({ index, row: window.__editchainRowAt?.(index) }))
      .filter(entry => entry.row && !entry.row.is_subop)
      .map(({ index, row }) => ({ index, key: row!.continuity_key, node: row!.node_key, lane: row!.lane,
        parents: row!.parents, above: row!.above, below: row!.below, transitions: row!.transitions, task: row!.task_group }));
    for (const row of rows) identities[row.node] = row.key;
    return rows.map(row => ({ ...row, parents: row.parents.map(parent => identities[parent] || parent) }));
  });
}

async function item(id: string) {
  await browser.waitUntil(async () => (await rows()).some(row => row.key.includes(id)), { timeout: 60000, timeoutMsg: `Missing ${id}` });
  return (await rows()).find(row => row.key.includes(id))!;
}

async function idle() {
  await browser.execute(async () => {
    await (window as any).__editchainRendererDebug.whenIdle(10000);
    await Promise.all(document.getAnimations().map(animation => animation.finished.catch(() => undefined)));
  });
}

function validateTopology(snapshot: Awaited<ReturnType<typeof rows>>) {
  expect(new Set(snapshot.map(row => row.key)).size).toBe(snapshot.length);
  const byKey = new Map(snapshot.map(row => [row.key, row]));
  for (const row of snapshot) {
    for (const parent of row.parents) {
      const target = byKey.get(parent);
      if (target) expect(row.index).toBeLessThan(target.index);
    }
  }
  for (let index = 1; index < snapshot.length; index++) {
    expect(snapshot[index - 1].below).toEqual(snapshot[index].above);
  }
}

describe('Physical Codex subagent branches', () => {
  it('retains three sibling spawns, tied startup records, and explicit completion merges', async () => {
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('editchain-history.open');
    });
    const workbench = await browser.getWorkbench();
    let view: Awaited<ReturnType<typeof workbench.getWebviewByTitle>> | undefined;
    await browser.waitUntil(async () => { try { view = await workbench.getWebviewByTitle('EditChain History'); return true; } catch { return false; } }, { timeout: 30000 });
    await view!.open();
    await item('msg_79');
    const original = await rows();
    const mainLane = original.find(row => row.key.includes('msg_79'))!.lane;
    const metadata = JSON.parse(fs.readFileSync(source, 'utf8').split('\n')[0]).payload;
    for (let index = 0; index < children.length; index++) {
      collab(`exec-spawn-${index}`, [children[index][0]]);
    }
    const paths: string[] = [];
    for (let index = 0; index < children.length; index++) {
      const [thread, name] = children[index];
      const file = path.join(workspace, 'sessions', `rollout-child-${index}.jsonl`);
      paths.push(file);
      const timestamp = new Date().toISOString();
      append(file, 'session_meta', { ...metadata, id: thread, timestamp, parent_thread_id: parent,
        source: { subagent: { thread_spawn: { parent_thread_id: parent, depth: 1, agent_nickname: name } } } }, timestamp);
      append(file, 'event_msg', { type: 'task_started', turn_id: `child-turn-${index}`, model_context_window: null }, timestamp);
      // Equal-time developer and user records reproduce the real screenshot's broken branch ends.
      message(file, `child-skills-${index}`, `${name}: investigation instructions`, `child-turn-${index}`, 'developer', timestamp);
      message(file, `child-plugins-${index}`, `${name}: available tools`, `child-turn-${index}`, 'user', timestamp);
      message(file, `child-work-${index}`, `${name}: investigating extension loading`, `child-turn-${index}`);
      await item(`child-work-${index}`);
      await browser.waitUntil(async () => {
        const snapshot = await rows();
        const root = snapshot.find(row => row.key.includes(`child-skills-${index}`));
        const spawn = snapshot.find(row => row.key.includes(`exec-spawn-${index}`));
        return !!root && !!spawn && root.parents.includes(spawn.key);
      }, { timeout: 60000, timeoutMsg: `${name} did not attach to its exact spawn` });
    }
    // The last child arrives while its spawn is still the parent's tip.
    // It must reserve a separate branch before the parent has another row.
    message(source, 'main-continuation', 'Main session continues while three investigations run');
    await item('main-continuation');
    await idle();
    const branched = await rows();
    validateTopology(branched);
    const lanes = children.map((_, index) => branched.find(row => row.key.includes(`child-work-${index}`))!.lane);
    expect(new Set(lanes).size).toBe(3);
    expect(lanes).not.toContain(mainLane);
    expect(branched.find(row => row.key.includes('main-continuation'))!.lane).toBe(mainLane);
    await view!.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('notifications.clearAll'); });
    await view!.open();
    await browser.saveScreenshot(path.resolve('trace/subagent-branches.png'));
    for (let index = 0; index < children.length; index++) {
      message(paths[index], `child-done-${index}`, `${children[index][1]}: investigation complete`, `child-turn-${index}`);
      append(paths[index], 'event_msg', { type: 'task_complete', turn_id: `child-turn-${index}`, last_agent_message: null });
      await item(`child-done-${index}`);
    }
    collab('exec-join-subagents', children.map(([thread]) => thread), true);
    await item('exec-join-subagents');
    await browser.waitUntil(async () => (await rows()).find(row => row.key.includes('exec-join-subagents'))?.parents.length === 4,
      { timeout: 60000, timeoutMsg: 'Completion must retain main continuation and all three child terminals' });
    message(source, 'main-after-join', 'Main session integrates the three completed investigations');
    await item('main-after-join');
    await idle();
    // Offscreen paths now fold by default. Explicitly open them before
    // comparing every physical node and edge across the full branch history.
    for (const folded of (await rows()).filter(row => row.task?.expanded === false)) {
      await browser.execute(key => {
        const row = Array.from(document.querySelectorAll('.row[data-continuity]'))
          .find(row => row.getAttribute('data-continuity') === key);
        (row?.querySelector('.task-chevron') as HTMLButtonElement)?.click();
      }, folded.key);
      await browser.waitUntil(async () => (await rows()).find(row => row.key === folded.key)?.task?.expanded === true, { timeout: 30000 });
      await idle();
    }
    const merged = await rows();
    validateTopology(merged);
    for (const row of [...original, ...branched]) expect(merged.find(current => current.key === row.key)?.lane).toBe(row.lane);
    expect(merged.find(row => row.key.includes('main-after-join'))!.lane).toBe(mainLane);
    await browser.saveScreenshot(path.resolve('trace/subagent-merges.png'));
    fs.writeFileSync(path.resolve('trace/subagents.json'), JSON.stringify({ original, branched, merged, lanes, mainLane }, null, 2));
    await view!.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('editchain-history.stopLive'); });
  });
});
