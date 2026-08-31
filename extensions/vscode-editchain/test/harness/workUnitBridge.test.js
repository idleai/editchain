// Node unit tests for the round-two Activity-view wire contract through the
// fixture bridge: work_unit / promoted / activity_bundle fidelity.
//
// Loads test/harness/fixtureBridge.js (a browser IIFE) in a sandboxed global
// and drives GetWindow / GetLayout against the `workUnits` fixture, asserting:
//   - the bridge faithfully PASSES THROUGH authored work_unit / promoted /
//     activity_bundle metadata and DEFAULTS the additive fields on rows that
//     omit them (mirroring the HistoryRow serde defaults);
//   - the activity_bundle kind is coerced through the wire enum exactly like
//     serde: "execute-run" survives, any other string maps to "unknown"
//     (forward compatibility), so clients can style ONLY typed execute-run
//     bundles;
//   - the Raw profile (hide_trace=false on the wire) is served the unbundled
//     raw stream: members are top-level rows again, activity_bundle is None
//     everywhere, while work_unit/promoted metadata is still present (the
//     service annotates raw views too);
//   - the fixture itself satisfies the view-wide invariants the renderer
//     relies on: exactly one is_start/is_end per unit id, counts equal the
//     per-view tallies, exact bundle member_counts, exact promoted sets, and
//     fallback (title null) for units without narrative evidence;
//   - GetLayout stays coherent with the profile's window rows (raw layout
//     matches raw rows; edges only reference kept nodes).
//
// Run: node --test test/harness/workUnitBridge.test.js
'use strict';

const { test, before } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');

const capturedEvents = [];
let nextReqId = 1000;

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
require(path.join(__dirname, 'fixtures.js'));

function request(body) {
  const id = nextReqId++;
  capturedEvents.length = 0;
  window.vscode.postMessage({ id, body });
  const matches = capturedEvents.filter((ev) => ev.data && ev.data.id === id);
  assert.equal(matches.length, 1, 'expected exactly one response for request id ' + id);
  return matches[0].data.body;
}

function windowRows(fixtureName, filter) {
  window.__editchainSetScenario(fixtureName);
  const resp = request({
    GetWindow: { offset: 0, limit: 1000, filter: filter || {} },
  });
  assert.ok(resp.Ok, 'GetWindow must succeed: ' + JSON.stringify(resp.Error || resp));
  return resp.Ok;
}

const FIXTURE = 'workUnits';
let fixture;

before(() => {
  fixture = window.__editchainFixtures[FIXTURE]();
});

test('fixture view-wide invariants: one start/end per unit id, exact counts, titles', () => {
  for (const [rows, label] of [[fixture.rows, 'activity'], [fixture.rawRows, 'raw']]) {
    const first = new Map();
    const last = new Map();
    const counts = new Map();
    rows.forEach((r, i) => {
      const id = r.work_unit.id;
      assert.ok(id && typeof id === 'string', label + ' row ' + i + ' work_unit.id');
      if (!first.has(id)) first.set(id, i);
      last.set(id, i);
      counts.set(id, (counts.get(id) || 0) + 1);
    });
    for (const [id, i] of first) {
      const starts = rows.filter((r) => r.work_unit.id === id && r.work_unit.is_start);
      const ends = rows.filter((r) => r.work_unit.id === id && r.work_unit.is_end);
      assert.equal(starts.length, 1, label + ' unit ' + id + ' has exactly one start');
      assert.equal(ends.length, 1, label + ' unit ' + id + ' has exactly one end');
      assert.equal(starts[0].work_unit.count, counts.get(id), label + ' start count matches per-view tally');
      assert.equal(starts[0].work_unit.is_start, true);
      assert.equal(starts[0].work_unit.is_end, false);
      assert.equal(ends[0].work_unit.is_end, true);
      assert.equal(ends[0].work_unit.is_start, false);
    }
    // Exact authored tallies (Activity 6/4/2, Raw 11/5/2).
    const expected = label === 'activity'
      ? { 'session:s1/turn:t1': 6, 'session:s1/turn:t2': 4, 'repo:ops': 2 }
      : { 'session:s1/turn:t1': 11, 'session:s1/turn:t2': 5, 'repo:ops': 2 };
    assert.deepEqual(Object.fromEntries(counts), expected, label + ' per-unit counts');
  }
  // Titled units carry the oldest narrative summary; the fallback ops unit
  // (no narrative evidence) has title null in BOTH views.
  for (const rows of [fixture.rows, fixture.rawRows]) {
    const title = (id) => {
      const r = rows.find((x) => x.work_unit.id === id);
      return r && r.work_unit.title;
    };
    assert.equal(title('session:s1/turn:t1'), 'User asks to fix the build');
    assert.equal(title('session:s1/turn:t2'), 'User asks to check the result');
    assert.equal(title('repo:ops'), null);
  }
});

test('exact bundle metadata: member_count matches folded members; promoted sets exact', () => {
  const actRows = fixture.rows;
  const byKey = new Map(actRows.map((r) => [r.node_key, r]));
  // Typed execute-run bundles (run1 unknown-clean, run2 all-success) + one
  // forward-compatible unknown kind; the ordinary execute-with-subops row
  // carries NO activity_bundle even though its summary reads like a run.
  const bundles = [
    ['wu:run1', 'execute-run', 3],
    ['wu:run2', 'execute-run', 2],
    ['wu:xbundle', 'checkpoint', 4],
  ];
  for (const [key, kind, count] of bundles) {
    const r = byKey.get(key);
    assert.ok(r && r.activity_bundle, key + ' must carry activity_bundle');
    assert.equal(r.activity_bundle.kind, kind, key + ' bundle kind');
    assert.equal(r.activity_bundle.member_count, count, key + ' exact member_count');
    assert.equal(r.promoted, false, key + ' bundle rows are never promoted');
    assert.ok(r.work_unit.count > 0, key + ' carries a positive work-unit count');
  }
  assert.equal(byKey.get('wu:execsub').activity_bundle, null,
    'ordinary execute-with-subops row must not carry activity_bundle');
  assert.ok((byKey.get('wu:execsub').sub_ops || []).length === 2);
  // Raw view: bundles unfolded, activity_bundle None everywhere, metadata kept.
  const rawByKey = new Map(fixture.rawRows.map((r) => [r.node_key, r]));
  for (const key of ['wu:a1', 'wu:a2', 'wu:a3', 'wu:b1', 'wu:b2', 'wu:x1', 'wu:x2', 'wu:x3', 'wu:x4']) {
    const r = rawByKey.get(key);
    assert.ok(r, key + ' must exist unfolded in raw rows');
    assert.equal(r.activity_bundle, null, key + ' raw rows never bundle');
    assert.ok(r.work_unit && r.work_unit.id, key + ' raw rows keep work_unit metadata');
  }
  assert.equal(fixture.rawRows.some((r) => r.activity_bundle), false, 'no raw row carries activity_bundle');
  const expectedPromoted = ['wu:req1', 'wu:req2', 'wu:fail', 'wu:chg', 'wu:ver'];
  for (const rows of [fixture.rows, fixture.rawRows]) {
    assert.deepEqual(
      rows.filter((r) => r.promoted).map((r) => r.node_key),
      expectedPromoted,
      'promoted set must be identical across profiles'
    );
  }
});

test('bridge PASSES THROUGH authored work_unit/promoted/activity_bundle in GetWindow (Activity profile)', () => {
  const ok = windowRows(FIXTURE, { hide_trace: true });
  assert.equal(ok.rows.filter((r) => !r.is_subop).length, 12,
    'activity view serves 12 top-level rows (plus expanded sub-op rows)');
  const subopRows = ok.rows.filter((r) => r.is_subop);
  assert.equal(subopRows.length, 11, 'bundle members + metadata sub-ops are expandable sub-op rows');
  for (const r of subopRows) {
    assert.equal(r.work_unit, null, 'sub-op rows never carry work-unit metadata');
    assert.equal(r.promoted, false, 'sub-op rows are never promoted');
    assert.equal(r.activity_bundle, null, 'sub-op rows never carry bundle metadata');
  }
  const byKey = new Map(ok.rows.map((r) => [r.node_key, r]));
  const req1 = byKey.get('wu:req1');
  assert.deepEqual(req1.work_unit, {
    id: 'session:s1/turn:t1',
    is_start: true,
    is_end: false,
    title: 'User asks to fix the build',
    count: 6,
  });
  assert.equal(req1.promoted, true);
  const run1 = byKey.get('wu:run1');
  assert.deepEqual(run1.activity_bundle, { kind: 'execute-run', member_count: 3 });
  assert.equal(run1.promoted, false, 'bundle rows are never promoted on the wire');
});

test('bridge DEFAULTS additive fields and coerces the bundle kind enum (serde parity)', () => {
  // A minimal legacy row (no additive fields at all).
  const legacy = {
    node_key: 'legacy:1',
    summary: 'old row',
    kind: 'message',
    timestamp_ms: 1,
    parents: [],
    group: 'session:s1',
    lane: 0,
    above: [],
    below: [],
    transitions: [],
  };
  window.__editchainFixture = {
    rows: [legacy],
    layoutRows: [{ node: legacy.node_key, lane: 0 }],
    edges: [],
  };
  const ok = request({ GetWindow: { offset: 0, limit: 10, filter: {} } }).Ok;
  assert.equal(ok.rows.length, 1);
  const row = ok.rows[0];
  assert.equal(row.work_unit, null, 'missing work_unit defaults to null (serde default)');
  assert.equal(row.promoted, false, 'missing promoted defaults to false');
  assert.equal(row.activity_bundle, null, 'missing activity_bundle defaults to null');

  // Enum coercion through the bridge.
  window.__editchainFixture = {
    rows: [
      { ...legacy, node_key: 'b:1', summary: 'typed run', kind: 'command',
        activity_bundle: { kind: 'execute-run', member_count: 2 } },
      { ...legacy, node_key: 'b:2', summary: 'unknown kind run', kind: 'command',
        activity_bundle: { kind: 'checkpoint', member_count: 4 } },
    ],
    layoutRows: [],
    edges: [],
  };
  const coerced = request({ GetWindow: { offset: 0, limit: 10, filter: {} } }).Ok.rows;
  assert.equal(coerced.find((r) => r.node_key === 'b:1').activity_bundle.kind, 'execute-run',
    'typed kind survives the enum round trip');
  assert.equal(coerced.find((r) => r.node_key === 'b:2').activity_bundle.kind, 'unknown',
    'unknown wire strings coerce to the Unknown variant (forward compatibility)');
});

test('bridge serves the raw stream for hide_trace=false and keeps GetLayout coherent', () => {
  const rawWindow = windowRows(FIXTURE, { hide_trace: false });
  assert.equal(rawWindow.rows.length, 18, 'raw profile serves 18 top-level rows');
  assert.equal(rawWindow.rows.filter((r) => r.activity_bundle).length, 0, 'raw rows never carry bundle rows');
  assert.ok(rawWindow.rows.every((r) => r.work_unit && r.work_unit.id !== undefined), 'raw rows keep work_unit');
  assert.ok(rawWindow.rows.some((r) => r.promoted), 'raw rows keep promotion metadata');
  // Unit markers are raw-view-wide (counts recomputed over the raw list).
  const run1member = rawWindow.rows.find((r) => r.node_key === 'wu:a2');
  assert.equal(run1member.work_unit.count, 11, 'raw member unit count uses the raw view tally');

  const rawLayout = request({ GetLayout: { offset: 0, limit: 1000, filter: { hide_trace: false } } }).Ok;
  assert.equal(rawLayout.rows.length, 18, 'raw layout matches raw row count');
  const layoutKeys = new Set(rawLayout.rows.map((r) => r.node));
  assert.equal(layoutKeys.size, 18);
  for (const e of rawLayout.edges) {
    assert.ok(layoutKeys.has(e.child), 'edge child ' + e.child + ' must exist in raw layout rows');
    assert.ok(layoutKeys.has(e.parent), 'edge parent ' + e.parent + ' must exist in raw layout rows');
  }
  // And the Activity profile (hide_trace true / absent) keeps its own layout.
  const actLayout = request({ GetLayout: { offset: 0, limit: 1000, filter: { hide_trace: true } } }).Ok;
  assert.equal(actLayout.rows.length, 12);
  for (const e of actLayout.edges) {
    assert.ok(new Set(actLayout.rows.map((r) => r.node)).has(e.child));
  }
});
