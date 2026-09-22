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
  assert.match(lines.join('\n'), /Totals are fixed at the start of each pass/);
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

const check = (changes = {}) => ({ pass: 1, total_records: 1000, checked_records: 250, complete: false, unavailable: 0, ...changes });
const work = (changes = {}) => ({ incoming: check(), outgoing: check({ checked_records: 500 }), pending_records: 0, pending_blobs: 5, download: null, ...changes });

test('both directions show percentages and remaining checks, then update while bytes are still unsaved', t => {
  const { sharingLabel, sharingDetails } = require('../../out/multiplayer/statusBar');
  const value = status([peer(work())]);
  const { lines, output, tick } = observe(t, value);
  assert.match(lines.at(-1), /Receiving check #1: 25%.*250\/1,000 records checked; 750 remaining/);
  assert.match(lines.at(-1), /Sending \(peer confirmed\) check #1: 50%.*500\/1,000 records checked; 500 remaining/);
  assert.match(lines.at(-1), /5 known content downloads left/);
  assert.equal(sharingLabel(value), 'Sharing · 1/1 connected · ↓25% ↑50%');
  assert.match(sharingDetails(value), /Receiving ↓ check #1: 25%/);
  lines.length = 0;
  output.update(status([peer(work({ download: { content: true, received_bytes: 65536, total_bytes: 190000 } }))]));
  tick(1000);
  assert.equal(lines.length, 1, 'byte-only updates are observable without durable counter changes');
  assert.match(lines[0], /Downloading content: 65,536\/190,000 bytes \(34.4%\); not yet saved/);
  assert.match(lines[0], /Receiving check #1: 25%/, 'partial content cannot finish its page');
  assert.match(lines[0], /Received here: 128 records, 5 content objects/, 'partial bytes do not invent durable receipts');
  output.update(status([peer(work({ incoming: check({ checked_records: 500 }), pending_blobs: 0 }))]));
  tick(1000);
  assert.match(lines.at(-1), /Receiving check #1: 50%.*500 remaining/);
});

test('unknown totals, empty checks, missing content and a new pass have explicit meanings', t => {
  const { sharingLabel } = require('../../out/multiplayer/statusBar');
  const unknown = check({ checked_records: 0, total_records: null });
  const { lines, output, tick } = observe(t, status([peer(work({ incoming: unknown }))]));
  assert.match(lines.at(-1), /Receiving: waiting for the shared-history total/);
  assert.ok(!lines.at(-1).includes('Receiving check #1: 0%'), 'unknown total must not become a fabricated zero');
  const incoming = check({ total_records: 0, checked_records: 0, complete: true });
  const outgoing = check({ checked_records: 1000, complete: true, unavailable: 2 });
  const missing = status([{ ...peer({ ...work({ incoming, outgoing, pending_blobs: 0 }), synchronizing: false, rounds: 1 }), state: 'Waiting for content' }]);
  output.update(missing); tick(1000);
  assert.match(lines.at(-1), /Receiving check #1: 100%.*0\/0 records checked; 0 remaining/);
  assert.match(lines.at(-1), /Sending \(peer confirmed\) check #1: 100%.*2 content request\(s\) still unavailable/);
  assert.equal(sharingLabel(missing), 'Sharing · 1/1 connected · waiting for content');
  output.update(status([peer({ ...work({ incoming: check({ pass: 2, total_records: 1250, checked_records: 0 }) }), rounds: 1 })]));
  tick(1000);
  assert.match(lines.at(-1), /Receiving check #2: 0%.*0\/1,250 records checked; 1,250 remaining/);
  assert.match(lines.at(-1), /Received here: 128 records, 5 content objects/, 'a new check does not erase cumulative receipts');
  output.update(status([{ fingerprint, state: 'Waiting to reconnect' }])); tick(1000);
  output.update(status([peer({ ...work({ incoming: unknown, outgoing: unknown, pending_blobs: 0 }), records: 0, blobs: 0 })])); tick(1000);
  assert.match(lines.at(-1), /Receiving: waiting for the shared-history total/, 'reconnect does not retain a stale percentage');
});

test('work validation rejects contradictory counts and percentages never round incomplete work to 100', () => {
  const { validWorkProgress, checkPercent } = require('../../out/multiplayer/progress');
  assert.equal(validWorkProgress(work()), true);
  for (const incoming of [check({ checked_records: 1001 }), check({ checked_records: -1 }), check({ total_records: NaN }),
    check({ total_records: null, complete: true }), check({ complete: true }), check({ total_records: Number.MAX_SAFE_INTEGER + 1 })]) {
    assert.equal(validWorkProgress(work({ incoming })), false);
  }
  assert.equal(validWorkProgress(work({ download: { content: true, received_bytes: 2, total_bytes: 1 } })), false);
  assert.equal(validWorkProgress(work({ pending_blobs: -1 })), false);
  assert.equal(checkPercent(check({ total_records: 10000, checked_records: 9999 })), '99.99%');
  assert.equal(checkPercent(check({ total_records: 2_500_000, checked_records: 512 })), '0.02%');
  assert.equal(checkPercent(check({ total_records: Number.MAX_SAFE_INTEGER, checked_records: Number.MAX_SAFE_INTEGER - 1 })), '99.99%');
});

test('completed checks stay quiet across unchanged passes', t => {
  const complete = check({ checked_records: 1000, complete: true });
  const { lines, output, tick } = observe(t, status([{ ...peer({ ...work({ incoming: complete, outgoing: complete, pending_blobs: 0 }), synchronizing: false, rounds: 1 }), state: 'Live' }]));
  lines.length = 0;
  for (let pass = 2; pass < 10; pass++) {
    output.update(status([{ ...peer({ ...work({ incoming: { ...complete, pass }, outgoing: { ...complete, pass }, pending_blobs: 0 }), synchronizing: false, rounds: pass }), state: 'Live' }]));
    tick(1000);
  }
  assert.equal(lines.length, 0, 'completed pass numbers alone must not fill the output');
});
