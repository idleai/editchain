import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const workspace = process.env.EDITCHAIN_LIVE_TEST_ROOT!;
const source = path.join(workspace, 'sessions', 'rollout-live.jsonl');

// Same wrapper -> nested command -> wrapper result sequence as a live Codex
// rollout. Each append is a separate collector transaction, without ending the turn.
function append(type: string, payload: object): void {
  fs.appendFileSync(source, JSON.stringify({ timestamp: new Date().toISOString(), type, payload }) + '\n');
}

function tool(index: number, complete = false): void {
  append('response_item', {
    type: complete ? 'custom_tool_call_output' : 'custom_tool_call',
    id: `${complete ? 'ctco' : 'ctc'}_live_wrapper_${index}`,
    call_id: `call_live_wrapper_${index}`,
    ...(complete ? { output: 'Nested command completed successfully' }
      : { name: 'exec', status: 'completed', input: 'Run the incremental branching check' }),
    internal_chat_message_metadata_passthrough: { turn_id: 'turn-live' },
  });
}

function command(index: number): void {
  append('event_msg', {
    type: 'item_completed', thread_id: '11111111-1111-7111-8111-111111111111', turn_id: 'turn-live',
    item: { type: 'CommandExecution', id: `exec-live-command-${index}`, process_id: null,
      command: ['echo', `Incremental command ${index}`], cwd: pathToFileURL(workspace).href, parsed_cmd: [],
      source: 'unified_exec_startup', status: 'completed', stdout: 'ok', stderr: '',
      aggregated_output: 'ok', exit_code: 0, duration: { secs: 0, nanos: 1000 } },
    started_at_ms: Date.now() - 1, completed_at_ms: Date.now(),
  });
}

async function snapshot() {
  return browser.execute(() => {
    const rows = [];
    for (let index = 0; index < 200; index++) {
      const row = window.__editchainRowAt?.(index);
      if (row && !row.is_subop) rows.push({ key: row.continuity_key, node: row.node_key,
        parents: row.parents, lane: row.lane });
    }
    // HistoryRow exposes physical references; retain their stable item identities
    // as revisions replace physical node IDs in the bounded content cache.
    const identities = (window as any).__branchIdentities ||= {};
    for (const row of rows) identities[row.node] = row.key;
    return rows.map(row => ({ ...row, parents: row.parents.map(parent => identities[parent] || parent) }));
  });
}

async function waitForItem(id: string, previousNode?: string) {
  await browser.waitUntil(async () => (await snapshot()).some(row =>
    row.key.includes(id) && (!previousNode || row.node !== previousNode)),
  { timeout: 60000, timeoutMsg: `${id} did not arrive incrementally` });
  return (await snapshot()).find(row => row.key.includes(id))!;
}

describe('Stable live Codex branching', () => {
  it('keeps one session path through nested commands and repeated tool completions', async () => {
    await browser.executeWorkbench(async vscode => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('workbench.action.closeSidebar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('editchain-history.open');
    });
    const workbench = await browser.getWorkbench();
    let view: Awaited<ReturnType<typeof workbench.getWebviewByTitle>> | undefined;
    await browser.waitUntil(async () => {
      try { view = await workbench.getWebviewByTitle('EditChain History'); return true; } catch { return false; }
    }, { timeout: 30000 });
    await view!.open();
    await browser.waitUntil(async () => (await snapshot()).length > 70, { timeout: 90000 });
    const original = await snapshot();
    const evidence = [];
    let precedingCommand: string | undefined;
    for (let index = 1; index <= 4; index++) {
      tool(index);
      const started = await waitForItem(`ctc_live_wrapper_${index}`);
      if (precedingCommand) expect(started.parents).toEqual([precedingCommand]);
      await browser.waitUntil(() => browser.execute(key => {
        const element = Array.from(document.querySelectorAll('.row[data-continuity]'))
          .find(row => row.getAttribute('data-continuity') === key);
        (window as any).__branchToolElement = element;
        return !!element;
      }, started.key), { timeout: 10000 });
      command(index);
      const nested = await waitForItem(`exec-live-command-${index}`);
      expect(nested.parents).toEqual([started.key]);
      const before = await snapshot();
      tool(index, true);
      const completed = await waitForItem(`ctc_live_wrapper_${index}`, started.node);
      expect(completed.key).toBe(started.key);
      expect(completed.lane).toBe(started.lane);
      await browser.execute(async () => { await (window as any).__editchainRendererDebug.whenIdle(10000); });
      expect(await browser.execute(() => (window as any).__branchToolElement?.isConnected)).toBe(true);
      const after = await snapshot();
      expect(after.length).toBe(before.length);
      for (const previous of [...original, ...before]) {
        expect(after.find(row => row.key === previous.key)?.lane).toBe(previous.lane);
      }
      evidence.push({ started, nested, completed });
      precedingCommand = nested.key;
    }
    append('response_item', { type: 'message', id: 'msg_live_branching_done', role: 'assistant',
      content: [{ type: 'output_text', text: 'Branching check complete: one continuous session path' }],
      phase: 'commentary', internal_chat_message_metadata_passthrough: { turn_id: 'turn-live' } });
    const next = await waitForItem('msg_live_branching_done');
    expect(next.parents).toEqual([precedingCommand]);
    await browser.execute(async () => {
      await Promise.all(document.getAnimations().map(animation => animation.finished.catch(() => undefined)));
    });
    const rows = await snapshot();
    const children = new Map<string, number>();
    for (const row of rows) for (const parent of row.parents) children.set(parent, (children.get(parent) || 0) + 1);
    expect([...children].filter(([parent, count]) => count > 1 && !parent.startsWith('git:'))).toEqual([]);
    expect(fs.readFileSync(source, 'utf8')).not.toContain('task_complete');
    const endpoints = await browser.execute(() => Array.from(document.querySelectorAll('.row[data-continuity]'))
      .filter(row => /ctc_live_wrapper_|exec-live-command-|msg_live_branching_done/.test(row.getAttribute('data-continuity') || ''))
      .filter(row => row.querySelector('.group-label')).map(row => row.getAttribute('data-continuity')));
    expect(endpoints).toEqual([next.key]);
    const centers = await browser.execute(() => (window as any).__editchainRendererDebug.laneXAll());
    expect(centers.slice(1).every((x: number, index: number) => Math.abs(x - centers[index] - 14.76) < 0.01)).toBe(true);
    fs.writeFileSync(path.resolve('trace/live-branching.json'), JSON.stringify({ evidence, rows, centers, endpoints }, null, 2));
    await browser.saveScreenshot(path.resolve('trace/live-branching.png'));
    await view!.close();
    await browser.executeWorkbench(async vscode => { await vscode.commands.executeCommand('editchain-history.stopLive'); });
  });
});
