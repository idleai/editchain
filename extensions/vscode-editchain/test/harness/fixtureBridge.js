// Fixture replacement for VS Code's acquireVsCodeApi(). It implements only
// the two requests the Rust history renderer sends: GetWindow and
// FindInHistory.

(function () {
  'use strict';

  let persistedState;

  function fixedRows(fixture) {
    const source = fixture.rows || [];
    const parents = new Map(source.map((row) => [
      row.node_key,
      Array.isArray(row.parents) ? row.parents.slice() : [],
    ]));
    const hidden = new Set(source
      .filter((row) => row.is_submodule || row.visibility === 'trace' || !row.timestamp_ms)
      .map((row) => row.node_key));

    const nearestVisible = (key, visiting, output) => {
      if (visiting.has(key)) return;
      visiting.add(key);
      if (!hidden.has(key)) {
        output.add(key);
        return;
      }
      for (const parent of parents.get(key) || []) {
        nearestVisible(parent, visiting, output);
      }
    };

    return source
      .filter((row) => !hidden.has(row.node_key))
      .map((row) => {
        const visibleParents = new Set();
        for (const parent of parents.get(row.node_key) || []) {
          nearestVisible(parent, new Set(), visibleParents);
        }
        return { ...row, parents: [...visibleParents] };
      });
  }

  function normalizeActivityFields(row) {
    const output = { ...row };
    if (!Object.prototype.hasOwnProperty.call(output, 'work_unit')) output.work_unit = null;
    if (!Object.prototype.hasOwnProperty.call(output, 'session_summary')) {
      output.session_summary = null;
    }
    if (!Object.prototype.hasOwnProperty.call(output, 'promoted')) output.promoted = false;
    if (!Object.prototype.hasOwnProperty.call(output, 'activity_bundle')) {
      output.activity_bundle = null;
    }
    if (!Object.prototype.hasOwnProperty.call(output, 'group_end')) output.group_end = false;
    if (!Object.prototype.hasOwnProperty.call(output, 'hierarchy_depth')) {
      output.hierarchy_depth = output.is_subop ? 1 : 0;
    }
    const bundle = output.activity_bundle;
    if (bundle && typeof bundle === 'object') {
      output.activity_bundle = {
        ...bundle,
        kind: ['work-group', 'execute-run', 'plan-repeat'].includes(bundle.kind)
          ? bundle.kind
          : 'unknown',
      };
    }
    return output;
  }

  function expandSubOps(rows) {
    const output = [];
    for (let rowIndex = 0; rowIndex < rows.length; rowIndex += 1) {
      const row = rows[rowIndex];
      const parentRow = output.length;
      output.push(row);
      const subOps = row.sub_ops || [];
      if (subOps.length === 0) continue;
      const nextRow = rows[rowIndex + 1];
      const regionLanes = (row.below || []).filter((lane) =>
        (nextRow ? nextRow.above || [] : []).includes(lane));
      for (let index = 0; index < subOps.length; index += 1) {
        const subOp = subOps[index];
        const fileChange = subOp.file_change && typeof subOp.file_change === 'object'
          ? subOp.file_change
          : null;
        output.push({
          op_id: subOp.op_id,
          git_oid: fileChange?.commit_oid || null,
          repository: fileChange?.repository || null,
          summary: subOp.summary,
          timestamp_ms: subOp.timestamp_ms,
          group: row.group,
          group_end: false,
          node_key: row.node_key + '::sub:' + index,
          parents: [],
          is_submodule: false,
          is_system: !fileChange,
          author: '',
          commit_id: '',
          kind: fileChange ? 'file' : subOp.kind,
          record_role: fileChange ? 'artifact' : (subOp.record_role || 'unknown'),
          activity_kind: fileChange ? 'change' : (subOp.activity_kind || 'unknown'),
          visibility: fileChange ? 'supporting' : (subOp.visibility || 'supporting'),
          outcome: 'unknown',
          chain_state: row.chain_state || 'active',
          lane: row.lane || 0,
          above: regionLanes.slice(),
          below: regionLanes.slice(),
          transitions: [],
          sub_ops: [],
          is_subop: true,
          hierarchy_depth: 1,
          parent_row: parentRow,
          subop_kind: subOp.kind,
          file_change: fileChange,
          work_unit: null,
          session_summary: null,
          promoted: false,
          activity_bundle: null,
        });
      }
    }
    return output;
  }

  function windowResponse(fixture, request) {
    const offset = request.offset || 0;
    const limit = request.limit || 0;
    const rows = fixedRows(fixture).map((row, index, all) => ({
      ...row,
      group_end: !all[index + 1] || all[index + 1].group !== row.group,
    }));
    const subOpCounts = offset === 0
      ? rows.map((row) => (row.sub_ops || []).length)
      : null;
    let expansionSpans = null;
    if (subOpCounts) {
      expansionSpans = [];
      let absoluteRow = 0;
      for (const count of subOpCounts) {
        if (count > 0) expansionSpans.push({ row: absoluteRow, descendant_count: count });
        absoluteRow += 1 + count;
      }
    }
    const expanded = expandSubOps(rows);
    const includeLayout = request.include_layout === true;
    const slice = expanded.slice(offset, offset + limit).map((row) => {
      const output = normalizeActivityFields(row);
      if (!includeLayout) {
        output.lane = 0;
        output.above = [];
        output.below = [];
        output.transitions = [];
      }
      return output;
    });
    const maxLane = fixture.max_lane ?? (fixture.layoutRows || [])
      .reduce((maximum, row) => Math.max(maximum, row.lane || 0), 0);
    return {
      rows: slice,
      total: expanded.length,
      chain_generation: 0,
      max_lane: includeLayout ? maxLane : 0,
      sub_op_counts: subOpCounts,
      expansion_spans: expansionSpans,
      layout_ready: includeLayout,
    };
  }

  function findInHistoryResponse(fixture, request) {
    const query = String(request.query || '').toLowerCase();
    const rows = fixedRows(fixture);
    const topK = typeof request.top_k === 'number' && request.top_k > 0 ? request.top_k : 25;
    const candidates = rows.filter((row) =>
      String(row.summary || '').toLowerCase().includes(query));
    const starts = new Map();
    let absoluteRow = 0;
    for (const row of rows) {
      starts.set(row.node_key, absoluteRow);
      absoluteRow += 1 + (row.sub_ops || []).length;
    }
    const capped = candidates.slice(0, topK);
    return {
      matches: capped.map((row) => ({
        node_key: row.node_key,
        row: starts.get(row.node_key),
      })),
      more: capped.length >= topK,
    };
  }

  function respond(id, body) {
    window.dispatchEvent(new MessageEvent('message', { data: { id, body } }));
  }

  function handleRequest(id, body) {
    const fixture = window.__editchainFixture || {};
    if (!body || typeof body !== 'object') return;
    window.__editchainRequestLog.push(body);
    const requestName = Object.keys(body)[0];
    if (requestName === 'GetWindow') {
      const respondNow = () => respond(id, { Ok: windowResponse(fixture, body.GetWindow) });
      const layoutHold = window.__editchainHoldLayoutWindow;
      if (body.GetWindow.include_layout === true && layoutHold && !layoutHold.taken) {
        layoutHold.taken = true;
        layoutHold.release = respondNow;
        return;
      }
      const hold = window.__editchainHoldWindow;
      if (hold && !hold.taken) {
        hold.taken = true;
        hold.release = respondNow;
      } else {
        respondNow();
      }
      return;
    }
    if (requestName === 'FindInHistory') {
      if (window.__editchainFindError) {
        respond(id, { Error: window.__editchainFindError });
        return;
      }
      const respondNow = () => respond(id, {
        Ok: findInHistoryResponse(fixture, body.FindInHistory),
      });
      const hold = window.__editchainHoldFind;
      if (hold && !hold.taken) {
        hold.taken = true;
        hold.release = respondNow;
      } else {
        respondNow();
      }
      return;
    }
    respond(id, { Error: 'unhandled request in fixture bridge' });
  }

  window.vscode = {
    postMessage(message) {
      if (message?.body !== undefined) {
        const id = typeof message.id === 'number'
          ? message.id
          : (++window.__editchainReqId);
        handleRequest(id, message.body);
      }
    },
    getState() {
      return persistedState;
    },
    setState(state) {
      persistedState = state;
    },
  };

  window.acquireVsCodeApi = () => window.vscode;
  window.__editchainReqId = 0;
  window.__editchainRequestLog = [];
  window.__editchainClearRequestLog = () => {
    window.__editchainRequestLog = [];
  };
  window.__editchainHoldWindow = null;
  window.__editchainHoldLayoutWindow = null;
  window.__editchainHoldFind = null;
  window.__editchainFindError = null;

  window.__editchainSetScenario = function (name) {
    const all = window.__editchainFixtures || {};
    if (!all[name]) throw new Error('unknown scenario: ' + name);
    window.__editchainFixture = all[name]();
    window.__editchainScenarioName = name;
    persistedState = undefined;
    window.__editchainRequestLog = [];
  };

  window.__editchainStart = function () {
    const fixture = window.__editchainFixture || {};
    const body = fixture.openError
      ? { Error: fixture.openError }
      : {
        Ok: {
          nodes: fixedRows(fixture).length,
          repos: 1,
          ...(fixture.openWarnings ? { warnings: fixture.openWarnings } : {}),
          ...(fixture.diagnostics ? { diagnostics: fixture.diagnostics } : {}),
        },
      };
    window.dispatchEvent(new MessageEvent('message', {
      data: { id: 'open', body },
    }));
    if (!fixture.openError) {
      window.dispatchEvent(new MessageEvent('message', {
        data: { id: 'ready', body: { Ok: {} } },
      }));
    }
  };
})();
