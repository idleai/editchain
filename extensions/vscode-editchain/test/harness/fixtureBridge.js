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

  // Slice a full dataset into a GetWindow response.
  function windowResponse(fixture, req) {
    const offset = req.offset || 0;
    const limit = req.limit || 0;
    const hideSub = !!req.hide_submodules;
    let rows = fixture.rows || [];
    if (hideSub) rows = rows.filter((r) => !r.is_submodule);
    const total = fixture.total !== undefined && fixture.total >= 0
      ? fixture.total
      : rows.length;
    const slice = rows.slice(offset, offset + limit);
    return { rows: slice, total, chain_generation: 0 };
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