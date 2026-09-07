// Node unit tests for the fixture bridge's chain-filter fidelity.
//
// Loads test/harness/fixtureBridge.js (a browser IIFE) in a sandboxed global
// and drives GetWindow / GetLayout requests against a small fixture, asserting
// that the bridge mirrors the Rust ChainFilter semantics exactly:
//   - summary_pattern / kind_pattern HIDE matching rows, preserving chain
//     endpoints (no parent / no child in the full row set), with the service's
//     matcher semantics (regex when it compiles, literal substring otherwise) —
//     NOT plain substring includes;
//   - include_kind_pattern is an INCLUSIVE constraint: only matching kinds are
//     kept, and non-matching kinds are excluded unconditionally (including
//     endpoints), so "messages only" works server-side;
//   - splice reconnects kept rows' parents to their nearest kept ancestors and
//     layout edges never reference hidden rows (hidden-key population);
//   - hide_undated / hide_submodules keep window and layout in agreement.
//
// Run: node --test test/harness/fixtureBridge.filter.test.js
'use strict';

const { test, before, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');

const capturedEvents = [];
let nextReqId = 100;

// --- sandbox: emulate the browser globals fixtureBridge.js expects ----------
global.window = global;
global.MessageEvent = class {
  constructor(type, init) {
    this.type = type;
    this.data = init && init.data;
  }
};
global.dispatchEvent = (ev) => capturedEvents.push(ev);

require(path.join(__dirname, 'fixtureBridge.js'));

function request(body) {
  const id = nextReqId++;
  capturedEvents.length = 0;
  window.vscode.postMessage({ id, body });
  const matches = capturedEvents.filter((ev) => ev.data && ev.data.id === id);
  assert.equal(matches.length, 1, 'expected exactly one response for request id ' + id);
  return matches[0].data.body;
}

function fixtureRows() {
  const base = { lane: 0, above: [], below: [], transitions: [], sub_ops: [] };
  // A 4-row chain, newest first: n:1 -> n:2 -> n:3 -> n:4 (n:1 is the newest
  // root, n:4 the oldest leaf). n:2 is the only tool row; n:4 is a submodule.
  // Every row carries the additive r4 taxonomy with EXACT Rust wire values.
  return [
    { ...base, node_key: 'n:1', summary: 'dated newest', kind: 'message', timestamp_ms: 3000, parents: ['n:2'],
      record_role: 'narrative', activity_kind: 'conversation', visibility: 'primary', outcome: 'unknown', turn_id: 'n:1' },
    { ...base, node_key: 'n:2', summary: 'undated middle', kind: 'tool', timestamp_ms: 0, lane: 1, parents: ['n:3'],
      record_role: 'result', activity_kind: 'execute', visibility: 'primary', outcome: 'unknown', turn_id: 'n:2' },
    { ...base, node_key: 'n:3', summary: 'dated oldest', kind: 'message', timestamp_ms: 1000, parents: ['n:4'],
      record_role: 'narrative', activity_kind: 'conversation', visibility: 'primary', outcome: 'unknown', turn_id: 'n:3' },
    { ...base, node_key: 'n:4', summary: 'submodule note', kind: 'message', timestamp_ms: 2000, is_submodule: true, parents: [],
      record_role: 'narrative', activity_kind: 'conversation', visibility: 'primary', outcome: 'unknown', turn_id: 'n:4' },
  ];
}

function setFixture() {
  const rows = fixtureRows();
  window.__editchainFixture = {
    rows,
    layoutRows: rows.map((r) => ({ node: r.node_key, lane: r.lane })),
    edges: [
      { child: 'n:1', parent: 'n:2', points: [{ row: 0, lane: 0 }, { row: 1, lane: 1 }] },
      { child: 'n:2', parent: 'n:3', points: [{ row: 1, lane: 1 }, { row: 2, lane: 0 }] },
      { child: 'n:3', parent: 'n:4', points: [{ row: 2, lane: 0 }, { row: 3, lane: 0 }] },
    ],
  };
}

function windowKeys(resp) {
  return resp.Ok.rows.map((r) => r.node_key);
}

function layoutNodes(resp) {
  return resp.Ok.rows.map((r) => r.node);
}

function edgeKeys(resp) {
  return resp.Ok.edges.map((e) => e.child + '->' + e.parent);
}

before(() => {
  setFixture();
});

afterEach(() => {
  setFixture(); // every test starts from the same unfiltered fixture
});

test('summary_pattern HIDES matching intermediates and keeps endpoints (GetWindow)', () => {
  // '^dated' matches n:1 (root endpoint — kept) and n:3 (intermediate —
  // hidden). n:2 and n:4 do not match and stay.
  const resp = request({
    GetWindow: {
      offset: 0,
      limit: 100,
      filter: { summary_pattern: '^dated', hide_undated: false, splice: true },
    },
  });
  assert.deepEqual(windowKeys(resp), ['n:1', 'n:2', 'n:4']);
  assert.equal(resp.Ok.total, 3);
  // Splice rewrites n:2's parent to the nearest kept ancestor (n:4).
  const n2 = resp.Ok.rows.find((r) => r.node_key === 'n:2');
  assert.deepEqual(n2.parents, ['n:4']);
});

test('summary_pattern falls back to literal substring for non-regex patterns', () => {
  // '[' is not a valid regex, so the service matcher treats it as a literal.
  const noHits = request({
    GetWindow: {
      offset: 0,
      limit: 100,
      filter: { summary_pattern: '[', hide_undated: false },
    },
  });
  assert.deepEqual(windowKeys(noHits), ['n:1', 'n:2', 'n:3', 'n:4']);
  // A plain keyword still HIDES matching intermediates ('middle' -> n:2).
  const keyword = request({
    GetWindow: {
      offset: 0,
      limit: 100,
      filter: { summary_pattern: 'middle', hide_undated: false, splice: true },
    },
  });
  assert.deepEqual(windowKeys(keyword), ['n:1', 'n:3', 'n:4']);
});

test('GetLayout hides the same rows GetWindow filters out (summary regex)', () => {
  const filter = { summary_pattern: '^dated', hide_undated: false, splice: true };
  const windowResp = request({ GetWindow: { offset: 0, limit: 100, filter } });
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, filter } });
  assert.deepEqual(windowKeys(windowResp), ['n:1', 'n:2', 'n:4']);
  assert.deepEqual(layoutNodes(layoutResp), ['n:1', 'n:2', 'n:4']);
});

test('layout edges never reference hidden rows and follow the splice', () => {
  const filter = { summary_pattern: 'middle|dated oldest', hide_undated: false, splice: true };
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, filter } });
  // n:2 (middle) and n:3 (dated oldest) are hidden intermediates; edges must
  // reconnect n:1 -> n:2? No: n:1's parent n:2 is hidden, so n:1 splices to
  // n:4; the hidden rows appear nowhere in the edge list.
  assert.deepEqual(layoutNodes(layoutResp), ['n:1', 'n:4']);
  const edges = edgeKeys(layoutResp);
  assert.deepEqual(edges, ['n:1->n:4']);
  for (const e of layoutResp.Ok.edges) {
    assert.ok(!['n:2', 'n:3'].includes(e.child), 'edge child must be kept: ' + e.child);
    assert.ok(!['n:2', 'n:3'].includes(e.parent), 'edge parent must be kept: ' + e.parent);
  }
});

test('hide_undated filters window and layout coherently', () => {
  const filter = { summary_pattern: '', kind_pattern: '', include_kind_pattern: '', hide_undated: true, splice: true };
  const windowResp = request({ GetWindow: { offset: 0, limit: 100, filter } });
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, filter } });
  assert.deepEqual(windowKeys(windowResp), ['n:1', 'n:3', 'n:4']);
  assert.deepEqual(layoutNodes(layoutResp), ['n:1', 'n:3', 'n:4']);
  // n:1 splices across the hidden undated n:2 to n:3; edges avoid n:2.
  assert.deepEqual(edgeKeys(layoutResp), ['n:1->n:3', 'n:3->n:4']);
});

test('hide_submodules filters window and layout coherently', () => {
  const windowResp = request({ GetWindow: { offset: 0, limit: 100, hide_submodules: true } });
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, hide_submodules: true } });
  assert.deepEqual(windowKeys(windowResp), ['n:1', 'n:2', 'n:3']);
  assert.deepEqual(layoutNodes(layoutResp), ['n:1', 'n:2', 'n:3']);
  // The edge to the hidden submodule row (n:3 -> n:4) is dropped.
  assert.deepEqual(edgeKeys(layoutResp), ['n:1->n:2', 'n:2->n:3']);
});

test('kind_pattern HIDES matching nodes (regex) with endpoint preservation', () => {
  const filter = { summary_pattern: '', kind_pattern: '^m', hide_undated: false, splice: true };
  const windowResp = request({ GetWindow: { offset: 0, limit: 100, filter } });
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, filter } });
  // '^m' matches kind "message" (n:1, n:3, n:4): n:1 and n:4 are endpoints and
  // stay; n:3 is hidden. The tool n:2 never matches and stays.
  assert.deepEqual(windowKeys(windowResp), ['n:1', 'n:2', 'n:4']);
  assert.deepEqual(layoutNodes(layoutResp), ['n:1', 'n:2', 'n:4']);
});

test('include_kind_pattern keeps ONLY matching kinds, excluding endpoints', () => {
  // Inclusive constraint "messages only": the tool n:2 must be excluded even
  // though it is not an endpoint, and non-matching endpoints are excluded too.
  const filter = {
    summary_pattern: '',
    kind_pattern: '',
    include_kind_pattern: '^(message|command)$',
    hide_undated: false,
    splice: true,
  };
  const windowResp = request({ GetWindow: { offset: 0, limit: 100, filter } });
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, filter } });
  assert.deepEqual(windowKeys(windowResp), ['n:1', 'n:3', 'n:4']);
  assert.deepEqual(layoutNodes(layoutResp), ['n:1', 'n:3', 'n:4']);
  // Splice across the hidden tool: n:1 -> n:3; edges never reference n:2.
  assert.deepEqual(edgeKeys(layoutResp), ['n:1->n:3', 'n:3->n:4']);
  for (const e of layoutResp.Ok.edges) {
    assert.notEqual(e.child, 'n:2');
    assert.notEqual(e.parent, 'n:2');
  }
});

test('include_kind_pattern can keep a lone non-endpoint kind', () => {
  const filter = {
    summary_pattern: '',
    kind_pattern: '',
    include_kind_pattern: '^tool$',
    hide_undated: false,
    splice: true,
  };
  const windowResp = request({ GetWindow: { offset: 0, limit: 100, filter } });
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, filter } });
  assert.deepEqual(windowKeys(windowResp), ['n:2']);
  assert.deepEqual(layoutNodes(layoutResp), ['n:2']);
  // All of n:2's ancestors are non-matching kinds, so it becomes rootless:
  // no edges to hidden rows.
  assert.deepEqual(edgeKeys(layoutResp), []);
});

test('combined filter (regex summary + hide_undated) filters window and layout identically', () => {
  const filter = {
    summary_pattern: '^d',
    kind_pattern: '',
    include_kind_pattern: '',
    hide_undated: true,
    splice: true,
  };
  const windowResp = request({ GetWindow: { offset: 0, limit: 100, filter } });
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, filter } });
  // hide_undated drops n:2; '^d' matches n:1 (endpoint, kept) and n:3
  // (intermediate, hidden). Kept: n:1, n:4.
  assert.deepEqual(windowKeys(windowResp), ['n:1', 'n:4']);
  assert.deepEqual(layoutNodes(layoutResp), ['n:1', 'n:4']);
  assert.deepEqual(edgeKeys(layoutResp), ['n:1->n:4']);
});

test('Search returns the protocol SearchHit envelope with exact string IDs', () => {
  const big = {
    ...fixtureRows()[0],
    node_key: '9007199254740993:0:42',
    op_id: '9007199254740993:0:42',
    summary: 'big id needle',
  };
  window.__editchainFixture.rows.unshift(big);
  const resp = request({
    Search: { query: 'big id needle', mode: 'Lexical', top_k: 5, filters: {} },
  });
  const hit = resp.Ok.results[0];
  assert.ok(hit, 'search returned a hit');
  assert.equal(hit.op_id, '9007199254740993:0:42');
  assert.equal(hit.text, 'big id needle');
  assert.equal(typeof hit.op_id, 'string');
  assert.equal(hit.source, 'EditChain');
  // Git fixture rows keep exact repository/oid strings for search navigation.
  window.__editchainFixture.rows.push({
    ...fixtureRows()[3],
    node_key: 'git:deadbeef',
    git_oid: 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef',
    repository: '9007199254740993',
    summary: 'git needle commit',
  });
  const gitResp = request({
    Search: { query: 'git needle', mode: 'Lexical', top_k: 5, filters: {} },
  });
  const gitHit = gitResp.Ok.results[0];
  assert.equal(gitHit.git_oid, 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef');
  assert.equal(typeof gitHit.repository, 'string');
  assert.equal(gitHit.source, 'Git');
});

test('Search applies kind filters (messages-only) and time bounds', () => {
  const resp = request({
    Search: {
      query: '',
      mode: 'Lexical',
      top_k: 20,
      filters: { kinds: ['Message'], after: 1 },
    },
  });
  // Messages only + dated: n:1 (message), n:3 (message), n:4 (message) stay;
  // the tool n:2 is excluded by kind and n:4 is dated so it stays.
  const texts = resp.Ok.results.map((h) => h.text);
  assert.deepEqual(texts, ['dated newest', 'dated oldest', 'submodule note']);
});

test('Search git hits carry synthetic op_id plus real git identity (navigable)', () => {
  // A git row mirroring the real service: the commit is indexed as a synthetic
  // op ("0:0:seq") that is NOT a projection node, so the hit must ALSO carry
  // the exact (git_oid, repository) identity the renderer navigates with.
  const gitRow = {
    ...fixtureRows()[3],
    node_key: 'git:deadbeef',
    git_oid: 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef',
    repository: '9007199254740993',
    is_submodule: true,
    summary: 'git needle commit',
  };
  window.__editchainFixture.rows.push(gitRow);
  const resp = request({
    Search: { query: 'git needle', mode: 'Lexical', top_k: 5, filters: {} },
  });
  const hit = resp.Ok.results[0];
  assert.equal(hit.source, 'Git');
  assert.equal(hit.kind, 'git');
  assert.equal(hit.is_submodule, true);
  assert.equal(hit.git_oid, 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef');
  assert.equal(typeof hit.git_oid, 'string');
  assert.equal(hit.repository, '9007199254740993');
  assert.equal(typeof hit.repository, 'string');
  // The synthetic index-only op_id is present (the real service ships it) but
  // the renderer must never navigate with it: it is not in the projection.
  assert.ok(/^0:0:\d+$/.test(hit.op_id), 'synthetic op id shape: ' + hit.op_id);
  assert.equal(typeof hit.op_id, 'string');
  // EditChain hits keep their exact op_id and never borrow git identity.
  const opResp = request({
    Search: { query: 'dated newest', mode: 'Lexical', top_k: 5, filters: {} },
  });
  const opHit = opResp.Ok.results[0];
  assert.equal(opHit.op_id, 'n:1');
  assert.equal(opHit.source, 'EditChain');
  assert.equal(opHit.git_oid, null);
  assert.equal(opHit.repository, null);
  assert.equal(typeof opHit.op_id, 'string');
});

// --- r4: Activity/Raw profile (hide_trace) + additive HistoryRow fields -----

test('hide_trace=true filters visibility "trace" rows from window AND layout', () => {
  // A 5-row chain with two trace-VISIBILITY records (t:1, t:3). Activity
  // (hide_trace=true) must remove them unconditionally from both GetWindow
  // and GetLayout, and splice the kept parents so edges never reference a
  // hidden row. All taxonomy values are the exact Rust wire enums.
  const rows = [
    { ...fixtureRows()[0], node_key: 't:0', summary: 'agent reply', kind: 'message', record_role: 'narrative', activity_kind: 'conversation', visibility: 'primary', parents: ['t:1'] },
    { ...fixtureRows()[1], node_key: 't:1', summary: 'tool result one', kind: 'tool', record_role: 'result', activity_kind: 'execute', visibility: 'trace', parents: ['t:2'] },
    { ...fixtureRows()[2], node_key: 't:2', summary: 'run tests', kind: 'command', record_role: 'action', activity_kind: 'execute', visibility: 'primary', parents: ['t:3'] },
    { ...fixtureRows()[3], node_key: 't:3', summary: 'tool result two', kind: 'tool', record_role: 'result', activity_kind: 'execute', visibility: 'trace', parents: ['t:4'] },
    { ...fixtureRows()[0], node_key: 't:4', summary: 'user asks', kind: 'message', record_role: 'narrative', activity_kind: 'conversation', visibility: 'primary', parents: [] },
  ];
  window.__editchainFixture = {
    rows,
    layoutRows: rows.map((r) => ({ node: r.node_key, lane: 0 })),
    edges: [
      { child: 't:0', parent: 't:1', points: [{ row: 0, lane: 0 }, { row: 1, lane: 0 }] },
      { child: 't:1', parent: 't:2', points: [{ row: 1, lane: 0 }, { row: 2, lane: 0 }] },
      { child: 't:2', parent: 't:3', points: [{ row: 2, lane: 0 }, { row: 3, lane: 0 }] },
      { child: 't:3', parent: 't:4', points: [{ row: 3, lane: 0 }, { row: 4, lane: 0 }] },
    ],
  };
  const filter = {
    summary_pattern: '',
    kind_pattern: '',
    include_kind_pattern: '',
    hide_undated: false,
    splice: true,
    hide_trace: true,
  };
  const windowResp = request({ GetWindow: { offset: 0, limit: 100, filter } });
  const layoutResp = request({ GetLayout: { offset: 0, limit: 100, filter } });
  // Kept: t:0, t:2, t:4 — trace-VISIBILITY records vanish entirely (no
  // endpoint keeping). Regression guard: hiding is driven by VISIBILITY —
  // record_role has no "trace" variant, so a non-trace-visibility row is
  // never filtered by this flag.
  assert.deepEqual(windowKeys(windowResp), ['t:0', 't:2', 't:4']);
  for (const kept of windowResp.Ok.rows) {
    assert.notEqual(kept.visibility, 'trace', 'kept row must not be trace-visibility');
    assert.ok(['narrative', 'action'].includes(kept.record_role),
      'kept record_role is exact Rust value: ' + kept.record_role);
    assert.ok(['conversation', 'execute'].includes(kept.activity_kind),
      'kept activity_kind is exact Rust value: ' + kept.activity_kind);
  }
  assert.equal(windowResp.Ok.total, 3);
  // Splice: t:0 has no kept parent; t:2's nearest kept ancestor is t:4.
  const t2 = windowResp.Ok.rows.find((r) => r.node_key === 't:2');
  assert.deepEqual(t2.parents, ['t:4']);
  assert.deepEqual(layoutNodes(layoutResp), ['t:0', 't:2', 't:4']);
  const edges = edgeKeys(layoutResp);
  // t:0's only parent (t:1) is hidden, so it splices to the nearest kept
  // ancestor (t:2); t:2 splices to t:4. No edge may reference a hidden row.
  assert.deepEqual(edges, ['t:0->t:2', 't:2->t:4']);
  for (const e of layoutResp.Ok.edges) {
    assert.notEqual(e.child, 't:1');
    assert.notEqual(e.parent, 't:1');
    assert.notEqual(e.child, 't:3');
    assert.notEqual(e.parent, 't:3');
  }
});

test('hide_trace=false (or omitted) keeps trace-visibility rows (Raw view)', () => {
  const rows = [
    { ...fixtureRows()[0], node_key: 't:0', summary: 'agent reply', record_role: 'narrative', activity_kind: 'conversation', visibility: 'primary', parents: ['t:1'] },
    { ...fixtureRows()[1], node_key: 't:1', summary: 'tool result', kind: 'tool', record_role: 'result', activity_kind: 'execute', visibility: 'trace', parents: [] },
  ];
  window.__editchainFixture = {
    rows,
    layoutRows: rows.map((r) => ({ node: r.node_key, lane: 0 })),
    edges: [{ child: 't:0', parent: 't:1', points: [{ row: 0, lane: 0 }, { row: 1, lane: 0 }] }],
  };
  // Explicit hide_trace:false (Raw profile) keeps everything.
  const raw = request({
    GetWindow: { offset: 0, limit: 100, filter: { splice: true, hide_trace: false } },
  });
  assert.deepEqual(windowKeys(raw), ['t:0', 't:1']);
  // Omitted hide_trace means false (ChainFilterDto serde default) — same view.
  const omitted = request({
    GetWindow: { offset: 0, limit: 100, filter: { splice: true } },
  });
  assert.deepEqual(windowKeys(omitted), ['t:0', 't:1']);
});

test('additive HistoryRow fields pass through GetWindow unchanged (exact Rust values)', () => {
  const rows = [
    {
      ...fixtureRows()[0],
      node_key: 't:0',
      summary: 'agent reply',
      kind: 'message',
      record_role: 'narrative',
      activity_kind: 'conversation',
      visibility: 'primary',
      outcome: 'success',
      turn_id: 'turn-42',
    },
  ];
  window.__editchainFixture = {
    rows,
    layoutRows: rows.map((r) => ({ node: r.node_key, lane: 0 })),
    edges: [],
  };
  const resp = request({ GetWindow: { offset: 0, limit: 100, filter: { splice: true } } });
  const row = resp.Ok.rows[0];
  assert.equal(row.record_role, 'narrative');
  assert.equal(row.activity_kind, 'conversation');
  assert.equal(row.visibility, 'primary');
  assert.equal(row.outcome, 'success');
  assert.equal(row.turn_id, 'turn-42');
});

test('wire taxonomy is restricted to the exact Rust enum values', () => {
  // Regression guard: the fixture rows must never carry the legacy
  // tool_call/command/edit/commit/review, error/interrupted, record_role
  // "turn"/"tool"/"trace", or visibility "hidden"/"visible" vocabulary that
  // drifted from the Rust wire enums.
  const rows = fixtureRows();
  const roles = new Set(['narrative', 'action', 'result', 'artifact', 'lifecycle', 'echo', 'unknown']);
  const kinds = new Set(['work', 'conversation', 'plan', 'explore', 'execute', 'change', 'verify',
    'diagnose', 'coordinate', 'source_control', 'external', 'system', 'unknown']);
  const visibilities = new Set(['primary', 'supporting', 'trace', 'unknown']);
  const outcomes = new Set(['success', 'warning', 'failure', 'cancelled', 'unknown']);
  const legacyByField = {
    record_role: ['turn', 'tool', 'trace', 'commit', 'message', 'error'],
    activity_kind: ['tool_call', 'command', 'edit', 'commit', 'review',
      'error', 'interrupted', 'tool_result', 'message', 'turn'],
    visibility: ['visible', 'hidden'],
    outcome: ['error', 'interrupted', 'successful', 'failed'],
  };
  for (const r of rows) {
    for (const [field, allowed] of [['record_role', roles], ['activity_kind', kinds],
      ['visibility', visibilities], ['outcome', outcomes]]) {
      assert.ok(allowed.has(r[field]),
        r.node_key + ' ' + field + '="' + r[field] + '" not in Rust enum ' + JSON.stringify([...allowed]));
    }
    for (const [field, legacy] of Object.entries(legacyByField)) {
      assert.ok(!legacy.includes(r[field]),
        r.node_key + ' ' + field + ' uses legacy value "' + r[field] + '"');
    }
  }
});

test('GetNodeDetails resolves bundled sub-op records by their op_id', () => {
  const rows = [
    {
      ...fixtureRows()[0],
      node_key: 'n:c:1',
      summary: 'combined op',
      kind: 'message',
      sub_ops: [
        { op_id: 'n:c:1::sub:0', summary: 'mode', kind: 'mode' },
      ],
    },
  ];
  window.__editchainFixture = {
    rows,
    layoutRows: rows.map((r) => ({ node: r.node_key, lane: 0 })),
    edges: [],
    subOpCounts: [1],
  };
  const resp = request({ GetNodeDetails: { op_id: 'n:c:1::sub:0' } });
  assert.equal(resp.Ok.summary, 'mode');
});

test('ResolveObject returns the git commit identity and message', () => {
  const rows = [
    {
      ...fixtureRows()[3],
      node_key: 'git:deadbeef',
      git_oid: 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef',
      repository: '9007199254740993',
      summary: 'git needle commit',
      author: 'ambientlight',
      timestamp_ms: 2000,
    },
  ];
  window.__editchainFixture = {
    rows,
    layoutRows: rows.map((r) => ({ node: r.node_key, lane: 0 })),
    edges: [],
  };
  const resp = request({
    ResolveObject: { repository: '9007199254740993', oid: 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef' },
  });
  assert.equal(resp.Ok.oid, 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef');
  assert.equal(resp.Ok.repository, '9007199254740993');
  assert.equal(resp.Ok.message, 'git needle commit');
  assert.equal(resp.Ok.author, 'ambientlight');
});
