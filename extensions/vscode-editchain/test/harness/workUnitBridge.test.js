// Node unit tests for the round-two Activity-view wire contract through the
// fixture bridge: work_unit / session_summary / promoted / activity_bundle fidelity.
//
// Loads test/harness/fixtureBridge.js (a browser IIFE) in a sandboxed global
// and drives GetWindow against the `workUnits` fixture, asserting:
//   - the bridge faithfully PASSES THROUGH authored work_unit /
//     session_summary / promoted / activity_bundle metadata and DEFAULTS them
//     on rows that
//     omit them (mirroring the HistoryRow serde defaults);
//   - the activity_bundle kind is coerced through the wire enum exactly like
//     serde: "work-group", "execute-run", and "plan-repeat" survive, any
//     other string maps
//     to "unknown" (forward compatibility), so clients style only recognized
//     typed bundles;
//   - the fixture itself satisfies the view-wide invariants the renderer
//     relies on: exactly one is_start/is_end per unit id, counts equal the
//     per-view tallies, exact bundle member_counts, exact promoted sets, and
//     fallback (title null) for units without narrative evidence.
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
  window.__editchainStart();
  const snapshot = capturedEvents.findLast((ev) => ev.data?.id === 'open').data.body.Ok.snapshot_id;
  for (const request of Object.values(body)) request.snapshot_id = snapshot;
  const id = nextReqId++;
  capturedEvents.length = 0;
  window.vscode.postMessage({ id, body });
  const matches = capturedEvents.filter((ev) => ev.data && ev.data.id === id);
  assert.equal(matches.length, 1, 'expected exactly one response for request id ' + id);
  return matches[0].data.body;
}

function windowRows(fixtureName) {
  window.__editchainSetScenario(fixtureName);
  const resp = request({
    GetWindow: { offset: 0, limit: 1000, include_layout: true },
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
  const rows = fixture.rows;
  const counts = new Map();
  rows.forEach((row, index) => {
    const id = row.work_unit.id;
    assert.ok(id && typeof id === 'string', 'row ' + index + ' work_unit.id');
    counts.set(id, (counts.get(id) || 0) + 1);
  });
  for (const id of counts.keys()) {
    const starts = rows.filter((row) => row.work_unit.id === id && row.work_unit.is_start);
    const ends = rows.filter((row) => row.work_unit.id === id && row.work_unit.is_end);
    assert.equal(starts.length, 1, 'unit ' + id + ' has exactly one start');
    assert.equal(ends.length, 1, 'unit ' + id + ' has exactly one end');
    assert.equal(starts[0].work_unit.count, counts.get(id));
  }
  assert.deepEqual(Object.fromEntries(counts), {
    'session:s1/turn:t1': 7,
    'session:s1/turn:t2': 4,
    'repo:ops': 2,
  });
  const sessionRows = rows.filter((row) => row.group.startsWith('session:'));
  const summaries = sessionRows.filter((row) => row.session_summary);
  assert.equal(summaries.length, 1);
  assert.equal(summaries[0], sessionRows[0]);
  assert.equal(summaries[0].session_summary.count, sessionRows.length);
  const title = (id) => rows.find((row) => row.work_unit.id === id)?.work_unit.title;
  assert.equal(title('session:s1/turn:t1'), '**User asks** to fix `the build`');
  assert.equal(title('session:s1/turn:t2'), 'User asks to check the result');
  assert.equal(title('repo:ops'), null);
});

test('exact bundle metadata: member_count matches folded members; promoted sets exact', () => {
  const actRows = fixture.rows;
  const byKey = new Map(actRows.map((r) => [r.node_key, r]));
  // Typed execute-run bundles (run1 unknown-clean, run2 all-success), one
  // Plan-repeat bundle, and one forward-compatible unknown kind. The ordinary
  // execute-with-subops row carries NO activity_bundle even though its summary
  // reads like a run.
  const bundles = [
    ['wu:run1', 'execute-run', 3],
    ['wu:run2', 'execute-run', 2],
    ['wu:plans', 'plan-repeat', 3],
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
  const expectedPromoted = ['wu:req1', 'wu:req2', 'wu:fail', 'wu:chg', 'wu:ver'];
  assert.deepEqual(
    fixture.rows.filter((row) => row.promoted).map((row) => row.node_key),
    expectedPromoted
  );
});

test('bridge PASSES THROUGH authored activity metadata in the fixed Activity view', () => {
  const ok = windowRows(FIXTURE);
  assert.equal(ok.rows.filter((r) => !r.is_subop).length, 13,
    'activity view serves 13 top-level rows (plus expanded sub-op rows)');
  const subopRows = ok.rows.filter((r) => r.is_subop);
  assert.equal(subopRows.length, 14, 'bundle members + metadata sub-ops are expandable sub-op rows');
  for (const r of subopRows) {
    assert.equal(r.work_unit, null, 'sub-op rows never carry work-unit metadata');
    assert.equal(r.session_summary, null, 'sub-op rows never carry session-summary metadata');
    assert.equal(r.promoted, false, 'sub-op rows are never promoted');
    assert.equal(r.activity_bundle, null, 'sub-op rows never carry bundle metadata');
  }
  const byKey = new Map(ok.rows.map((r) => [r.node_key, r]));
  const req1 = byKey.get('wu:req1');
  assert.deepEqual(req1.work_unit, {
    id: 'session:s1/turn:t1',
    is_start: true,
    is_end: false,
    title: '**User asks** to fix `the build`',
    count: 7,
  });
  assert.deepEqual(req1.session_summary, { count: 11 });
  assert.equal(req1.promoted, true);
  const run1 = byKey.get('wu:run1');
  assert.deepEqual(run1.activity_bundle, { kind: 'execute-run', member_count: 3 });
  assert.equal(run1.promoted, false, 'bundle rows are never promoted on the wire');
  const plans = byKey.get('wu:plans');
  assert.deepEqual(plans.activity_bundle, { kind: 'plan-repeat', member_count: 3 });
  assert.equal(plans.summary, '**Planning build and dry-run import steps**');
  assert.equal(plans.activity_kind, 'plan');
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
  const ok = request({ GetWindow: { offset: 0, limit: 10, include_layout: true } }).Ok;
  assert.equal(ok.rows.length, 1);
  const row = ok.rows[0];
  assert.equal(row.work_unit, null, 'missing work_unit defaults to null (serde default)');
  assert.equal(row.session_summary, null, 'missing session_summary defaults to null');
  assert.equal(row.promoted, false, 'missing promoted defaults to false');
  assert.equal(row.activity_bundle, null, 'missing activity_bundle defaults to null');

  // Enum coercion through the bridge.
  window.__editchainFixture = {
    rows: [
      { ...legacy, node_key: 'b:1', summary: 'typed run', kind: 'command',
        activity_bundle: { kind: 'execute-run', member_count: 2 } },
      { ...legacy, node_key: 'b:2', summary: 'typed plans', kind: 'reflection',
        activity_bundle: { kind: 'plan-repeat', member_count: 3 } },
      { ...legacy, node_key: 'b:work', summary: 'typed work', kind: 'work-group',
        activity_bundle: { kind: 'work-group', member_count: 5 } },
      { ...legacy, node_key: 'b:3', summary: 'unknown kind run', kind: 'command',
        activity_bundle: { kind: 'checkpoint', member_count: 4 } },
    ],
    layoutRows: [],
    edges: [],
  };
  const coerced = request({ GetWindow: { offset: 0, limit: 10, include_layout: true } }).Ok.rows;
  assert.equal(coerced.find((r) => r.node_key === 'b:1').activity_bundle.kind, 'execute-run',
    'typed kind survives the enum round trip');
  assert.equal(coerced.find((r) => r.node_key === 'b:2').activity_bundle.kind, 'plan-repeat',
    'typed Plan-repeat kind survives the enum round trip');
  assert.equal(coerced.find((r) => r.node_key === 'b:work').activity_bundle.kind, 'work-group',
    'typed work-group kind survives the enum round trip');
  assert.equal(coerced.find((r) => r.node_key === 'b:3').activity_bundle.kind, 'unknown',
    'unknown wire strings coerce to the Unknown variant (forward compatibility)');
});
