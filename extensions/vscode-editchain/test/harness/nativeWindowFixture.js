'use strict';

/** Deterministic native paging bridge shared by smoke tests and the update benchmark. */
async function installNativeWindowFixture(page, count = 2000) {
  await page.evaluate(count => {
    const base = window.__editchainFixture.rows[0];
    const rows = Array.from({ length: count }, (_, index) => ({ ...base,
      node_key: `native:${index}`, continuity_key: `native:${index}`, summary: `Row ${index}`,
      lane: 0, above: [], below: [], transitions: [], parents: [], sub_ops: [], is_subop: false,
      parent_row: null, native_expanded: false, task_group: null,
    }));
    rows[0].task_group = { task_id: 'native-task', thread_id: 'thread', turn_id: 'turn',
      status: 'completed', member_count: 4, anchor: 'native:0', expanded: true, summarized: false };
    const state = window.__nativeWindow = { rows, revision: 0, serial: count, expanded: true,
      held: null, holdToggle: false, versions: new Map(rows.map((row, index) => [row.node_key, index + 1])),
      requests: [], sentRows: 0, reusedRows: 0, bytes: 0, settled: 0, waiters: [], actions: [], latencies: [] };
    const members = new Set(['native:1', 'native:2', 'native:3']);
    const visible = () => state.expanded ? rows : rows.filter(row => !members.has(row.node_key));
    const version = key => state.versions.get(key).toString(16).padStart(64, '0');
    const respond = (id, value) => {
      const data = { id, body: { Ok: value } };
      state.bytes += JSON.stringify(data).length;
      window.dispatchEvent(new MessageEvent('message', { data }));
    };
    const locations = keys => keys.map(key => ({ key, node_key: key,
      row: visible().findIndex(row => row.continuity_key === key),
    })).filter(row => row.row >= 0);
    const work = { source_bytes: 0, provider_records: 0, provider_bootstraps: 0, chain_bytes: 0,
      chain_records: 0, presentation_ops: 0, items: 0, occurrences: 0, blocks: 0, capture_ms: 0, projection_ms: 0 };
    state.update = (index = 0, disclosure = false) => {
      const base_revision = state.revision++;
      state.versions.set(rows[index].node_key, ++state.serial);
      if (!disclosure) rows[index].summary = `Revision ${state.revision}`;
      const started = performance.now();
      const done = new Promise(resolve => state.waiters.push({ revision: state.revision, resolve, started }));
      respond(disclosure ? 'disclosure' : 'delta', { epoch: 'native', revision: state.revision, work, deltas: [{
        base_revision, revision: state.revision, snapshot_id: `native:${state.revision}`, total: visible().length,
        visible_total: visible().length, removed: [], upserts: [], chain_generation: state.revision, max_lane: 0, work,
      }] });
      return done;
    };
    state.prepend = () => {
      const key = `inserted:${++state.serial}`;
      rows.unshift({ ...rows[0], node_key: key, continuity_key: key, task_group: null });
      return state.update();
    };
    window.vscode.postMessage = message => {
      if (message.type === 'liveSettled' && message.snapshot_id?.startsWith('native:')) {
        if (message.error) throw new Error(message.error);
        state.settled = Number(message.snapshot_id.split(':')[1]);
        for (const waiter of state.waiters.filter(waiter => waiter.revision <= state.settled)) {
          state.latencies.push(performance.now() - waiter.started);
          waiter.resolve();
        }
        state.waiters = state.waiters.filter(waiter => waiter.revision > state.settled);
        for (const action of state.actions.filter(action => action.revision <= state.settled)) {
          window.dispatchEvent(new MessageEvent('message', { data: { id: 'disclosureDone', body: action } }));
        }
        state.actions = state.actions.filter(action => action.revision > state.settled);
      }
      if (message.type === 'toggleDisclosure') {
        const toggle = () => {
          state.expanded = !state.expanded;
          const parent = rows.find(row => row.node_key === message.key);
          parent.task_group.expanded = state.expanded;
          parent.task_group.summarized = !state.expanded;
          state.actions.push({ key: message.key, task: message.task, revision: state.revision + 1, error: null });
          return state.update(rows.indexOf(parent), true);
        };
        if (state.holdToggle) state.held = toggle;
        else void toggle();
      }
      if (!message.body) return;
      const type = Object.keys(message.body)[0];
      const request = message.body[type];
      state.requests.push({ type, ...request });
      const current = visible();
      const snapshot_id = `native:${state.revision}`;
      if (request.snapshot_id !== snapshot_id) {
        window.dispatchEvent(new MessageEvent('message', { data: { id: message.id, body: { Error: { code: 'stale_snapshot', message: 'retired fixture revision' } } } }));
        return;
      }
      if (type === 'ReconcileRows') {
        const found = locations(request.keys);
        const anchor = request.anchors.map(key => found.find(row => row.key === key)).find(Boolean)?.row ?? request.offset;
        const offset = Math.max(0, Math.min(anchor, current.length - 1) - request.before);
        const known = new Map(request.known.map(row => [row.key, row.version]));
        const patch = current.slice(offset, offset + request.limit).map(content => {
          const key = content.continuity_key;
          if (known.get(key) === version(key)) { state.reusedRows++; return { key, version: version(key) }; }
          state.sentRows++; return { key, version: version(key), content };
        });
        respond(message.id, { snapshot_id, locations: found, offset, rows: patch,
          total: current.length, max_lane: 0, chain_generation: state.revision });
      } else if (type === 'GetWindow') {
        const pageRows = current.slice(request.offset, request.offset + request.limit);
        state.sentRows += pageRows.length;
        respond(message.id, { snapshot_id, total: current.length, rows: pageRows, chain_generation: state.revision,
          max_lane: 0, layout_ready: true, sub_op_counts: null, expansion_spans: request.offset === 0 ? [] : null });
      } else if (type === 'LocateRows') {
        respond(message.id, { snapshot_id, rows: locations(request.keys) });
      }
    };
    respond('open', { protocol_version: 2, snapshot_id: 'native:0', nodes: count, repos: 1,
      live: { epoch: 'native', revision: 0, total: count, blocks: [], paged: true, reconcile_rows: true } });
    respond('ready', {});
  }, count);
}

module.exports = { installNativeWindowFixture };
