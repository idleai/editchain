// Fixture bridge: a test replacement for VS Code's `acquireVsCodeApi()`.
//
// Loaded in the harness page BEFORE media/main.js. It defines a global `vscode`
// object (postMessage / getState / setState) that dispatches requests against
// the selected scenario's protocol fixtures instead of a real service.
//
// The renderer sends `{ body: <RequestBody> }` via vscode.postMessage and
// expects responses as `{ id, body }` events on window. We emulate the
// extension host: for each request we compute a response body and dispatch a
// `message` event with the matching id.

(function () {
  'use strict';

  // --- state store (getState/setState) --------------------------------------
  let persistedState = undefined;

  // --- request dispatch ------------------------------------------------------

  // Apply a chain filter to a row list (mirrors the service's server-side
  // filtering for the harness). Returns { rows, hiddenKeys }.
  function applyFilter(rows, filter) {
    if (!filter) return { rows, hiddenKeys: new Set() };
    const hiddenKeys = new Set();
    const kept = rows.filter((r) => {
      if (filter.hide_undated && !r.timestamp_ms) return false;
      if (filter.summary_pattern && !String(r.summary || '').includes(filter.summary_pattern)) return false;
      return true;
    });
    return { rows: kept, hiddenKeys };
  }

  // Expand a top-level row's bundled sub_ops into a fixed fully-expanded flat
  // list (parent + one row per sub-op), mirroring the service. Each sub-op row
  // is flagged is_subop and draws every lane passing straight through its region
  // as a full-height line (above == below), with no dot.
  function expandSubOps(rows) {
    const out = [];
    for (let ri = 0; ri < rows.length; ri++) {
      const row = rows[ri];
      out.push(row);
      const subs = row.sub_ops || [];
      if (!subs.length) continue;
      // Lanes passing through this region = intersect(row_below[parent],
      // row_above[next]). The next top-level row's `above` lists lanes entering
      // it from above; the parent's `below` lists lanes leaving it downward.
      const nextRow = rows[ri + 1];
      const belowParent = row.below || [];
      const aboveNext = nextRow ? (nextRow.above || []) : [];
      const regionLanes = belowParent.filter((l) => aboveNext.includes(l));
      for (let i = 0; i < subs.length; i++) {
        const sub = subs[i];
        out.push({
          op_id: sub.op_id,
          git_oid: null,
          repository: null,
          summary: sub.summary,
          timestamp_ms: sub.timestamp_ms,
          group: row.group,
          node_key: row.node_key + '::sub:' + i,
          parents: [],
          is_submodule: false,
          is_system: true,
          author: '',
          commit_id: '',
          kind: sub.kind,
          lane: row.lane || 0,
          above: regionLanes.slice(),
          below: regionLanes.slice(),
          transitions: [],
          sub_ops: [],
          is_subop: true,
          parent_row: out.length - 1 - subs.length + i,
          subop_kind: sub.kind,
        });
      }
    }
    return out;
  }

  // Slice a full dataset into a GetWindow response.
  function windowResponse(fixture, req) {
    const offset = req.offset || 0;
    const limit = req.limit || 0;
    const hideSub = !!req.hide_submodules;
    let rows = fixture.rows || [];
    if (hideSub) rows = rows.filter((r) => !r.is_submodule);
    const filtered = applyFilter(rows, req.filter);
    rows = filtered.rows;
    // Global per-node sub-op counts (for prefix sums), shipped with every window
    // so a deep jump doesn't depend on the offset==0 window having been fetched.
    const subOpCounts = (fixture.subOpCounts || rows.map((r) => (r.sub_ops || []).length));
    const expanded = expandSubOps(rows);
    const total = fixture.total !== undefined && fixture.total >= 0
      ? fixture.total
      : expanded.length;
    const slice = expanded.slice(offset, offset + limit);
    return {
      rows: slice,
      total,
      chain_generation: 0,
      sub_op_counts: subOpCounts,
    };
  }

  // Slice a full dataset into a GetLayout response.
  function layoutResponse(fixture, req) {
    const offset = req.offset || 0;
    const limit = req.limit || 0;
    let rows = fixture.layoutRows || [];
    if (req.hide_submodules && fixture.rows) {
      const hidden = new Set(
        fixture.rows.filter((r) => r.is_submodule).map((r) => r.node_key)
      );
      rows = rows.filter((r) => !hidden.has(r.node));
    }
    if (req.filter && fixture.rows) {
      const filtered = applyFilter(fixture.rows, req.filter);
      const hiddenKeys = filtered.hiddenKeys;
      rows = rows.filter((r) => !hiddenKeys.has(r.node));
    }
    const rowSlice = rows.slice(offset, offset + limit);
    // Edges whose child falls inside [offset, offset+limit).
    const edges = (fixture.edges || []).filter((e) => {
      const idx = rows.findIndex((r) => r.node === e.child);
      return idx >= offset && idx < offset + limit;
    });
    return { rows: rowSlice, edges };
  }

  function respond(id, body) {
    window.dispatchEvent(new MessageEvent('message', { data: { id, body } }));
  }

  function handleRequest(id, body) {
    const fixture = window.__editchainFixture || {};
    if (!body || typeof body !== 'object') return;

    switch (body.Open !== undefined ? 'Open' : Object.keys(body)[0]) {
      case 'Open': {
        if (fixture.openError) {
          respond(id, { Error: fixture.openError });
        } else {
          const n = (fixture.rows || []).length;
          respond(id, { Ok: { nodes: n, repos: 1 } });
        }
        return;
      }
      case 'GetWindow': {
        respond(id, { Ok: windowResponse(fixture, body.GetWindow) });
        return;
      }
      case 'GetLayout': {
        respond(id, { Ok: layoutResponse(fixture, body.GetLayout) });
        return;
      }
      case 'GetNodeDetails': {
        const opId = body.GetNodeDetails.op_id;
        const row = (fixture.rows || []).find((r) => r.node_key === opId);
        if (row) {
          respond(id, { Ok: { op_id: opId, git_oid: null, repository: null,
            summary: row.summary, body: row.summary, parents: [], git_parents: [],
            refs: [], changed_paths: [] } });
        } else {
          respond(id, { Error: 'node not found' });
        }
        return;
      }
      case 'Search': {
        // Return the first few rows as search results.
        const q = (body.Search.query || '').toLowerCase();
        const hits = (fixture.rows || []).filter((r) =>
          r.summary.toLowerCase().includes(q));
        respond(id, { Ok: hits.slice(0, body.Search.top_k || 20) });
        return;
      }
      default:
        respond(id, { Error: 'unhandled request in fixture bridge' });
    }
  }

  // --- global vscode shim ----------------------------------------------------

  window.vscode = {
    postMessage(msg) {
      // msg is { body } for requests; { type:'log' } etc. are ignored here.
      if (msg && msg.body !== undefined && msg.id === undefined) {
        // Renderer sends requests without an id; assign one and dispatch.
        handleRequest(++window.__editchainReqId, msg.body);
      }
      // openJson / log messages are no-ops in the harness.
    },
    getState() {
      return persistedState;
    },
    setState(state) {
      persistedState = state;
    },
  };

  // acquireVsCodeApi is called by main.js; provide it too.
  window.acquireVsCodeApi = function () {
    return window.vscode;
  };

  window.__editchainReqId = 0;

  // Expose a way to select a scenario from the harness page / puppeteer.
  window.__editchainSetScenario = function (name) {
    const all = window.__editchainFixtures || {};
    if (!all[name]) throw new Error('unknown scenario: ' + name);
    window.__editchainFixture = all[name]();
    window.__editchainScenarioName = name;
    persistedState = undefined;
  };

  // Emulate the extension host's startup handshake (see extension.ts
  // openHistoryView): after the renderer is ready, push an `open` message with
  // the workspace summary, then a `ready` message so it starts loading.
  window.__editchainStart = function () {
    const fixture = window.__editchainFixture || {};
    const n = (fixture.rows || []).length;
    if (fixture.openError) {
      window.dispatchEvent(new MessageEvent('message', {
        data: { id: 'open', body: { Error: fixture.openError } },
      }));
      return;
    }
    window.dispatchEvent(new MessageEvent('message', {
      data: { id: 'open', body: { Ok: { nodes: n, repos: 1 } } },
    }));
    window.dispatchEvent(new MessageEvent('message', {
      data: { id: 'ready', body: { Ok: {} } },
    }));
  };
})();