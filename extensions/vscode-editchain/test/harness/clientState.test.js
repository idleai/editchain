'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { connectionState, joinState } = require('../../out/multiplayer/clientState');

test('the shipped Rust join model rejects an approval after stop and resume', () => {
  const state = joinState();
  const old = state.generation;
  state.retire();
  assert.equal(state.enable(state.generation), true);
  assert.equal(state.enable(old), false);
  assert.equal(state.is_current(old), false);
  assert.equal(state.enabled, true);
});

test('the shipped Rust connection model waits for reconciliation and late content', () => {
  const state = connectionState();
  const token = state.begin();
  const progress = { accepted: true, synchronizing: false, rounds: 0, unavailable: 0 };
  state.progress(token, JSON.stringify(progress));
  assert.equal(state.status, 'Catching up');
  state.progress(token, JSON.stringify({ ...progress, rounds: 1, unavailable: 1 }));
  assert.equal(state.status, 'Waiting for content');
  state.progress(token, JSON.stringify({ ...progress, rounds: 1 }));
  assert.equal(state.status, 'Live');
  state.waiting(token);
  state.waiting(token);
  assert.equal(state.retry_delay_ms, 1000, 'duplicate failures do not multiply retries');
  const current = state.begin();
  state.ready(token);
  assert.equal(state.status, 'Connecting', 'a late result cannot complete a new attempt');
  state.ready(current);
  assert.equal(state.status, 'Live');
  state.stop();
  state.waiting(current);
  assert.equal(state.status, 'Stopped', 'closed connections cannot schedule recovery');
});
