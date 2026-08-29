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

  // Match a raw pattern like the service's ChainFilter matcher: regex when it
  // compiles, literal substring otherwise.
  function matchPattern(text, pattern) {
    try {
      return new RegExp(pattern).test(text);
    } catch {
      return text.indexOf(pattern) !== -1;
    }
  }

  // Apply a chain filter to a TOP-LEVEL row list (mirrors the service's
  // server-side filtering for the harness). Returns { rows, hiddenKeys }.
  //
  // Semantics are exactly the service's ChainFilter:
  //   - hide_undated hides every undated row unconditionally (including leaves);
  //   - include_kind_pattern is an INCLUSIVE constraint: when set, only rows
  //     whose kind matches are kept — every non-matching kind is excluded
  //     unconditionally, including endpoints;
  //   - summary_pattern / kind_pattern HIDE matching rows, preserving chain
  //     endpoints (rows with no parent or no child in the FULL row set), so
  //     the oldest root and newest leaf stay visible even when they match;
  //   - splice rewrites kept rows' parents to their nearest kept ancestors so
  //     GetWindow parents and GetLayout edges never reference hidden rows.
  // Every pattern uses the service's matcher semantics (regex when it
  // compiles, literal substring otherwise) — see `matchPattern` — and every
  // filtered-out row's node_key is recorded so GetLayout can hide the same
  // rows the GetWindow response excludes (coherent filtered views).
  function applyFilter(rows, filter) {
    if (!filter) return { rows, hiddenKeys: new Set() };
    const hiddenKeys = new Set();
    // Original parent map per node_key (for endpoint detection + splicing).
    const parentsOf = new Map();
    const childrenOf = new Map();
    for (const r of rows) {
      if (r.node_key === undefined) continue;
      const ps = Array.isArray(r.parents) ? r.parents.slice() : [];
      parentsOf.set(r.node_key, ps);
      for (const p of ps) {
        const kids = childrenOf.get(p) || [];
        kids.push(r.node_key);
        childrenOf.set(p, kids);
      }
      if (!childrenOf.has(r.node_key)) childrenOf.set(r.node_key, []);
    }
    const hide = (r) => {
      if (r.node_key !== undefined) hiddenKeys.add(r.node_key);
      return false;
    };
    const kept = [];
    for (const r of rows) {
      // Clone before any splice mutation: applyFilter must be idempotent —
      // GetWindow and GetLayout each run their own pass over the same rows.
      const row = { ...r, parents: Array.isArray(r.parents) ? r.parents.slice() : [] };
      if (filter.hide_undated && !r.timestamp_ms) {
        hide(r);
        continue;
      }
      if (filter.include_kind_pattern &&
          !matchPattern(String(r.kind || ''), filter.include_kind_pattern)) {
        hide(r);
        continue;
      }
      if ((filter.summary_pattern &&
           matchPattern(String(r.summary || ''), filter.summary_pattern)) ||
          (filter.kind_pattern &&
           matchPattern(String(r.kind || ''), filter.kind_pattern))) {
        const ps = parentsOf.get(r.node_key) || [];
        const cs = childrenOf.get(r.node_key) || [];
        const isEndpoint = ps.length === 0 || cs.length === 0;
        if (!isEndpoint) {
          hide(r);
          continue;
        }
      }
      kept.push(row);
    }
    // Splice: reconnect each kept row's parents to its nearest kept ancestors
    // (mirrors the service's `nearest_kept_ancestors` walk).
    if (filter.splice && hiddenKeys.size > 0) {
      const hidden = (k) => hiddenKeys.has(k);
      for (const r of kept) {
        const spliced = [];
        const seen = new Set();
        const walk = (curKey, visited) => {
          if (visited.has(curKey)) return; // cycle guard
          visited.add(curKey);
          const ps = parentsOf.get(curKey) || [];
          for (const p of ps) {
            if (hidden(p)) walk(p, visited);
            else if (!seen.has(p)) {
              seen.add(p);
              spliced.push(p);
            }
          }
        };
        for (const p of (parentsOf.get(r.node_key) || [])) {
          if (hidden(p)) walk(p, new Set());
          else if (!seen.has(p)) {
            seen.add(p);
            spliced.push(p);
          }
        }
        r.parents = spliced;
      }
    }
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
    // Global per-node sub-op counts (for prefix sums). Mirrors the real
    // service: shipped ONLY with the offset-0 window, so the renderer must
    // establish the snapshot from offset zero before paging deep windows.
    const subOpCounts = req.offset === 0
      ? (fixture.subOpCounts || rows.map((r) => (r.sub_ops || []).length))
      : null;
    const expanded = expandSubOps(rows);
    const total = fixture.total !== undefined && fixture.total >= 0
      ? fixture.total
      : expanded.length;
    // Global max lane (mirrors HistoryWindow.max_lane): explicit on the
    // fixture, else derived from the layout rows so the renderer sizes the
    // graph column from real lane data.
    const maxLane = fixture.max_lane !== undefined
      ? fixture.max_lane
      : (fixture.layoutRows || []).reduce((m, r) => Math.max(m, r.lane || 0), 0);
    const slice = expanded.slice(offset, offset + limit);
    return {
      rows: slice,
      total,
      chain_generation: 0,
      max_lane: maxLane,
      sub_op_counts: subOpCounts,
    };
  }

  // Slice a full dataset into a GetLayout response.
  function layoutResponse(fixture, req) {
    const offset = req.offset || 0;
    const limit = req.limit || 0;
    let rows = fixture.layoutRows || [];
    // Coherent with windowResponse: the hidden set is computed from the SAME
    // row population GetWindow filters — submodules are hidden when requested,
    // then the chain filter's hidden keys are added — so layout never shows
    // rows the window response excluded.
    const hiddenKeys = new Set();
    if (req.hide_submodules) {
      for (const r of (fixture.rows || [])) {
        if (r.is_submodule && r.node_key !== undefined) hiddenKeys.add(r.node_key);
      }
    }
    // The window's filter result also carries the SPLICED parent lists; layout
    // reuses them so its edges match the window's reconnected parents exactly.
    const filtered = applyFilter(fixture.rows, req.filter);
    const filterHidden = filtered.hiddenKeys;
    for (const k of filterHidden) hiddenKeys.add(k);
    rows = rows.filter((r) => !hiddenKeys.has(r.node));
    const rowSlice = rows.slice(offset, offset + limit);
    // The spliced parent map (node_key -> nearest kept ancestors) from the
    // window's filter pass.
    const splicedParents = new Map();
    for (const r of filtered.rows) splicedParents.set(r.node_key, r.parents || []);
    const rowIndex = new Map();
    rows.forEach((r, i) => rowIndex.set(r.node, i));
    // Edges reference only kept rows: every edge is emitted for a kept child
    // against its spliced (nearest kept) parents, and dropped when a parent is
    // itself hidden or outside the requested window. Unfiltered fixtures keep
    // their authored edge geometry untouched.
    let edges;
    if (hiddenKeys.size === 0) {
      edges = (fixture.edges || []).filter((e) => {
        const idx = rowIndex.get(e.child);
        return idx !== undefined && idx >= offset && idx < offset + limit;
      });
    } else {
      edges = [];
      for (const [childKey, parents] of splicedParents) {
        const childIdx = rowIndex.get(childKey);
        if (childIdx === undefined || childIdx < offset || childIdx >= offset + limit) continue;
        const childLane = rows[childIdx].lane || 0;
        for (const parentKey of parents) {
          if (hiddenKeys.has(parentKey)) continue; // never reference hidden rows
          const parentIdx = rowIndex.get(parentKey);
          if (parentIdx === undefined) continue;
          const parentLane = rows[parentIdx].lane || 0;
          edges.push({
            child: childKey,
            parent: parentKey,
            points: [
              { row: childIdx, lane: childLane },
              { row: parentIdx, lane: parentLane },
            ],
          });
        }
      }
    }
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
          respond(id, {
            Ok: {
              nodes: n,
              repos: 1,
              ...(fixture.openWarnings ? { warnings: fixture.openWarnings } : {}),
              ...(fixture.diagnostics ? { diagnostics: fixture.diagnostics } : {}),
            },
          });
        }
        return;
      }
      case 'GetWindow': {
        const respondNow = () => respond(id, { Ok: windowResponse(fixture, body.GetWindow) });
        // Controlled-release hook: the stale-response race holds the FIRST
        // GetWindow response and releases it explicitly (no sleep-based
        // timing), so the test is deterministic.
        const hold = window.__editchainHoldWindow;
        if (hold && !hold.taken) {
          hold.taken = true;
          hold.release = respondNow;
        } else {
          respondNow();
        }
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
        // Return the first few rows as search results, mirroring the service's
        // SearchFilters semantics (kind tags + earliest-timestamp bound) and
        // the protocol's JSON-safe SearchHit envelope: every identifier is an
        // exact string (op_id "node:boot:seq", repository decimal, oid hex),
        // never a number that JavaScript could round. Git rows mirror the real
        // service: they carry BOTH a synthetic index-only op_id ("0:0:seq")
        // and the real (git_oid, repository) identity, so the renderer must
        // navigate by the git identity, never the synthetic op id.
        const q = (body.Search.query || '').toLowerCase();
        const filters = body.Search.filters || {};
        let hits = (fixture.rows || []).filter((r) =>
          String(r.summary || '').toLowerCase().includes(q));
        if (Array.isArray(filters.kinds) && filters.kinds.length) {
          // Fixture rows carry lowercase index terms ("message"/"command"…);
          // map the protocol's TagFilter variants ("Message"/"Command"…) down.
          const kinds = new Set(filters.kinds.map((k) => String(k).toLowerCase()));
          hits = hits.filter((r) => kinds.has(String(r.kind || '').toLowerCase()));
        }
        if (typeof filters.after === 'number' && filters.after > 0) {
          hits = hits.filter((r) => !!r.timestamp_ms && r.timestamp_ms >= filters.after);
        }
        const results = hits.slice(0, body.Search.top_k || 20).map((r, i) => ({
          // EditChain hits carry their real op id; git hits carry a synthetic
          // index-only op id plus the real git identity.
          op_id: r.op_id || (r.git_oid ? '0:0:' + i : (r.node_key || null)),
          git_oid: r.git_oid || null,
          repository: r.repository || null,
          text: r.summary || '',
          score: 1,
          source: r.git_oid ? 'Git' : 'EditChain',
          session_id: null,
          actor_id: r.author || '1',
          kind_tags: 0,
          timestamp_ms: r.timestamp_ms || 0,
          generation: 0,
          // Discriminated identity: git hits are kind "git", matching the
          // service; op rows keep their fixture kind.
          is_submodule: !!r.is_submodule,
          kind: r.git_oid ? 'git' : (r.kind || 'message'),
        }));
        const respondNow = () => respond(id, { Ok: { results } });
        // Controlled-release hook: the reversed-search race holds the FIRST
        // Search response and releases it after the newer query has rendered.
        const hold = window.__editchainHoldSearch;
        if (hold && !hold.taken) {
          hold.taken = true;
          hold.release = respondNow;
        } else {
          respondNow();
        }
        return;
      }
      default:
        respond(id, { Error: 'unhandled request in fixture bridge' });
    }
  }

  // --- global vscode shim ----------------------------------------------------

  window.vscode = {
    postMessage(msg) {
      // msg is { id, body } for requests; { type:'log' } etc. are ignored here.
      if (msg && msg.body !== undefined) {
        // The renderer tags every request with a client-generated id; fall
        // back to assigning one for older callers.
        const id = typeof msg.id === 'number' ? msg.id : (++window.__editchainReqId);
        handleRequest(id, msg.body);
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
  // Controlled-release hooks for the deterministic race tests (see
  // layoutProbe). The first GetWindow / Search request after a hook is set is
  // captured as `{ taken:false }` and only responds when the test calls
  // `hook.release()`. No timers — completion is explicit, so the races never
  // depend on wall-clock timing.
  window.__editchainHoldWindow = null;
  window.__editchainHoldSearch = null;

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
      data: {
        id: 'open',
        body: {
          Ok: {
            nodes: n,
            repos: 1,
            ...(fixture.openWarnings ? { warnings: fixture.openWarnings } : {}),
            ...(fixture.diagnostics ? { diagnostics: fixture.diagnostics } : {}),
          },
        },
      },
    }));
    window.dispatchEvent(new MessageEvent('message', {
      data: { id: 'ready', body: { Ok: {} } },
    }));
  };
})();
