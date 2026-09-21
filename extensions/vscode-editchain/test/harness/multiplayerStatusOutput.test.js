'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { MultiplayerStatusOutput } = require('../../out/multiplayer/statusOutput');

const fingerprint = 'd0e9612942717b35e7fc4f566bbd2766132e50214bd50eba3ac0b58b1100a318';
const peer = (changes = {}) => ({ fingerprint, state: 'Catching up', progress: {
  accepted: true, records: 128, blobs: 5, rounds: 0, synchronizing: true, unavailable: 0, sent_records: 0, sent_blobs: 0, ...changes,
} });
const status = (peers = [peer()]) => ({ space: 'test-space', enabled: true, hosting: false, peers });
function observe(t, value = status()) {
  t.mock.timers.enable({ apis: ['Date', 'setInterval'], now: Date.UTC(2026, 8, 21) });
  const lines = [], output = new MultiplayerStatusOutput(line => lines.push(line));
  t.after(() => output.dispose());
  output.show(value);
  return { lines, output, tick: milliseconds => t.mock.timers.tick(milliseconds) };
}

test('live status follows incoming saved counts without reopening or inventing a total', t => {
  const { lines, output, tick } = observe(t, status([peer({ records: 0, blobs: 0 })]));
  assert.match(lines.join('\n'), /Total remaining and percentage are unknown/);
  assert.match(lines.at(-1), /Connected; checking shared history \(first pass\).*Received here: 0 records, 0 content objects/);
  lines.length = 0;
  // Coalesce a burst of worker responses into the latest saved totals.
  for (let blobs = 1; blobs <= 5; blobs++) output.update(status([peer({ blobs })]));
  assert.equal(lines.length, 0);
  tick(1000);
  assert.equal(lines.length, 1);
  assert.match(lines[0], /^\[2026-09-21T00:00:01.000Z\] Peer d0e961294271: Connected/);
  assert.match(lines[0], /Received here: 128 records, 5 content objects \(this connection\)/);
  assert.match(lines[0], /Completed passes: 0; missing-content responses: 0/);
  assert.match(lines[0], /Last saved-data update observed 1s ago/);
});

test('waiting heartbeats keep appearing even when the worker emits nothing', t => {
  const { lines, output, tick } = observe(t);
  lines.length = 0;
  tick(14_000);
  assert.equal(lines.length, 0);
  tick(1000);
  assert.equal(lines.length, 1);
  assert.match(lines[0], /No new saved-data update observed in 15s/);
  output.update(status([peer({ blobs: 6 })]));
  tick(1000);
  assert.match(lines.at(-1), /6 content objects.*Last saved-data update observed 1s ago/);
  tick(15_000);
  assert.match(lines.at(-1), /Last saved-data update observed 16s ago/);
});

test('completion and unavailable content are visible without idle round spam', t => {
  const { lines, output, tick } = observe(t);
  output.update(status([{ ...peer({ rounds: 1, synchronizing: false, unavailable: 2 }), state: 'Waiting for content' }]));
  tick(1000);
  assert.match(lines.at(-1), /Connected; waiting for content.*Completed passes: 1; missing-content responses: 2/);
  output.update(status([{ ...peer({ rounds: 2, synchronizing: false }), state: 'Live' }]));
  tick(1000);
  assert.match(lines.at(-1), /Connected; caught up at last check/);
  lines.length = 0;
  // The periodic native reconciliation can run many empty passes while idle.
  for (let rounds = 3; rounds < 30; rounds++) {
    output.update(status([peer({ rounds })]));
    output.update(status([{ ...peer({ rounds, synchronizing: false }), state: 'Live' }]));
    tick(1000);
  }
  assert.equal(lines.length, 0);
});

test('separate peers and reconnections retain independent receipt activity', t => {
  const other = { ...peer({ records: 7, blobs: 2 }), fingerprint: 'f'.repeat(64) };
  const { lines, output, tick } = observe(t, status([peer(), other]));
  tick(5000);
  output.update(status([peer({ blobs: 6 }), other]));
  tick(1000);
  assert.match(lines.at(-1), /Peer d0e961294271.*Last saved-data update observed 1s ago/);
  tick(9000);
  assert.match(lines.at(-1), /Peer ffffffffffff.*No new saved-data update observed in 15s/);
  output.update(status([{ fingerprint, state: 'Waiting to reconnect' }, other]));
  tick(1000);
  assert.match(lines.at(-1), /Peer d0e961294271: Waiting to reconnect/);
  output.update(status([peer({ records: 0, blobs: 0 }), other]));
  tick(1000);
  assert.match(lines.at(-1), /Received here: 0 records, 0 content objects.*No new saved-data update observed in 1s/);
  output.update(status([other]));
  assert.match(lines.at(-1), /Peer d0e961294271: connection no longer listed/);
  lines.length = 0;
  tick(30_000);
  assert.ok(!lines.some(line => line.includes('Peer d0e961294271')));
});

test('connecting events do not continually reset the waiting heartbeat', t => {
  const connecting = status([{ fingerprint, state: 'Connecting' }]);
  const { lines, output, tick } = observe(t, connecting);
  lines.length = 0;
  for (let seconds = 0; seconds < 30; seconds++) { output.update(connecting); tick(1000); }
  assert.equal(lines.length, 2);
  assert.ok(lines.every(line => line.includes('Peer d0e961294271: Connecting.')));
});

test('opening twice has one watcher; stop and disposal cancel it, and a later session resumes it', t => {
  const { lines, output, tick } = observe(t);
  output.show(status());
  assert.equal(lines.filter(line => line.includes('Live multiplayer status:')).length, 1);
  lines.length = 0;
  tick(15_000);
  assert.equal(lines.length, 1);
  output.update({ enabled: false, hosting: false, peers: [] });
  assert.match(lines.at(-1), /Sharing stopped/);
  lines.length = 0;
  tick(60_000);
  assert.equal(lines.length, 0);
  output.update(status());
  tick(1000);
  assert.ok(lines.some(line => line.includes('checking shared history (first pass)')));
  output.dispose(); lines.length = 0;
  tick(60_000); output.update(status()); output.show(status());
  assert.equal(lines.length, 0);
});

test('outgoing confirmations update the log even when no incoming records change', t => {
  const { lines, output, tick } = observe(t);
  lines.length = 0;
  output.update(status([peer({ sent_records: 26, sent_blobs: 7 })]));
  tick(1000);
  assert.equal(lines.length, 1);
  assert.match(lines[0], /Received here: 128 records, 5 content objects/);
  assert.match(lines[0], /Sent \(confirmed saved by peer\): 26 records, 7 content objects/);
  assert.match(lines[0], /Last send confirmation observed 1s ago/);
  assert.match(lines[0], /No new saved-data update observed in 1s/);
});

test('authentication identifies an existing connection without reporting a disconnect', t => {
  const { lines, output, tick } = observe(t, status([{ connection: 'edge-1', state: 'Authenticating', progress: peer({ accepted: false, records: 0, blobs: 0 }).progress }]));
  tick(20_000); lines.length = 0;
  output.update(status([{ ...peer({ records: 0, blobs: 0 }), connection: 'edge-1' }]));
  tick(1000);
  assert.equal(lines.length, 1);
  assert.match(lines[0], /Peer d0e961294271: Connected/);
  assert.match(lines[0], /No new saved-data update observed in 1s/);
  assert.ok(!lines.some(line => line.includes('connection no longer listed')));
});

test('a joining peer can acquire and retire its connection ID without a false disappearance', t => {
  const { lines, output, tick } = observe(t, status([{ fingerprint, state: 'Connecting' }]));
  lines.length = 0;
  output.update(status([{ ...peer(), connection: 'outgoing-edge' }]));
  tick(1000);
  assert.match(lines.at(-1), /Peer d0e961294271: Connected/);
  output.update(status([{ fingerprint, state: 'Waiting to reconnect' }]));
  tick(1000);
  assert.match(lines.at(-1), /Waiting to reconnect/);
  assert.ok(!lines.some(line => line.includes('connection no longer listed')));
});
